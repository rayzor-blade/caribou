//! The heap slots: ash's C-shaped entry points over `caribou::heap`.
//!
//! Ash's bindgen `vdynamic`/`hl_type`/`varray`/`vbyte` and `caribou_abi::hl`
//! share one layout, so pointers are cast at the edge and nothing is
//! translated.

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use ash_std::hl::{hl_type, varray, vbyte, vdynamic};
use ash_std::rt::{Finalizer, HeapVisitor};
use caribou::heap;

fn raw(ptr: Option<NonNull<u8>>) -> *mut u8 {
    ptr.map_or(ptr::null_mut(), NonNull::as_ptr)
}

pub unsafe extern "C" fn gc_alloc(size: usize) -> *mut u8 {
    raw(heap::gc_alloc(size))
}

pub unsafe extern "C" fn gc_alloc_noptr(size: usize) -> *mut u8 {
    raw(heap::gc_alloc_noptr(size))
}

pub unsafe extern "C" fn alloc_locked(size: usize) -> *mut u8 {
    raw(heap::gc_locked_init().allocate(size))
}

pub unsafe extern "C" fn alloc_locked_noptr(size: usize) -> *mut u8 {
    raw(heap::gc_locked_init().allocate_noptr(size))
}

pub unsafe extern "C" fn alloc_immortal(size: usize) -> *mut u8 {
    raw(heap::gc_locked_init().allocate_immortal(size))
}

/// `None` leaves word zero to the caller, as ash's does; the core's
/// `alloc_with_finalizer` always writes it, so that case is composed from
/// the same two steps under the one lock hold.
pub unsafe extern "C" fn alloc_with_finalizer(
    size: usize,
    finalize: Option<Finalizer>,
) -> *mut c_void {
    match finalize {
        Some(finalize) => unsafe { heap::alloc_with_finalizer(size, finalize) },
        None => {
            let mut gc = heap::gc_locked_init();
            let Some(p) = gc.allocate(size) else {
                return ptr::null_mut();
            };
            gc.register_finalizable(p.as_ptr());
            p.as_ptr().cast()
        }
    }
}

pub unsafe extern "C" fn allocation_size(ptr: *const c_void) -> usize {
    unsafe { heap::allocation_size(ptr) }
}

/// Whether `ptr` is a heap object's start, from the side table and without
/// the lock: what a closure's bound value is tested as on every call.
pub unsafe extern "C" fn is_gc_ptr(ptr: *const c_void) -> bool {
    heap::is_allocation_start(ptr)
}

pub unsafe extern "C" fn out_of_memory(what: *const u8, len: usize) -> ! {
    let what = unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(what, len)) };
    heap::out_of_memory(what)
}

pub unsafe extern "C" fn gc_safepoint() {
    heap::gc_safepoint();
}

pub unsafe extern "C" fn gc_set_blocking(blocking: bool) -> bool {
    heap::gc_set_blocking(blocking)
}

pub unsafe extern "C" fn mark_site(site: u64) {
    heap::mark_site(site);
}

pub unsafe extern "C" fn gc_register_current_os_thread() {
    #[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
    heap::gc_register_current_os_thread();
}

pub unsafe extern "C" fn gc_unregister_current_os_thread() {
    #[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
    heap::gc_unregister_current_os_thread();
}

pub unsafe extern "C" fn gc_register_fiber_stack(id: u32, base: usize, size: usize) {
    unsafe { heap::gc_register_fiber_stack(u64::from(id), base, size) };
}

pub unsafe extern "C" fn gc_update_fiber_sp(id: u32, sp: usize) {
    unsafe { heap::gc_update_fiber_sp(u64::from(id), sp) };
}

pub unsafe extern "C" fn gc_unregister_fiber_stack(id: u32) {
    unsafe { heap::gc_unregister_fiber_stack(u64::from(id)) };
}

pub unsafe extern "C" fn gc_add_persistent(ptr: *mut vdynamic) {
    unsafe { heap::gc_add_persistent(ptr.cast()) };
}

pub unsafe extern "C" fn gc_remove_persistent(ptr: *mut vdynamic) {
    unsafe { heap::gc_remove_persistent(ptr.cast()) };
}

