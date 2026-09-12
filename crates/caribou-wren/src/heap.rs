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
//! pin. A wren_lift cycle claims through the core's own per-cycle claim, drops
//! and forgets what it did not claim, and ends with a core collection, whose
//! sweep clears the claims and returns the lines. `WREN_DESC` has a trace hook
//! and no drop hook: the core traces wren_lift objects precisely wherever it
//! reaches them and never runs their drop.

use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use caribou::heap::{self, Handle, ImmixAllocator, TraceFn, Tracer, TypeDesc};
use caribou_abi::hl::{self, hl_type, hl_type_detail};
use caribou_abi::mem;
use wren_lift::runtime::rt::{RtStats, Visit, wlift_rt_object_drop, wlift_rt_object_trace};

/// Bytes before the wren_lift object: the descriptor word and the record
/// word, padded so the object keeps the allocation's 16-byte alignment.
const PREFIX: usize = 16;

/// One wren_lift heap: the handle `heap_new` mints. Touched only under the
/// GC lock, which the anchor's trace hook runs under too.
pub struct WrenHeap {
    /// Core starts of every allocation wren_lift has not reclaimed.
    pins: Vec<usize>,
    /// Roots the anchor, whose word one points back at this record.
    anchor: Handle,
    /// Bytes handed out since the last cycle; what the stress switch polls.
    bytes_since_cycle: AtomicUsize,
    live_bytes: usize,
    allocated_bytes: usize,
    freed_bytes: usize,
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

/// Word zero of every wren_lift object's allocation.
static WREN_DESC: TypeDesc = desc("wren object", trace_object);
/// Word zero of a record's anchor.
static ANCHOR_DESC: TypeDesc = desc("wren heap", trace_anchor);

fn desc_ptr(d: &'static TypeDesc) -> *mut hl_type {
    d as *const TypeDesc as *mut hl_type
}

/// The core's precise trace of a wren_lift object: its children through
/// wren_lift's own visitor.
unsafe extern "C" fn trace_object(obj: *mut u8, tracer: *mut Tracer<'_>) {
    unsafe { wlift_rt_object_trace()(obj.add(PREFIX), mark_child, tracer as *mut c_void) };
}

unsafe extern "C" fn mark_child(child: *mut u8, ctx: *mut c_void) {
    unsafe { (*(ctx as *mut Tracer<'_>)).mark(child.wrapping_sub(PREFIX)) };
}

/// The anchor's trace: every pin of its record.
unsafe extern "C" fn trace_anchor(obj: *mut u8, tracer: *mut Tracer<'_>) {
    let rec = unsafe { &*(*(obj as *const usize).add(1) as *const WrenHeap) };
    let tracer = unsafe { &mut *tracer };
    for &start in &rec.pins {
        tracer.mark(start as *const u8);
    }
}

/// The record behind a handle, to read. The anchor's trace hook reads the
/// record too, from inside any core collection, so no exclusive borrow may
/// be live across a call that can collect.
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
    let ours = unsafe { *words == &WREN_DESC as *const TypeDesc as usize }
        && unsafe { *words.add(1) == rec as *const WrenHeap as usize };
    ours.then_some((start, size))
}

/// `WLIFT_GC_STRESS`: collect at every poll, as wren_lift's own heap does.
fn stress_enabled() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| std::env::var_os("WLIFT_GC_STRESS").is_some())
}

pub unsafe extern "C" fn heap_new() -> *mut c_void {
    // One hold: the anchor is a traced object nothing reaches until its
    // handle exists, and a collection in between would forget it.
    let mut gc = heap::gc_locked_init();
    let rec = Box::into_raw(Box::new(WrenHeap {
        pins: Vec::new(),
        anchor: Handle::NULL,
        bytes_since_cycle: AtomicUsize::new(0),
        live_bytes: 0,
        allocated_bytes: 0,
        freed_bytes: 0,
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
    unsafe { (anchor as *mut usize).add(1).write(rec as usize) };
    unsafe { (*rec).anchor = gc.handle_new(anchor) };
    rec as *mut c_void
}

/// wren_lift has dropped every object; their memory returns with the next
/// core sweep.
pub unsafe extern "C" fn heap_drop(heap: *mut c_void) {
    let rec = unsafe { Box::from_raw(heap as *mut WrenHeap) };
    let mut gc = heap::gc_locked_init();
    for &start in &rec.pins {
        gc.forget_allocation(start as *const u8);
    }
    let anchor = gc.handle_get(rec.anchor);
    gc.forget_allocation(anchor);
    gc.handle_release(rec.anchor);
}

pub unsafe extern "C" fn alloc_raw(heap: *mut c_void, size: usize) -> *mut u8 {
    // One hold from allocation to pin: a fresh traced object is unmarked, and
    // a collection before it is pinned would forget it.
    let gc = heap::gc_locked_init();
    let p = unsafe {
        heap::alloc_gen(
            desc_ptr(&WREN_DESC),
            size + PREFIX,
            mem::KIND_DYNAMIC | mem::TRACED,
        )
    } as *mut u8;
    if p.is_null() {
        return p;
    }
    let start = p as usize;
    unsafe { (p as *mut usize).add(1).write(heap as usize) };
    let reserved = gc
        .allocation_containing(start)
        .map_or(size + PREFIX, |(_, size)| size);
    let rec = unsafe { record_mut(heap) };
    rec.pins.push(start);
    rec.allocated_bytes += reserved;
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

pub unsafe extern "C" fn mark_allocation(_heap: *mut c_void, ptr: *mut u8) -> bool {
    heap::claim_for_cycle(ptr.wrapping_sub(PREFIX))
}

pub unsafe extern "C" fn is_marked(_heap: *mut c_void, ptr: *mut u8) -> bool {
    heap::is_claimed(ptr.wrapping_sub(PREFIX))
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

pub unsafe extern "C" fn should_collect(heap: *mut c_void) -> bool {
    if stress_enabled() {
        return unsafe { record(heap) }
            .bytes_since_cycle
            .load(Ordering::Relaxed)
            > 0;
    }
    heap::should_collect()
}

/// Holds the GC lock until `collect_end`: the claims made between are the
/// core's, and a core collection meanwhile would sweep them away.
pub unsafe extern "C" fn collect_begin(_heap: *mut c_void) {
    unsafe { heap::lock() };
}

pub unsafe extern "C" fn collect_end(heap: *mut c_void) -> usize {
    let drop_object = wlift_rt_object_drop();
    let mut gc = heap::gc_locked();
    let mut live = 0usize;
    let mut dead = Vec::new();
    unsafe { record_mut(heap) }.pins.retain(|&start| {
        let size = gc.allocation_containing(start).map_or(0, |(_, size)| size);
        if gc.is_claimed(start as *const u8) {
            live += size;
            true
        } else {
            dead.push((start, size));
            false
        }
    });
    let mut freed = 0usize;
    for (start, size) in dead {
        unsafe { drop_object((start + PREFIX) as *mut u8) };
        gc.forget_allocation(start as *const u8);
        freed += size;
    }
    // The core's collection is the sweep: it retains the pins, whose claims
    // stand, clears them, and returns the forgotten objects' lines. A
    // collection the core abandoned leaves the claims for this cycle, and the
    // next would then see them as already made.
    let before = heap::collections();
    gc.collect_garbage();
    let rec = unsafe { record_mut(heap) };
    if heap::collections() == before {
        for &start in &rec.pins {
            gc.unclaim(start as *const u8);
        }
    }
    drop(gc);
    rec.live_bytes = live;
    rec.freed_bytes += freed;
    rec.bytes_since_cycle.store(0, Ordering::Relaxed);
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
        })
    };
}
