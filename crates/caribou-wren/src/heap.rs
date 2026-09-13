//! The memory slots: wren_lift's C-shaped heap entry points over
//! `caribou::heap`.
//!
//! A wren_lift object begins at the address `alloc_raw` returns, so the core's
//! descriptor word cannot be its word zero. Every allocation is `PREFIX` bytes
//! longer than asked, and wren_lift is handed the address after the prefix:
//! word zero of the core allocation is [`WREN_DESC`], word one the heap record
//! it belongs to. Every slot that takes or yields an address translates.
//!
//! wren_lift's cycle is the only reclaimer of wren_lift objects; a core
//! collection must retain every one of them, and no root of the core's reaches
//! them. So each record keeps the core starts of its allocations (its pins),
//! and one core object, the anchor, rooted by a handle, whose trace marks every
//! pin. A wren_lift cycle marks in a bit of each object's record word; at its
//! end the objects the core's handles reach are marked too, with everything
//! reachable from them, since a handle is how another language holds a Wren
//! object and wren_lift's roots do not include it. Then the marked pins
//! become the core's per-cycle claims, in address order, the rest are dropped
//! and forgotten, and a core collection, which the anchor sits out, sweeps: it
//! clears the claims and returns the lines. `WREN_DESC` has a trace hook and
//! no drop hook: the core traces wren_lift objects precisely wherever it
//! reaches them and never runs their drop.
//!
//! The thread a heap is minted on is an ordinary core mutator, in deferred
//! mode. A collection another mutator starts waits for it to park, which it
//! does in `should_collect`, where a slot takes the GC lock (`alloc_raw`
//! first among them), and in `collect_begin`. The core's trigger, firing
//! inside `alloc_raw`, records `collect_pending` instead of collecting
//! there; `should_collect` reports it, and wren_lift's own cycle runs on it.
//! The record keeps a trigger of its own beside the shared one, so another
//! mutator's collections cannot starve its cycle. The invariant that makes
//! the other thread's precise trace sound: wren_lift completes every write
//! to an object between two of its polls, and this thread parks only at one.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use caribou::heap::{self, Handle, ImmixAllocator, TraceFn, Tracer, TypeDesc};
use caribou_abi::hl::{self, hl_type, hl_type_detail};
use caribou_abi::mem;
use wren_lift::runtime::rt::{RtStats, Visit, wlift_rt_object_drop, wlift_rt_object_trace};

use crate::import::{self, Imports};
use crate::publish::Exports;

/// Bytes before the wren_lift object: the descriptor word and the record
/// word, padded so the object keeps the allocation's 16-byte alignment.
pub(crate) const PREFIX: usize = 16;
/// In the record word: wren_lift has marked the object in the open cycle.
/// The record is a `Box`, so the bit is free. Cleared by `collect_end`.
const MARKED: usize = 1;
/// The object owns nothing outside the heap: the sweep skips `object_drop`.
const PLAIN: usize = 2;
const FLAGS: usize = MARKED | PLAIN;

/// The record word of the core allocation at `start`.
#[inline(always)]
fn record_word(start: *mut u8) -> *mut usize {
    (start as *mut usize).wrapping_add(1)
}

/// The record the core allocation at `start` belongs to.
#[inline(always)]
unsafe fn record_of<'a>(start: *mut u8) -> &'a WrenHeap {
    unsafe { &*((*record_word(start) & !FLAGS) as *const WrenHeap) }
}