pub unsafe extern "C" fn add_root_slot(slot: usize) {
    heap::gc_locked_init().add_root_slot(slot);
}

pub unsafe extern "C" fn remove_root_slot(slot: usize) {
    heap::gc_locked_init().remove_root_slot(slot);
}

pub unsafe extern "C" fn gc_lock() {
    unsafe { heap::lock() };
}

pub unsafe extern "C" fn gc_unlock() {
    unsafe { heap::unlock() };
}

pub unsafe extern "C" fn gc_lock_held_depth() -> usize {
    heap::gc_lock_held_depth()
}

pub unsafe extern "C" fn gc_lock_unwind_to(depth: usize) {
    heap::gc_lock_unwind_to(depth);
}

pub unsafe extern "C" fn gc_registered_threads(out: *mut u64, cap: usize) -> usize {
    unsafe { heap::registered_threads(out, cap) }
}

pub unsafe extern "C" fn gc_print_stats() {
    heap::print_stats();
}

pub unsafe extern "C" fn mark_size(data_size: i32) -> i32 {
    heap::mark_size(data_size)
}

pub unsafe extern "C" fn gc_walk_heap(visitor: HeapVisitor, ctx: *mut c_void) {
    // Same signature over the same layouts; only the pointee names differ.
    type CoreVisitor = unsafe extern "C" fn(
        *mut caribou_abi::hl::vdynamic,
        *mut caribou_abi::hl::hl_type,
        *mut c_void,
    );
    let visitor: CoreVisitor = unsafe { std::mem::transmute::<HeapVisitor, CoreVisitor>(visitor) };
    unsafe { heap::walk_heap(visitor, ctx) };
}

pub unsafe extern "C" fn gc_init() {
    heap::init();
}

pub unsafe extern "C" fn gc_set_stack_top(top: usize) {
    unsafe { heap::set_stack_top(top) };
}

pub unsafe extern "C" fn register_thread(stack_top: *mut c_void) {
    unsafe { heap::register_thread(stack_top) };
}

pub unsafe extern "C" fn unregister_thread() {
    heap::unregister_thread();
}

pub unsafe extern "C" fn gc_set_globals(ptr: *const *mut c_void, count: usize) {
    unsafe { heap::set_globals(ptr, count) };
}

pub unsafe extern "C" fn gc_scan_roots_done() {
    heap::scan_roots_done();
}

pub unsafe extern "C" fn gc_clear_scan_roots() {
    heap::clear_scan_roots();
}

pub unsafe extern "C" fn gc_add_scan_root(ptr: *const c_void, size: usize) {
    unsafe { heap::add_scan_root(ptr, size) };
}

pub unsafe extern "C" fn gc_set_scan_roots_live(ranges: *const (usize, usize), len: *const usize) {
    unsafe { heap::set_scan_roots_live(ranges, len) };
}

pub unsafe extern "C" fn gc_set_scan_roots(ranges: *const (usize, usize), count: usize) {
    unsafe { heap::set_scan_roots(ranges, count) };
}

pub unsafe extern "C" fn gc_track_external(bytes: u64) {
    heap::track_external(bytes);
}

pub unsafe extern "C" fn gc_enable(b: bool) {
    heap::enable(b);
}

pub unsafe extern "C" fn gc_get_flags() -> i32 {
    heap::get_flags()
}

pub unsafe extern "C" fn gc_set_flags(f: i32) {
    heap::set_flags(f);
}

pub unsafe extern "C" fn gc_major() {
    heap::major();
}

pub unsafe extern "C" fn gc_stats(
    total_allocated: *mut f64,
    allocation_count: *mut f64,
    current_memory: *mut f64,
) {
    unsafe { heap::stats(total_allocated, allocation_count, current_memory) };
}

pub unsafe extern "C" fn gc_profile(b: bool) {
    heap::profile(b);
}

/// -1, upstream's "cannot answer": the core records no allocation's type.
pub unsafe extern "C" fn gc_get_live_objects(t: *mut hl_type, arr: *mut varray) -> i32 {
    heap::get_live_objects(t.cast(), arr.cast())
}

pub unsafe extern "C" fn gc_dump_memory(filename: *mut vbyte) {
    unsafe { heap::dump_memory(filename) };
}
