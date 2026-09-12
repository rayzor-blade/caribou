//! The heap: a non-moving, conservative, type-agnostic Immix collector with
//! thread-local bump regions, a mutator registry with stop-the-world
//! safepoints, registered fiber stacks, deferred finalizers and parallel
//! marking, with typed objects traced and dropped through the descriptor in
//! `desc.rs`. `immix.rs` keeps the order and names of ash's `gc.rs` so the two
//! stay diffable; ash's C entry points are plain functions here, without the
//! `hlp_gc_` prefix, and ash re-exports them as `extern "C"` forwarders.

mod desc;
mod immix;

pub use desc::{DropFn, TraceFn, TypeDesc};
pub use immix::{
    // Allocation.
    alloc_gen, alloc_with_finalizer, allocation_size, gc_alloc, mark_size, out_of_memory, zalloc,
    Finalizer, ImmixAllocator, Tracer, GC,
    // The reentrant GC lock.
    gc_guard, gc_lock_held_depth, gc_lock_unwind_to, gc_locked, gc_locked_init, lock, unlock,
    GcGuard, GcRef, HL_GLOBAL_LOCK,
    // Mutators, safepoints and the stop-the-world rendezvous.
    gc_safepoint, gc_set_blocking, mark_site, register_thread, registered_threads, safepoint,
    set_poll_request_hook, set_stack_top, unregister_thread, SITE_ENTER_BLOCKING,
    SITE_LEAVE_BLOCKING, SITE_LOCK_CONDVAR, SITE_LOCK_INNER, SITE_RUNNING,
    SITE_SAFEPOINT_WORLD_LOCK, SITE_SCHEDULER_IDLE, SITE_TLAB_REFILL,
    // Roots.
    add_scan_root, clear_scan_roots, gc_add_persistent, gc_remove_persistent, register_root,
    scan_roots_done, set_globals, set_scan_roots, set_scan_roots_live,
    // Handles and root ranges.
    handle_get, handle_new, handle_release, handle_retain, register_root_range,
    unregister_root_range, Handle,
    // Fiber stacks.
    gc_register_fiber_stack, gc_unregister_fiber_stack, gc_update_fiber_sp,
    // Collection control, statistics and diagnostics.
    dump_memory, enable, get_flags, get_live_objects, init, major, print_stats,
    print_stats_if_enabled, profile, set_flags, stats, track_external, walk_heap,
};

// Only where the pool has OS threads to register, as in `immix.rs`.
#[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
pub use immix::{gc_register_current_os_thread, gc_unregister_current_os_thread};
