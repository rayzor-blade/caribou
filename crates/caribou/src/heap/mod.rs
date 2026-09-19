//! The heap: a non-moving, conservative, type-agnostic Immix collector with
//! thread-local bump regions, a mutator registry with stop-the-world
//! safepoints, registered fiber stacks, deferred finalizers and parallel
//! marking, with typed objects traced and dropped through the descriptor in
//! `desc.rs`. `immix.rs` keeps the order and names of ash's `gc.rs` so the two
//! stay diffable; ash's C entry points are plain functions here, without the
//! `hlp_gc_` prefix, and ash re-exports them as `extern "C"` forwarders.

mod desc;
mod immix;

pub use desc::{CORE_MARK, DropFn, TraceFn, TypeDesc, is_descriptor};
pub use immix::{
    Finalizer,
    GC,
    GcGuard,
    GcRef,
    HL_GLOBAL_LOCK,
    Handle,
    ImmixAllocator,
    SITE_ENTER_BLOCKING,
    SITE_LEAVE_BLOCKING,
    SITE_LOCK_CONDVAR,
    SITE_LOCK_INNER,
    SITE_RUNNING,
    SITE_SAFEPOINT_WORLD_LOCK,
    SITE_SCHEDULER_IDLE,
    SITE_TLAB_REFILL,
    Tracer,
    // Roots.
    add_scan_root,
    // Allocation.
    alloc_gen,
    alloc_with_finalizer,
    allocation_size,
    // A hosted collector's claims, reclamation and trigger.
    claim_for_cycle,
    clear_scan_roots,
    collect_pending,
    collections,
    containing_allocation,
    // Collection control, statistics and diagnostics.
    dump_memory,
    enable,
    free_allocation,
    gc_add_persistent,
    gc_alloc,
    // Mutators, safepoints and the stop-the-world rendezvous.
    gc_block_at,
    // The reentrant GC lock.
    gc_guard,
    gc_lock_held_depth,
    gc_lock_unwind_to,
    gc_locked,
    gc_locked_init,
    // Fiber stacks.
    gc_register_fiber_stack,
    gc_remove_persistent,
    gc_safepoint,
    gc_set_blocking,
    gc_unblock,
    gc_unregister_fiber_stack,
    gc_update_fiber_sp,
    get_flags,
    get_live_objects,
    // Handles and root ranges.
    handle_get,
    handle_new,
    handle_release,
    handle_release_deferred,
    handle_retain,
    heartbeat_interval,
    hosted_stop,
    init,
    is_allocation_start,
    is_claimed,
    is_traced_allocation,
    lock,
    major,
    mark_site,
    mark_size,
    out_of_memory,
    print_stats,
    print_stats_if_enabled,
    profile,
    register_root,
    register_root_range,
    register_thread,
    registered_threads,
    safepoint,
    scan_roots_done,
    set_blocking_hook,
    set_deferred_collection,
    set_flags,
    set_globals,
    set_poll_request_hook,
    set_safepoint_hook,
    set_scan_roots,
    set_scan_roots_live,
    set_stack_top,
    set_stop_hook,
    should_collect,
    stats,
    stop_requested,
    thread_registered,
    thread_token,
    track_external,
    unlock,
    unregister_root_range,
    unregister_thread,
    walk_heap,
    zalloc,
};

// Only where the pool has OS threads to register, as in `immix.rs`.
#[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
pub use immix::{gc_register_current_os_thread, gc_unregister_current_os_thread};
