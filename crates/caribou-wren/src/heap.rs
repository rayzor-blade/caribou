//! The memory slots: wren_lift's C-shaped heap entry points over
//! `caribou::heap`.
//!
//! A wren_lift object begins at the address `alloc_raw` returns, so the core's
//! descriptor word cannot be its word zero. Every allocation is `PREFIX` bytes
//! longer than asked, and wren_lift is handed the address after the prefix:
//! word zero of the core allocation is the descriptor its heap record carries,
//! which is how the record is found from the object, and word one is the
//! bridge word: the object another language keeps standing for this one (the
//! protocol's shadow), and three flag bits. Every slot that takes or yields an
//! address translates.
//!
//! A wren_lift object dies only in a wren_lift cycle; a core collection on
//! its own must retain every one of them, since wren_lift's roots are not the
//! core's. So each record keeps the core starts of its allocations (its
//! pins), and one core object, the anchor, rooted by a handle, whose trace
//! marks every pin. A cycle is the two collectors in turn. wren_lift marks
//! from its own roots, in a bit of each object's bridge word; the objects a
//! core handle reaches are marked too, with everything reachable from them,
//! since a handle is how an embedder holds a Wren object. The marked pins
//! become the core's claims: live for the collection that follows, which the
//! anchor sits out. An unmarked object with a shadow (another language's
//! stand-in, see the bridge word) is neither claimed nor dropped, nor is
//! anything it reaches: whether its shadow is alive is the core's to say. The
//! core's collection marks from its roots, and a live shadow's trace marks the
//! object it stands for, which the core traces through wren_lift's own
//! visitor. Between the core's mark and its sweep, the record drops and
//! forgets the pending objects the mark did not reach. Every other unmarked
//! object is dead at the end of wren_lift's marking, dropped and forgotten
//! there. The descriptor has a trace hook and no drop hook.
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
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use caribou::cell;
use caribou::heap::{self, Handle, ImmixAllocator, TraceFn, Tracer, TypeDesc};
use caribou::protocol::CallSite;
use caribou_abi::hl::{self, hl_type, hl_type_detail};
use caribou_abi::mem;
use wren_lift::runtime::rt::{RtStats, Visit, wlift_rt_object_drop, wlift_rt_object_trace};
use wren_lift::runtime::vm::VM;

use crate::import::{self, Imports};
use crate::publish::Exports;

/// Bytes before the wren_lift object: the descriptor word and the bridge
/// word, which keep the allocation's 16-byte alignment.
pub(crate) const PREFIX: usize = 16;
/// In the bridge word: wren_lift has marked the object in the open cycle.
/// Cleared by `collect_end`.
const MARKED: usize = 1;
/// The object owns nothing outside the heap: the sweep skips `object_drop`.
const PLAIN: usize = 2;
/// An instance of an installed class holding another language's object in
/// its first field, which the trace marks.
const ADOPTED: usize = 4;
/// Left to the core's collection to decide, in the cycle that is closing.
const PENDING: usize = 8;
/// A shadow is a core object, 16-aligned, so the flags fit under it.
const FLAGS: usize = MARKED | PLAIN | ADOPTED | PENDING;
/// In a cell's bridge word, which holds no shadow: the cell is in the
/// record's `views`, retained by the anchor like a pin.
const VIEW_HELD: usize = 16;

/// The bridge word of the core allocation at `start`. A cycle writes its
/// flags plainly, under the GC lock it holds throughout; a shadow is kept
/// under the same lock and dropped by a hook of the sweep, so nothing
/// races a plain write. The other flags are set by atomic read-modify-
/// writes, since a shadow may be kept meanwhile.
#[inline(always)]
fn bridge_word(start: *mut u8) -> *mut usize {
    (start as *mut usize).wrapping_add(1)
}

#[inline(always)]
fn bridge_atom<'a>(start: *mut u8) -> &'a AtomicUsize {
    unsafe { AtomicUsize::from_ptr(bridge_word(start)) }
}

/// The record the core allocation at `start` belongs to: its descriptor
/// is the record's first field.
#[inline(always)]
unsafe fn record_of<'a>(start: *mut u8) -> &'a WrenHeap {
    unsafe { &**(start as *const *const WrenHeap) }
}