/// One wren_lift heap: the handle `heap_new` mints. Touched only under the
/// GC lock, which the trace hooks run under too, except the atomics, which
/// `should_collect` reads without it.
pub struct WrenHeap {
    /// Core starts of every allocation wren_lift has not reclaimed.
    pins: Vec<usize>,
    /// Roots the anchor, whose word one points back at this record.
    anchor: Handle,
    /// Bytes handed out since the last cycle, counted as the core counts
    /// its trigger pressure; what the stress switch, this record's own
    /// trigger and the heartbeat poll.
    bytes_since_cycle: AtomicUsize,
    /// The core's threshold as of this record's last cycle: a cycle is due
    /// when this record alone has allocated that much since, whatever
    /// another mutator's collections did to the shared trigger meanwhile.
    trigger: AtomicUsize,
    /// `should_collect` calls since the last cycle; the heartbeat reads the
    /// clock on every 1024th.
    polls: AtomicU64,
    /// When the last cycle closed.
    last_cycle: Instant,
    /// wren_lift has dropped every object and `heap_drop` is under way: the
    /// pins hold dangling containers, and a trace must not walk them.
    closing: AtomicBool,
    /// `collect_end` has claimed every pin it kept in the core's side table
    /// and is collecting: the anchor has nothing to add.
    claimed: bool,
    live_bytes: usize,
    allocated_bytes: usize,
    freed_bytes: usize,
    freed_objects: usize,
    /// The other languages' classes installed in this heap's VM, and the
    /// handles its instances of them hold.
    imports: RefCell<Imports>,
    /// This heap's classes published to the registry.
    exports: RefCell<Exports>,
}

impl WrenHeap {
    pub(crate) fn imports(&self) -> &RefCell<Imports> {
        &self.imports
    }

    pub(crate) fn exports(&self) -> &RefCell<Exports> {
        &self.exports
    }
}

/// The record of the heap holding the wren_lift object at `obj`.
pub(crate) fn record_for<'a>(obj: *mut u8) -> &'a WrenHeap {
    unsafe { record_of(obj.wrapping_sub(PREFIX)) }
}

/// Whether `start` is the core start of an object of `rec`'s heap.
pub(crate) fn owns_start(rec: &WrenHeap, start: usize) -> bool {
    let gc = heap::gc_locked_init();
    unsafe { resolve(&gc, rec, start) }.is_some_and(|(found, _)| found == start)
}

thread_local! {
    /// Heaps minted on this thread, and whether the first of them registered
    /// the thread; one another runtime registered stays that runtime's.
    static HEAPS_HERE: Cell<usize> = const { Cell::new(0) };
    static REGISTERED_HERE: Cell<bool> = const { Cell::new(false) };
}

/// Make this thread a core mutator in deferred mode. The stack top is the
/// OS's, so the call may come from any depth.
fn enter_thread() {
    let heaps = HEAPS_HERE.with(|c| c.replace(c.get() + 1));
    if heaps == 0 && !heap::thread_registered() {
        #[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
        heap::gc_register_current_os_thread();
        REGISTERED_HERE.with(|c| c.set(heap::thread_registered()));
    }
    heap::set_deferred_collection(true);
}

/// The last heap on this thread is gone.
fn leave_thread() {
    let heaps = HEAPS_HERE.with(|c| {
        let n = c.get().saturating_sub(1);
        c.set(n);
        n
    });
    if heaps != 0 {
        return;
    }
    heap::set_deferred_collection(false);
    if REGISTERED_HERE.with(|c| c.replace(false)) {
        heap::unregister_thread();
    }
}

const fn desc(name: &'static str, trace: TraceFn) -> TypeDesc {
    let mut d = TypeDesc::new(hl_type {
        kind: hl::HABSTRACT,
        detail: hl_type_detail {
            abs_name: ptr::null(),
        },
        vobj_proto: ptr::null_mut(),
        mark_bits: ptr::null_mut(),
    });
    d.trace = Some(trace);
    d.name = name.as_ptr();
    d.name_len = name.len();
    d
}

/// Word zero of every wren_lift object's allocation. Mutable for one field:
/// `lang` is the id the world assigns, written by `set_wren_lang` before the
/// first VM exists and read from then on.
static mut WREN_DESC: TypeDesc = {
    let mut d = desc("wren object", trace_object);
    d.protocol = &crate::proto::WREN_PROTO;
    d
};
/// Word zero of a record's anchor.
static ANCHOR_DESC: TypeDesc = desc("wren heap", trace_anchor);

