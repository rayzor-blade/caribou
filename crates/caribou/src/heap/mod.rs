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
    gc_alloc_noptr,
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

/// Overwrite the stack below the caller's frame and the general-purpose
/// registers, where a pointer the program no longer needs would still be
/// found by the conservative scan: what a test does between letting an
/// object go and the collection it expects to take it. The caller's own
/// frame is not touched; a value it still holds is a value it means to.
#[inline(never)]
pub fn scrub_stack_and_registers() {
    let buf = [0u8; 1 << 16];
    std::hint::black_box(&buf);
    scrub_registers();
}

/// Zero every register the compiler may leave a dead value in. `rbx` is
/// LLVM's own on x86-64, `x18` the platform's and `x19` LLVM's on
/// AArch64, so those keep what they hold.
#[inline(never)]
fn scrub_registers() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: only registers named as clobbered are written; no memory.
    unsafe {
        core::arch::asm!(
            "xor eax, eax", "xor ecx, ecx", "xor edx, edx", "xor esi, esi", "xor edi, edi",
            "xor r8d, r8d", "xor r9d, r9d", "xor r10d, r10d", "xor r11d, r11d",
            "xor r12d, r12d", "xor r13d, r13d", "xor r14d, r14d", "xor r15d, r15d",
            out("rax") _, out("rcx") _, out("rdx") _, out("rsi") _, out("rdi") _,
            out("r8") _, out("r9") _, out("r10") _, out("r11") _,
            out("r12") _, out("r13") _, out("r14") _, out("r15") _,
            options(nomem, nostack),
        );
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: as above.
    unsafe {
        core::arch::asm!(
            "mov x0, xzr", "mov x1, xzr", "mov x2, xzr", "mov x3, xzr", "mov x4, xzr",
            "mov x5, xzr", "mov x6, xzr", "mov x7, xzr", "mov x8, xzr", "mov x9, xzr",
            "mov x10, xzr", "mov x11, xzr", "mov x12, xzr", "mov x13, xzr", "mov x14, xzr",
            "mov x15, xzr", "mov x16, xzr", "mov x17, xzr", "mov x20, xzr",
            "mov x21, xzr", "mov x22, xzr", "mov x23, xzr", "mov x24, xzr", "mov x25, xzr",
            "mov x26, xzr", "mov x27, xzr", "mov x28, xzr",
            out("x0") _, out("x1") _, out("x2") _, out("x3") _, out("x4") _,
            out("x5") _, out("x6") _, out("x7") _, out("x8") _, out("x9") _,
            out("x10") _, out("x11") _, out("x12") _, out("x13") _, out("x14") _,
            out("x15") _, out("x16") _, out("x17") _, out("x20") _,
            out("x21") _, out("x22") _, out("x23") _, out("x24") _, out("x25") _,
            out("x26") _, out("x27") _, out("x28") _,
            options(nomem, nostack),
        );
    }
}