/// One wren_lift heap: the handle `heap_new` mints. Touched only under the
/// GC lock, which the trace hooks run under too, except the atomics, which
/// `should_collect` reads without it. Never freed: a value that outlives
/// its VM still reads its descriptor, and is refused by the VM check.
#[repr(C)]
pub struct WrenHeap {
    /// Word zero of every object of this heap. First, so the object's
    /// descriptor address is the record's.
    desc: TypeDesc,
    /// Core starts of every allocation wren_lift has not reclaimed, and
    /// the cells Wren holds through their views, by the thread that made
    /// or took them.
    shards: Shards,
    /// What the claimed adopted instances hold, for the anchor to mark in
    /// the collection a cycle ends with: a claimed object is not traced.
    held: Vec<*const u8>,
    /// The thread the heap was minted on, which its VM runs on.
    thread: u64,
    /// The VM entered on that thread for this heap (`proto::enter_vm`),
    /// null when none is: what a direct send runs on without a lookup.
    entered: AtomicPtr<VM>,
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
    /// clock on every 1024th, and `heartbeat` stands once it is due.
    polls: AtomicU64,
    heartbeat: AtomicBool,
    /// When the last cycle closed.
    last_cycle: Instant,
    /// wren_lift has dropped every object and `heap_drop` is under way: the
    /// pins hold dangling containers, and a trace must not walk them.
    closing: AtomicBool,
    /// `collect_end` has claimed every pin it kept in the core's side table
    /// and is collecting: the anchor has nothing to add.
    claimed: bool,
    live_bytes: usize,
    allocated_bytes: AtomicUsize,
    freed_bytes: usize,
    freed_objects: usize,
    /// How many cycles this record has run.
    cycles: usize,
    /// The other languages' classes installed in this heap's VM, and the
    /// handles its instances of them hold.
    imports: RefCell<Imports>,
    /// The VM's symbols for the signatures the bridge asks for, by the core
    /// symbol and shape asked (`proto::Signatures`).
    signatures: RefCell<crate::proto::Signatures>,
    /// A site per signature the protocol sends of its own accord, for
    /// `bridge::enter` to keep whether such a send has called back.
    fixed_sites: [CallSite; crate::proto::Fixed::COUNT],
    /// This heap's classes published to the registry.
    exports: RefCell<Exports>,
}

/// One allocation of a record: its core start, and the bytes the core
/// reserved for it, so a cycle needs no lookup to account for it.
#[derive(Clone, Copy)]
struct Pin {
    start: usize,
    size: u32,
}

/// What one thread allocates on a record and holds through views: the
/// pins, and the cells Wren holds, no pins, so the anchor retains them by
/// this list outside a cycle and `collect_end` claims the ones the cycle
/// marked and lets the rest go. Pushed by that thread alone; a cycle and
/// the anchor's trace read every shard while every other thread is at
/// rest, so neither takes a lock.
#[derive(Default)]
struct Shard {
    pins: Vec<Pin>,
    views: Vec<usize>,
}

/// A record's shards, one per thread that has allocated on it, and each
/// thread's way to its own: a cache by record, since a record is never
/// freed and its address never reused.
#[derive(Default)]
struct Shards(Mutex<Vec<*mut Shard>>);

unsafe impl Send for Shards {}
unsafe impl Sync for Shards {}

thread_local! {
    static MY_SHARDS: RefCell<Vec<(usize, *mut Shard)>> = const { RefCell::new(Vec::new()) };
}

impl Shards {
    /// The calling thread's shard on the record at `rec`, made on first
    /// need. Written through by its thread alone, and read by a cycle
    /// or a trace while the thread is at rest, so a write through it
    /// is sound on the thread that asked.
    #[inline]
    fn mine(&self, rec: usize) -> *mut Shard {
        MY_SHARDS.with(|mine| {
            let found = mine
                .borrow()
                .iter()
                .find(|(r, _)| *r == rec)
                .map(|&(_, s)| s);
            found.unwrap_or_else(|| {
                let fresh = Box::into_raw(Box::new(Shard::default()));
                self.0.lock().unwrap_or_else(|e| e.into_inner()).push(fresh);
                mine.borrow_mut().push((rec, fresh));
                fresh
            })
        })
    }