/// The descriptor every wren_lift object carries.
pub(crate) fn wren_desc() -> *const TypeDesc {
    &raw const WREN_DESC
}

/// The language id of Wren objects, as the world assigned it.
pub(crate) fn wren_lang() -> u32 {
    unsafe { (*wren_desc()).lang }
}

/// Record the world's id for Wren. Before any VM allocates: a descriptor is
/// read by every collection and every message from then on.
pub(crate) fn set_wren_lang(lang: u32) {
    unsafe { WREN_DESC.lang = lang };
}

fn desc_ptr(d: *const TypeDesc) -> *mut hl_type {
    d as *mut hl_type
}

/// The core's precise trace of a wren_lift object: its children through
/// wren_lift's own visitor.
unsafe extern "C" fn trace_object(obj: *mut u8, tracer: *mut Tracer<'_>) {
    let rec = unsafe { record_of(obj) };
    if rec.closing.load(Ordering::Relaxed) {
        return;
    }
    unsafe { wlift_rt_object_trace()(obj.add(PREFIX), mark_child, tracer as *mut c_void) };
}

unsafe extern "C" fn mark_child(child: *mut u8, ctx: *mut c_void) {
    unsafe { (*(ctx as *mut Tracer<'_>)).mark(child.wrapping_sub(PREFIX)) };
}

/// The anchor's trace: every pin of its record, unless `collect_end` has
/// just claimed them all itself.
unsafe extern "C" fn trace_anchor(obj: *mut u8, tracer: *mut Tracer<'_>) {
    let rec = unsafe { record_of(obj) };
    if rec.claimed {
        return;
    }
    let tracer = unsafe { &mut *tracer };
    for &start in &rec.pins {
        tracer.mark(start as *const u8);
    }
}

/// The record behind a handle, to read. The trace hooks read the record
/// too, from inside any core collection, so no exclusive borrow may be live
/// across a call that can collect.
#[inline(always)]
unsafe fn record<'a>(heap: *mut c_void) -> &'a WrenHeap {
    unsafe { &*(heap as *const WrenHeap) }
}

#[inline(always)]
unsafe fn record_mut<'a>(heap: *mut c_void) -> &'a mut WrenHeap {
    unsafe { &mut *(heap as *mut WrenHeap) }
}

/// The core start of the record's allocation containing `addr`, with the
/// bytes the core reserved for it.
unsafe fn resolve(gc: &ImmixAllocator, rec: &WrenHeap, addr: usize) -> Option<(usize, usize)> {
    let (start, size) = gc.allocation_containing(addr)?;
    let words = start as *const usize;
    let ours = unsafe { *words == wren_desc() as usize }
        && unsafe { *words.add(1) & !FLAGS == rec as *const WrenHeap as usize };
    ours.then_some((start, size))
}

/// `WLIFT_GC_STRESS`: collect at every poll, as wren_lift's own heap does.
fn stress_enabled() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| std::env::var_os("WLIFT_GC_STRESS").is_some())
}

pub unsafe extern "C" fn heap_new() -> *mut c_void {
    enter_thread();
    // One hold: the anchor is a traced object nothing reaches until its
    // handle exists, and a collection in between would forget it.
    let mut gc = heap::gc_locked_init();
    let rec = Box::into_raw(Box::new(WrenHeap {
        pins: Vec::new(),
        anchor: Handle::NULL,
        bytes_since_cycle: AtomicUsize::new(0),
        trigger: AtomicUsize::new(gc.trigger_threshold()),
        polls: AtomicU64::new(0),
        last_cycle: Instant::now(),
        closing: AtomicBool::new(false),
        claimed: false,
        live_bytes: 0,
        allocated_bytes: 0,
        freed_bytes: 0,
        freed_objects: 0,
        imports: RefCell::new(Imports::default()),
        exports: RefCell::new(Exports::default()),
    }));
    let anchor = unsafe {
        heap::alloc_gen(
            desc_ptr(&ANCHOR_DESC),
            PREFIX,
            mem::KIND_DYNAMIC | mem::TRACED,
        )
    } as *mut u8;
    if anchor.is_null() {
        heap::out_of_memory("a wren heap's anchor");
    }
    unsafe { record_word(anchor).write(rec as usize) };
    unsafe { (*rec).anchor = gc.handle_new(anchor) };
    rec as *mut c_void
}

/// wren_lift has dropped every object; their memory returns with the next
/// core sweep. `closing` is set before the lock, whose acquisition can park
/// this thread in a collection that would otherwise trace the dropped
/// objects.
pub unsafe extern "C" fn heap_drop(heap: *mut c_void) {
    let rec = unsafe { Box::from_raw(heap as *mut WrenHeap) };
    rec.closing.store(true, Ordering::Relaxed);
    let mut gc = heap::gc_locked_init();
    import::release_all(&rec, &mut gc);
    for &start in &rec.pins {
        gc.forget_allocation(start as *const u8);
    }
    let anchor = gc.handle_get(rec.anchor);
    gc.forget_allocation(anchor);
    gc.handle_release(rec.anchor);
    drop(gc);
    leave_thread();
}

pub unsafe extern "C" fn alloc_raw(heap: *mut c_void, size: usize) -> *mut u8 {
    unsafe { alloc_with(heap, size, 0) }
}

pub unsafe extern "C" fn alloc_plain(heap: *mut c_void, size: usize) -> *mut u8 {
    unsafe { alloc_with(heap, size, PLAIN) }
}

unsafe fn alloc_with(heap: *mut c_void, size: usize, flags: usize) -> *mut u8 {
    // One hold from allocation to pin: a fresh traced object is unmarked, and
    // a collection before it is pinned would forget it.
    let gc = heap::gc_locked_init();
    let p = unsafe {
        heap::alloc_gen(
            desc_ptr(wren_desc()),
            size + PREFIX,
            mem::KIND_DYNAMIC | mem::TRACED,
        )
    } as *mut u8;
    if p.is_null() {
        return p;
    }
    let start = p as usize;
    unsafe { record_word(p).write(heap as usize | flags) };
    let reserved = gc
        .allocation_containing(start)
        .map_or(size + PREFIX, |(_, size)| size);
    let rec = unsafe { record_mut(heap) };
    rec.pins.push(start);
    rec.allocated_bytes += reserved;
    // The core charges the 16-byte-aligned size, not the lines reserved.
    rec.bytes_since_cycle
        .fetch_add((size + PREFIX).next_multiple_of(16), Ordering::Relaxed);
    unsafe { p.add(PREFIX) }
}

pub unsafe extern "C" fn containing_allocation(heap: *mut c_void, addr: usize) -> *mut u8 {
    let rec = unsafe { record(heap) };
    let gc = heap::gc_locked_init();
    match unsafe { resolve(&gc, rec, addr) } {
        Some((start, _)) => (start + PREFIX) as *mut u8,
        None => ptr::null_mut(),
    }
}

pub unsafe extern "C" fn is_heap_ptr(heap: *mut c_void, addr: usize) -> bool {
    let rec = unsafe { record(heap) };
    let gc = heap::gc_locked_init();
    unsafe { resolve(&gc, rec, addr) }.is_some()
}

/// The mark is a bit in the object's own prefix, as cheap as a header byte;
/// the core's side table hears of it in `collect_end`, in address order.
pub unsafe extern "C" fn mark_allocation(_heap: *mut c_void, ptr: *mut u8) -> bool {
    let word = record_word(ptr.wrapping_sub(PREFIX));
    let w = unsafe { *word };
    if w & MARKED != 0 {
        return false;
    }
    unsafe { *word = w | MARKED };
    true
}

pub unsafe extern "C" fn is_marked(_heap: *mut c_void, ptr: *mut u8) -> bool {
    unsafe { *record_word(ptr.wrapping_sub(PREFIX)) & MARKED != 0 }
}

/// A word is a candidate as a raw address and, when its top 14 bits are set,
/// as the 48-bit payload of a NaN-boxed object `Value`.
pub unsafe extern "C" fn scan_range(
    heap: *mut c_void,
    lo: usize,
    hi: usize,
    visit: Visit,
    ctx: *mut c_void,
) {
    const WORD: usize = std::mem::size_of::<usize>();
    let rec = unsafe { record(heap) };
    let gc = heap::gc_locked_init();
    let mut p = lo.next_multiple_of(WORD);
    while p + WORD <= hi {
        let w = unsafe { ptr::read_volatile(p as *const usize) };
        let mut found = unsafe { resolve(&gc, rec, w) };
        #[cfg(target_pointer_width = "64")]
        {
            const TAG_OBJ: usize = 0xFFFC_0000_0000_0000;
            const PAYLOAD: usize = 0x0000_FFFF_FFFF_FFFF;
            if found.is_none() && w & TAG_OBJ == TAG_OBJ {
                found = unsafe { resolve(&gc, rec, w & PAYLOAD) };
            }
        }
        if let Some((start, _)) = found {
            unsafe { visit((start + PREFIX) as *mut u8, ctx) };
        }
        p += WORD;
    }
}

pub unsafe extern "C" fn track_external(_heap: *mut c_void, bytes: usize) {
    heap::track_external(bytes as u64);
}

/// True when this record has allocated a threshold's worth since its last
/// cycle, the core's trigger is due, a trigger was deferred out of an
/// allocation, or the heartbeat has elapsed with something allocated.
///
/// A stop another mutator asked for is answered here by parking, not by a
/// cycle: a cycle stops the world in turn, and two hosted heaps would then
/// collect each other without end. Parking here is sound for the reason
/// `collect_begin` gives, and wren_lift polls only where it would accept a
/// whole cycle.
pub unsafe extern "C" fn should_collect(heap: *mut c_void) -> bool {
    if heap::stop_requested() {
        heap::gc_safepoint();
    }
    let rec = unsafe { record(heap) };
    let since = rec.bytes_since_cycle.load(Ordering::Relaxed);
    if stress_enabled() {
        return since > 0;
    }
    if since >= rec.trigger.load(Ordering::Relaxed)
        || heap::should_collect()
        || heap::collect_pending()
    {
        return true;
    }
    // Only this thread writes the counter: a plain increment, no RMW.
    let polls = rec.polls.load(Ordering::Relaxed).wrapping_add(1);
    rec.polls.store(polls, Ordering::Relaxed);
    polls & 1023 == 0 && since > 0 && rec.last_cycle.elapsed() >= heap::heartbeat_interval()
}

/// Parks first if another mutator asked for the world: that collection runs
/// before this cycle opens. Sound because this thread parks only at a
/// safepoint, and every write wren_lift makes to an object is complete
/// between two of them. Then holds the GC lock until `collect_end`: this
/// thread polls nowhere inside a cycle, so a collection another mutator
/// started meanwhile would wait on it in vain, and the marks it makes are
/// read by nothing else.
pub unsafe extern "C" fn collect_begin(_heap: *mut c_void) {
    heap::gc_safepoint();
    unsafe { heap::lock() };
}

/// Mark every object of `rec` a core handle reaches, and everything
/// reachable from it, as wren_lift's own marking would have: a handle is
/// how another language holds a Wren object that crossed out.
fn mark_held(gc: &ImmixAllocator, rec: &WrenHeap) {
    let mut gray: Vec<*mut u8> = Vec::new();
    gc.for_each_handle(|p| {
        if let Some((start, _)) = unsafe { resolve(gc, rec, p as usize) } {
            let obj = (start + PREFIX) as *mut u8;
            if unsafe { mark_allocation(ptr::null_mut(), obj) } {
                gray.push(obj);
            }
        }
    });
    if gray.is_empty() {
        return;
    }
    let trace = wlift_rt_object_trace();
    while let Some(obj) = gray.pop() {
        unsafe {
            trace(
                obj,
                mark_gray,
                &mut gray as *mut Vec<*mut u8> as *mut c_void,
            )
        };
    }
}

unsafe extern "C" fn mark_gray(child: *mut u8, ctx: *mut c_void) {
    if unsafe { mark_allocation(ptr::null_mut(), child) } {
        unsafe { (*(ctx as *mut Vec<*mut u8>)).push(child) };
    }
}

pub unsafe extern "C" fn collect_end(heap: *mut c_void) -> usize {
    let drop_object = wlift_rt_object_drop();
    let mut gc = heap::gc_locked();
    mark_held(&gc, unsafe { record(heap) });
    let mut live = 0usize;
    let mut dead = Vec::new();
    // Marked pins become the core's claims, in address order; the rest die.
    unsafe { record_mut(heap) }.pins.retain(|&start| {
        let word = record_word(start as *mut u8);
        let w = unsafe { *word };
        if w & MARKED != 0 {
            unsafe { *word = w & !MARKED };
            live += gc
                .claim_start(start as *const u8)
                .expect("a pin is an allocation start");
            true
        } else {
            let (_, size) = gc
                .allocation_containing(start)
                .expect("a pin is an allocation start");
            dead.push((start, size, w & PLAIN != 0));
            false
        }
    });
    let mut freed = 0usize;
    let rec = unsafe { record(heap) };
    let dead_count = dead.len();
    for (start, size, plain) in dead {
        let obj = (start + PREFIX) as *mut u8;
        import::finalize_dead(rec, obj, &mut gc);
        if !plain {
            unsafe { drop_object(obj) };
        }
        gc.forget_allocation(start as *const u8);
        freed += size;
    }
    unsafe { record_mut(heap) }.freed_objects += dead_count;
    // The core's collection is the sweep: it retains the pins, whose claims
    // stand, clears them, and returns the forgotten objects' lines. A
    // collection the core abandoned leaves the claims standing, and a claim
    // made outside a collection is withdrawn.
    let before = heap::collections();
    unsafe { record_mut(heap) }.claimed = true;
    gc.collect_garbage();
    let rec = unsafe { record_mut(heap) };
    rec.claimed = false;
    if heap::collections() == before {
        for &start in &rec.pins {
            gc.unclaim(start as *const u8);
        }
    }
    rec.trigger.store(gc.trigger_threshold(), Ordering::Relaxed);
    rec.live_bytes = live;
    rec.freed_bytes += freed;
    rec.bytes_since_cycle.store(0, Ordering::Relaxed);
    rec.polls.store(0, Ordering::Relaxed);
    rec.last_cycle = Instant::now();
    drop(gc);
    unsafe { heap::unlock() };
    live
}

pub unsafe extern "C" fn for_each_allocation(heap: *mut c_void, visit: Visit, ctx: *mut c_void) {
    let rec = unsafe { record(heap) };
    let _gc = heap::gc_locked_init();
    // By index: `visit` may not allocate, so the list cannot grow under it.
    for i in 0..rec.pins.len() {
        unsafe { visit((rec.pins[i] + PREFIX) as *mut u8, ctx) };
    }
}

pub unsafe extern "C" fn stats(heap: *mut c_void, out: *mut RtStats) {
    let rec = unsafe { record(heap) };
    let mut heap_bytes = 0f64;
    unsafe { heap::stats(ptr::null_mut(), ptr::null_mut(), &mut heap_bytes) };
    unsafe {
        out.write(RtStats {
            heap_bytes: heap_bytes as usize,
            live_bytes: rec.live_bytes,
            allocated_bytes: rec.allocated_bytes,
            freed_bytes: rec.freed_bytes,
            freed_objects: rec.freed_objects,
        })
    };
}