    /// Every shard, for a cycle or a trace with every thread at rest.
    fn all(&self) -> Vec<*mut Shard> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl WrenHeap {
    /// The VM entered for this heap, when the caller is on its thread.
    #[inline(always)]
    pub(crate) fn entered_here(&self) -> *mut VM {
        if self.thread != heap::thread_token() {
            return ptr::null_mut();
        }
        self.entered.load(Ordering::Relaxed)
    }

    pub(crate) fn set_entered(&self, vm: *mut VM) {
        self.entered.store(vm, Ordering::Relaxed);
    }

    pub(crate) fn signatures(&self) -> &RefCell<crate::proto::Signatures> {
        &self.signatures
    }

    pub(crate) fn fixed_site(&self, which: crate::proto::Fixed) -> &CallSite {
        &self.fixed_sites[which as usize]
    }

    pub(crate) fn imports(&self) -> &RefCell<Imports> {
        &self.imports
    }

    /// Cycles run and objects they freed, for a report.
    pub(crate) fn cycle_counts(&self) -> (usize, usize) {
        (self.cycles, self.freed_objects)
    }

    pub(crate) fn exports(&self) -> &RefCell<Exports> {
        &self.exports
    }
}

/// The record at `address`, which a call site keys a VM by. Records are
/// never freed, so any key a site ever held still names one.
#[inline(always)]
pub(crate) unsafe fn record_at<'a>(address: usize) -> &'a WrenHeap {
    unsafe { &*(address as *const WrenHeap) }
}

/// The record of the heap holding the wren_lift object at `obj`.
pub(crate) fn record_for<'a>(obj: *mut u8) -> &'a WrenHeap {
    unsafe { record_of(obj.wrapping_sub(PREFIX)) }
}

/// The address of the record an object of the core start `start` belongs
/// to, without reading the record: what to compare when the VM may be
/// gone.
#[inline(always)]
pub(crate) fn record_address(start: *mut u8) -> usize {
    unsafe { *(start as *const usize) }
}

/// Whether the core start `start` is a wren_lift object's: its descriptor
/// is a record's.
#[inline(always)]
pub(crate) fn is_wren(start: *mut u8) -> bool {
    let desc = unsafe { caribou::protocol::desc_of(start) };
    !desc.is_null() && ptr::eq(unsafe { (*desc).protocol }, &crate::proto::WREN_PROTO)
}

/// Mark the object at `obj` as adopted: an instance of an installed class
/// holding another language's object in its first field.
pub(crate) fn set_adopted(obj: *mut u8) {
    bridge_atom(obj.wrapping_sub(PREFIX)).fetch_or(ADOPTED, Ordering::Relaxed);
}

#[inline(always)]
pub(crate) fn is_adopted(obj: *mut u8) -> bool {
    (unsafe { *bridge_word(obj.wrapping_sub(PREFIX)) }) & ADOPTED != 0
}

/// The object of language `lang` kept on the object at `obj`, if any.
pub(crate) fn shadow_of(obj: *mut u8, lang: u32) -> Option<*mut u8> {
    let p = (bridge_atom(obj.wrapping_sub(PREFIX)).load(Ordering::Acquire) & !FLAGS) as *mut u8;
    (!p.is_null() && unsafe { lang_of(p) } == lang).then_some(p)
}

/// Keep `shadow` on the object at `obj`: `Ok` when kept, `Err(Some(p))`
/// when `p` of the same language already is, `Err(None)` when the word
/// holds another language's. From another thread than the VM's, under
/// the GC lock, apart from any cycle; the VM's own thread runs no cycle
/// meanwhile.
pub(crate) fn keep_shadow(obj: *mut u8, shadow: *mut u8) -> Result<(), Option<*mut u8>> {
    let start = obj.wrapping_sub(PREFIX);
    let _gc = (unsafe { record_of(start) }.thread != heap::thread_token()).then(heap::gc_guard);
    let atom = bridge_atom(start);
    let mut w = atom.load(Ordering::Acquire);
    loop {
        let p = (w & !FLAGS) as *mut u8;
        if !p.is_null() {
            return Err((unsafe { lang_of(p) } == unsafe { lang_of(shadow) }).then_some(p));
        }
        match atom.compare_exchange_weak(
            w,
            w | shadow as usize,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Ok(()),
            Err(seen) => w = seen,
        }
    }
}

/// Forget `shadow` on the object at `obj`, if it is the one kept. From
/// the shadow's drop hook, so under the GC lock.
pub(crate) fn drop_shadow(obj: *mut u8, shadow: *mut u8) {
    let atom = bridge_atom(obj.wrapping_sub(PREFIX));
    let mut w = atom.load(Ordering::Acquire);
    while w & !FLAGS == shadow as usize {
        match atom.compare_exchange_weak(w, w & FLAGS, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(seen) => w = seen,
        }
    }
}

unsafe fn lang_of(obj: *mut u8) -> u32 {
    unsafe { (*caribou::protocol::desc_of(obj)).lang }
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

/// What every record's descriptor is made from. Mutable for one field:
/// `lang` is the id the world assigns, written by `set_wren_lang` before the
/// first VM exists and copied into each record from then on.
static mut WREN_DESC: TypeDesc = {
    let mut d = desc("wren object", trace_object);
    d.protocol = &crate::proto::WREN_PROTO;
    d
};
/// Word zero of a record's anchor.
static ANCHOR_DESC: TypeDesc = desc("wren heap", trace_anchor);

/// The language id of Wren objects, as the world assigned it.
pub(crate) fn wren_lang() -> u32 {
    unsafe { WREN_DESC.lang }
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
/// wren_lift's own visitor, and what an adopted instance holds.
unsafe extern "C" fn trace_object(obj: *mut u8, tracer: *mut Tracer<'_>) {
    let rec = unsafe { record_of(obj) };
    if rec.closing.load(Ordering::Relaxed) {
        return;
    }
    let object = unsafe { obj.add(PREFIX) };
    let word = bridge_word(obj);
    let w = unsafe { *word };
    // Reached in the collection a cycle ends with: the object lives.
    if w & PENDING != 0 {
        unsafe { *word = w & !PENDING };
    }
    if w & ADOPTED != 0 {
        unsafe { (*tracer).mark(import::held(object)) };
    }
    unsafe { wlift_rt_object_trace()(object, mark_child, tracer as *mut c_void) };
}

unsafe extern "C" fn mark_child(child: *mut u8, ctx: *mut c_void) {
    unsafe { (*(ctx as *mut Tracer<'_>)).mark(child.wrapping_sub(PREFIX)) };
}

/// The anchor's trace: every pin of its record, or, when `collect_end` has
/// claimed the live ones itself, what the claimed adopted instances hold.
/// The anchor's word one is its record.
unsafe extern "C" fn trace_anchor(obj: *mut u8, tracer: *mut Tracer<'_>) {
    let rec = unsafe { &*(*bridge_word(obj) as *const WrenHeap) };
    let tracer = unsafe { &mut *tracer };
    if rec.claimed {
        for &held in &rec.held {
            tracer.mark(held);
        }
        return;
    }
    for shard in rec.shards.all() {
        let shard = unsafe { &*shard };
        for pin in &shard.pins {
            tracer.mark(pin.start as *const u8);
        }
        for &view in &shard.views {
            tracer.mark(view as *const u8);
        }
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
    let ours = unsafe { *(start as *const usize) } == rec as *const WrenHeap as usize;
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
        desc: unsafe { ptr::read(&raw const WREN_DESC) },
        shards: Shards::default(),
        held: Vec::new(),
        thread: heap::thread_token(),
        entered: AtomicPtr::new(ptr::null_mut()),
        anchor: Handle::NULL,
        bytes_since_cycle: AtomicUsize::new(0),
        trigger: AtomicUsize::new(gc.trigger_threshold()),
        polls: AtomicU64::new(0),
        heartbeat: AtomicBool::new(false),
        last_cycle: Instant::now(),
        closing: AtomicBool::new(false),
        claimed: false,
        live_bytes: 0,
        allocated_bytes: AtomicUsize::new(0),
        freed_bytes: 0,
        freed_objects: 0,
        cycles: 0,
        imports: RefCell::new(Imports::default()),
        signatures: RefCell::new(crate::proto::Signatures::default()),
        fixed_sites: [const { CallSite::new() }; crate::proto::Fixed::COUNT],
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
    unsafe { bridge_word(anchor).write(rec as usize) };
    unsafe { (*rec).anchor = gc.handle_new(anchor) };
    rec as *mut c_void
}

/// wren_lift has dropped every object; their memory returns with the next
/// core sweep. `closing` is set before the lock, whose acquisition can park
/// this thread in a collection that would otherwise trace the dropped
/// objects.
pub unsafe extern "C" fn heap_drop(heap: *mut c_void) {
    // The record stays: see `WrenHeap`.
    let rec = unsafe { record_mut(heap) };
    rec.closing.store(true, Ordering::Relaxed);
    let mut gc = heap::gc_locked_init();
    import::forget_classes(rec);
    for shard in rec.shards.all() {
        for pin in &unsafe { &*shard }.pins {
            gc.forget_allocation(pin.start as *const u8);
        }
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
    // A fresh traced object is unmarked, and a collection before it is
    // pinned would forget it. None can come between: the allocator
    // collects, or parks for another mutator's collection, before it bumps,
    // and this thread reaches no safepoint until the pin is pushed.
    let reserved = (size + PREFIX).next_multiple_of(16);
    let rec = unsafe { record_mut(heap) };
    let p = unsafe {
        heap::alloc_gen(
            desc_ptr(&rec.desc),
            size + PREFIX,
            mem::KIND_DYNAMIC | mem::TRACED,
        )
    } as *mut u8;
    if p.is_null() {
        return p;
    }
    let start = p as usize;
    unsafe { bridge_word(p).write(flags) };
    let pin = Pin {
        start,
        size: reserved.min(u32::MAX as usize) as u32,
    };
    unsafe { (*rec.shards.mine(heap as usize)).pins.push(pin) };
    rec.allocated_bytes.fetch_add(reserved, Ordering::Relaxed);
    rec.bytes_since_cycle.fetch_add(reserved, Ordering::Relaxed);
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
/// A cell Wren holds through its view is marked in the same word.
pub unsafe extern "C" fn mark_allocation(_heap: *mut c_void, ptr: *mut u8) -> bool {
    let word = bridge_word(ptr.wrapping_sub(PREFIX));
    let w = unsafe { *word };
    if w & MARKED != 0 {
        return false;
    }
    unsafe { *word = w | MARKED };
    true
}

/// Wren holds the cell at `start` through its view from now on: the
/// anchor retains it until a cycle finds Wren no longer does. What Wren
/// holds this way is pressure its cycle answers, counted as the cell.
pub(crate) fn hold_view(heap: &WrenHeap, start: *mut u8) {
    let atom = bridge_atom(start);
    if atom.fetch_or(VIEW_HELD, Ordering::AcqRel) & VIEW_HELD == 0 {
        let shard = heap.shards.mine(heap as *const WrenHeap as usize);
        unsafe { (*shard).views.push(start as usize) };
        heap.bytes_since_cycle
            .fetch_add(size_of::<cell::Cell>(), Ordering::Relaxed);
    }
}

/// The core start of the viewed cell `addr` is in, if it is in one:
/// what a scan of a native range finds Wren holding beside its own
/// objects.
unsafe fn viewed_cell(gc: &ImmixAllocator, addr: usize) -> Option<(usize, usize)> {
    let (start, size) = gc.allocation_containing(addr)?;
    // Only a traced allocation has a descriptor at word zero to read.
    let held = heap::is_traced_allocation(start as *const c_void)
        && unsafe { cell::is_cell(start as *const u8) }
        && unsafe { *bridge_word(start as *mut u8) } & VIEW_HELD != 0;
    held.then_some((start, size))
}

pub unsafe extern "C" fn is_marked(_heap: *mut c_void, ptr: *mut u8) -> bool {
    unsafe { *bridge_word(ptr.wrapping_sub(PREFIX)) & MARKED != 0 }
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
        let mut found = unsafe { resolve(&gc, rec, w).or_else(|| viewed_cell(&gc, w)) };
        #[cfg(target_pointer_width = "64")]
        {
            const TAG_OBJ: usize = 0xFFFC_0000_0000_0000;
            const PAYLOAD: usize = 0x0000_FFFF_FFFF_FFFF;
            if found.is_none() && w & TAG_OBJ == TAG_OBJ {
                let addr = w & PAYLOAD;
                found = unsafe { resolve(&gc, rec, addr).or_else(|| viewed_cell(&gc, addr)) };
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

// A krio stack is a root the core scans from where it is suspended, as
// it scans its own tasks' stacks; krio's ids are the core's registry
// keys, so nothing is renumbered.
pub unsafe extern "C" fn stack_new(id: u64, base: usize, size: usize) {
    unsafe { heap::gc_register_fiber_stack(id, base, size) };
}

pub unsafe extern "C" fn stack_suspended(id: u64, sp: usize) {
    unsafe { heap::gc_update_fiber_sp(id, sp) };
}

pub unsafe extern "C" fn stack_drop(id: u64) {
    unsafe { heap::gc_unregister_fiber_stack(id) };
    caribou::sched::forget_stack(id);
}

/// A switch of stacks inside a Wren run: the state the adapters keep per
/// stack goes with it, Ash's trap chain above all.
pub unsafe extern "C" fn stack_switch(from: u64, to: u64) {
    caribou::sched::switch_stack(from, to);
}

// A thread that runs Wren is a core mutator in deferred mode for as long
// as it does, as a thread that mints a heap is; a thread safe in
// wren_lift's world is in a blocking region of the core's, which the
// collector does not wait for and scans where it stands, and one running
// again is held while a collection is under way.
pub unsafe extern "C" fn thread_start() {
    enter_thread();
}

pub unsafe extern "C" fn thread_stop() {
    leave_thread();
}

pub unsafe extern "C" fn thread_safe(sp: usize, extra_lo: usize, extra_hi: usize) {
    heap::gc_block_at(sp, (extra_lo, extra_hi));
}

pub unsafe extern "C" fn thread_running() {
    heap::gc_unblock();
}

/// The core's stop, to every thread running Wren: wren_lift holds its
/// pages unreadable, so a compiled loop faults and passes through safe
/// and running, where the core holds it.
pub(crate) fn host_stop(on: bool) {
    unsafe { wren_lift::runtime::rt::wlift_rt_host_stop()(on) };
}

/// Have `object_drop` run for the plain allocation at `ptr` once a cycle
/// finds it dead, as for a raw one: its plain bit is cleared, which is
/// all the sweep consults. False when `ptr` is not a plain allocation of
/// this heap.
pub unsafe extern "C" fn watch(heap: *mut c_void, ptr: *mut u8) -> bool {
    let rec = unsafe { record(heap) };
    let start = ptr.wrapping_sub(PREFIX);
    if !owns_start(rec, start as usize) {
        return false;
    }
    bridge_atom(start).fetch_and(!PLAIN, Ordering::Relaxed) & PLAIN != 0
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
    // The heartbeat: the clock is read on every 1024th poll, and a beat
    // stands until the cycle it asks for, since the answer is asked for
    // more than once on the way to one.
    if rec.heartbeat.load(Ordering::Relaxed) {
        return true;
    }
    let polls = rec.polls.load(Ordering::Relaxed).wrapping_add(1);
    rec.polls.store(polls, Ordering::Relaxed);
    let due =
        polls & 1023 == 0 && since > 0 && rec.last_cycle.elapsed() >= heap::heartbeat_interval();
    if due {
        rec.heartbeat.store(true, Ordering::Relaxed);
    }
    due
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

/// Flag every unmarked object as pending: what wren_lift's marking did
/// not reach is the core's collection's to decide, from every stack,
/// cell and handle. True when any is.
fn flag_pending(rec: &WrenHeap) -> bool {
    let mut any = false;
    for shard in rec.shards.all() {
        for pin in &unsafe { &*shard }.pins {
            let word = bridge_word(pin.start as *mut u8);
            let w = unsafe { *word };
            if w & MARKED == 0 {
                unsafe { *word = w | PENDING };
                any = true;
            }
        }
    }
    any
}

pub unsafe extern "C" fn collect_end(heap: *mut c_void) -> usize {
    let drop_object = wlift_rt_object_drop();
    let mut gc = heap::gc_locked();
    let rec = unsafe { record_mut(heap) };
    let pending = flag_pending(rec);
    let mut live = 0usize;
    let mut freed = 0usize;
    let mut dead = 0usize;
    // A dead object: wren_lift drops what it owns unless it is plain, and
    // its start is forgotten.
    let mut die = |gc: &mut ImmixAllocator, pin: &Pin, w: usize| {
        if w & ADOPTED != 0 {
            import::forget_front((pin.start + PREFIX) as *mut u8);
        }
        if w & PLAIN == 0 {
            unsafe { drop_object((pin.start + PREFIX) as *mut u8) };
        }
        gc.forget_allocation(pin.start as *const u8);
        freed += pin.size as usize;
        dead += 1;
    };
    let shards = rec.shards.all();
    // A viewed cell the cycle marked is the anchor's to mark, not a
    // claim: a claim is marked and not traced, and the cell holds what is
    // no pin, its object of another language, which only its trace keeps.
    // One the cycle did not reach leaves the list, for the core to decide.
    let held = &mut rec.held;
    for &shard in &shards {
        unsafe { &mut *shard }.views.retain(|&start| {
            let word = bridge_word(start as *mut u8);
            let w = unsafe { *word };
            if w & MARKED != 0 {
                unsafe { *word = w & !MARKED };
                held.push(start as *const u8);
                return true;
            }
            unsafe { *word = w & !VIEW_HELD };
            false
        });
    }
    // Marked pins become the core's claims, in address order, and what a
    // marked adopted instance holds is for the anchor to mark; the rest
    // are pending, for the core to decide.
    for &shard in &shards {
        for pin in &unsafe { &*shard }.pins {
            let word = bridge_word(pin.start as *mut u8);
            let w = unsafe { *word };
            if w & MARKED != 0 {
                unsafe { *word = w & !MARKED };
                live += gc
                    .claim_start(pin.start as *const u8)
                    .expect("a pin is an allocation start");
                if w & ADOPTED != 0 {
                    held.push(unsafe { import::held((pin.start + PREFIX) as *mut u8) });
                }
            }
        }
    }
    // The core's collection is the second half of the cycle. It retains
    // the claims, and its trace clears the pending flag of every pending
    // object it reaches; between its mark and its sweep the rest die. A
    // collection the core abandoned leaves the claims standing, and a
    // claim made outside a collection is withdrawn.
    let before = heap::collections();
    rec.claimed = true;
    gc.collect_garbage_then(|gc| {
        if !pending {
            return;
        }
        for &shard in &shards {
            unsafe { &mut *shard }.pins.retain(|pin| {
                let w = unsafe { *bridge_word(pin.start as *mut u8) };
                if w & PENDING == 0 {
                    return true;
                }
                die(gc, pin, w);
                false
            });
        }
    });
    rec.claimed = false;
    rec.held.clear();
    if heap::collections() == before {
        for &shard in &shards {
            for pin in &unsafe { &*shard }.pins {
                let word = bridge_word(pin.start as *mut u8);
                unsafe { *word &= !PENDING };
                gc.unclaim(pin.start as *const u8);
            }
        }
    }
    rec.freed_objects += dead;
    rec.cycles += 1;
    rec.trigger.store(gc.trigger_threshold(), Ordering::Relaxed);
    rec.live_bytes = live;
    rec.freed_bytes += freed;
    rec.bytes_since_cycle.store(0, Ordering::Relaxed);
    rec.polls.store(0, Ordering::Relaxed);
    rec.heartbeat.store(false, Ordering::Relaxed);
    rec.last_cycle = Instant::now();
    drop(gc);
    unsafe { heap::unlock() };
    live
}

pub unsafe extern "C" fn for_each_allocation(heap: *mut c_void, visit: Visit, ctx: *mut c_void) {
    let rec = unsafe { record(heap) };
    let _gc = heap::gc_locked_init();
    // `visit` may not allocate, so no shard grows under the walk.
    for shard in rec.shards.all() {
        for pin in unsafe { &(*shard).pins } {
            unsafe { visit((pin.start + PREFIX) as *mut u8, ctx) };
        }
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
            allocated_bytes: rec.allocated_bytes.load(Ordering::Relaxed),
            freed_bytes: rec.freed_bytes,
            freed_objects: rec.freed_objects,
        })
    };
}
