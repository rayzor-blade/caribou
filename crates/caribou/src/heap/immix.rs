// `static mut` + raw-pointer access is deliberate (single-threaded VM
// invariant): `static_mut_refs` demands the `&raw`/deref spelling, which
// these two lints then flag.
#![allow(clippy::deref_addrof, dangerous_implicit_autorefs)]
// ash's crate-wide allowances this module relies on.
#![allow(
    clippy::missing_safety_doc,
    clippy::too_many_arguments,
    clippy::not_unsafe_ptr_arg_deref
)]
use super::desc::TypeDesc;
use caribou_abi::hl::{self, HL_WSIZE, hl_type, hl_type_obj};
use std::cell::{Cell, RefCell};
use std::os::raw::c_void;
use std::ptr::{self, NonNull};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, Instant};
use std::{
    collections::{HashMap, HashSet},
    mem,
};
#[cfg(windows)]
use windows_sys::Win32::System::Memory::{
    DiscardVirtualMemory, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc,
    VirtualFree,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

const BLOCK_SIZE: usize = 32 * 1024; // 32 KB
const LINE_SIZE: usize = 128; // 128 bytes
const ALLOC_QUANTUM: usize = 16;
const OBJECT_MARK: u8 = 0x80;
const SPAN_OBJECT: u8 = (LINE_SIZE / ALLOC_QUANTUM + 1) as u8;
/// The rest of an `objects` byte: the two kind bits say how the marker treats
/// the allocation's words, the low five are the size code.
const OBJECT_KIND_MASK: u8 = 0x60;
/// Scanned conservatively: `Raw`, `Finalizer`, and `Typed` behind a bare
/// `hl_type`. What every allocation path but `alloc_gen` records.
const OBJECT_KIND_RAW: u8 = 0x00;
/// Never scanned.
const OBJECT_KIND_NOPTR: u8 = 0x20;
/// Word zero is a `*const TypeDesc`; the marker traces through its hook and
/// sweep drops through the other. `0x60` is reserved.
const OBJECT_KIND_TRACED: u8 = 0x40;
const OBJECT_SIZE_MASK: u8 = 0x1F;

/// Word zero of a `MEM_KIND_FINALIZER` block: `void (*)(void *block)`,
/// written by the caller after allocation (upstream's `hl_gc_alloc_finalizer`).
pub use caribou_abi::mem::Finalizer;

/// Finalizers of blocks a collection found unreachable, run at the next
/// outermost GC-lock release (`GcGuard::drop`): a body is arbitrary C that may
/// allocate or take the lock, so it cannot run inside the collector.
static PENDING_FINALIZERS: std::sync::Mutex<Vec<(usize, Finalizer)>> =
    std::sync::Mutex::new(Vec::new());

/// Entry count of `PENDING_FINALIZERS`: one relaxed load on every outermost
/// lock release instead of a mutex acquisition.
static PENDING_FINALIZER_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Allocate a block whose word zero holds `finalize`, called once nothing can
/// reach the block. `size` must leave room for the pointer.
pub unsafe fn alloc_with_finalizer(size: usize, finalize: Finalizer) -> *mut c_void {
    debug_assert!(size >= mem::size_of::<usize>());
    let mut gc = gc_locked_init();
    let Some(ptr) = gc.allocate(size) else {
        return ptr::null_mut();
    };
    let p = ptr.as_ptr();
    // Both under the one lock hold. A collection between the two would find a
    // registered block with a null callback and quietly skip it.
    gc.register_finalizable(p);
    unsafe { (p as *mut usize).write(finalize as *const () as usize) };
    p as *mut c_void
}

/// Call the finalizers a collection queued. The GC lock must NOT be held.
fn run_pending_finalizers() {
    thread_local! {
        static RUNNING: Cell<bool> = const { Cell::new(false) };
    }
    struct Running;
    impl Drop for Running {
        fn drop(&mut self) {
            RUNNING.with(|r| r.set(false));
        }
    }
    // A body may allocate, trigger a collection and re-enter here from its
    // release; those entries stay queued for the next release. A body that
    // longjmps past the guard leaves the flag set.
    if RUNNING.with(|r| r.replace(true)) {
        return;
    }
    let _running = Running;

    // Swap the queue out rather than calling under its lock: a body that
    // allocates enough to trigger a collection would otherwise deadlock
    // against the collector queueing the next batch.
    let due = {
        let Ok(mut queue) = PENDING_FINALIZERS.lock() else {
            return;
        };
        PENDING_FINALIZER_COUNT.store(0, Ordering::Relaxed);
        mem::take(&mut *queue)
    };
    for (block, finalize) in due {
        unsafe { finalize(block as *mut c_void) };
    }
}

thread_local! {
    /// Handles given up inside a collection, by the drop hook of a traced
    /// object that owned one; released at the next outermost lock release
    /// on this thread, a hook being unable to take the lock itself. Per
    /// thread: a hook runs under the lock, on the thread that releases it.
    static DEFERRED_RELEASES: RefCell<Vec<Handle>> = const { RefCell::new(Vec::new()) };
}

/// Release `h` once the GC lock is next free: what a drop hook calls for a
/// handle its object held. Nothing else should; `handle_release` is direct.
pub fn handle_release_deferred(h: Handle) {
    if h.is_null() {
        return;
    }
    DEFERRED_RELEASES.with(|queue| queue.borrow_mut().push(h));
}

/// Whether this thread has handles queued.
fn deferred_releases_pending() -> bool {
    DEFERRED_RELEASES.with(|queue| !queue.borrow().is_empty())
}

/// Release the handles a collection queued. The GC lock must NOT be held.
fn release_deferred_handles() {
    let due = DEFERRED_RELEASES.with(|queue| mem::take(&mut *queue.borrow_mut()));
    if due.is_empty() {
        return;
    }
    let mut gc = gc_locked_init();
    for h in due {
        gc.handle_release(h);
    }
}

/// Release one level of the GC lock, and drain the deferred queues if that
/// freed it. Every release goes through here so the drains cannot be missed.
/// A finalizer may give up a handle, so the drains repeat until both queues
/// are empty.
fn gc_lock_release() {
    if !GC_LOCK.release() {
        return;
    }
    loop {
        let releases = deferred_releases_pending();
        if releases {
            release_deferred_handles();
        }
        let finalizers = PENDING_FINALIZER_COUNT.load(Ordering::Relaxed) != 0;
        if finalizers {
            run_pending_finalizers();
        }
        if !releases && !finalizers {
            break;
        }
    }
}

/// Zeroed atomic bytes are valid. Use calloc-style allocation so an arena's
/// reservation does not eagerly touch a side table proportional to its cap.
fn allocation_table(count: usize) -> Vec<std::sync::atomic::AtomicU8> {
    unsafe {
        let layout = std::alloc::Layout::array::<std::sync::atomic::AtomicU8>(count)
            .expect("allocation table layout");
        let p = std::alloc::alloc_zeroed(layout) as *mut std::sync::atomic::AtomicU8;
        if p.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        Vec::from_raw_parts(p, count, count)
    }
}
/// How far below a stack's top to scan when nothing says where it ends.
/// wasm cannot ask a thread's stack size, and a recorded suspend point may sit
/// above live frames, so the window is taken from the top: larger than either
/// stack this runtime makes, small enough not to be the heap.
#[cfg(target_family = "wasm")]
const WASM_STACK_WINDOW: usize = 1024 * 1024;
#[cfg(not(target_family = "wasm"))]
const WASM_STACK_WINDOW: usize = 0;

/// The stride of every conservative walk: a machine word, since every walker
/// reads a `usize`. An eight-byte stride would skip every other slot on a
/// 32-bit target.
const WORD: usize = std::mem::size_of::<usize>();
/// `a` rounded up to a word boundary.
const fn word_align_up(a: usize) -> usize {
    (a + WORD - 1) & !(WORD - 1)
}

/// Normalize after widening: `top` may be a byte-sized AOT stack anchor.
/// Aligning only `saved_sp` lets `top - window` undo the alignment, making
/// every word read miss the real pointer slots on the suspended main stack.
fn stack_scan_start(saved_sp: usize, top: usize, window: usize) -> usize {
    word_align_up(saved_sp.min(top.saturating_sub(window)))
}
const LINES_PER_BLOCK: usize = BLOCK_SIZE / LINE_SIZE;
/// 64 line-claim bits to a word.
const MARK_WORDS: usize = LINES_PER_BLOCK / 64;

/// Floor and ceiling on the machine-derived heap cap. The mapping is virtual
/// and demand-zeroed; what the cap sizes eagerly is the per-block metadata,
/// which the ceiling bounds.
const HEAP_MAX_FLOOR: usize = 512 * 1024 * 1024;
/// A 32-bit target cannot name four gigabytes in a `usize`, and wasm's memory
/// is bounded below it anyway.
#[cfg(target_pointer_width = "64")]
const HEAP_MAX_CEILING: usize = 4 * 1024 * 1024 * 1024;
#[cfg(not(target_pointer_width = "64"))]
const HEAP_MAX_CEILING: usize = 1024 * 1024 * 1024;
/// Share of usable RAM the heap cap defaults to.
const HEAP_MAX_SHARE: usize = 4;
/// First collection fires after this many bytes allocated.
const INITIAL_TRIGGER_BYTES: usize = 4 * 1024 * 1024;
/// Adaptive threshold bounds: live*growth clamped to [floor, ceiling].
const DEFAULT_TRIGGER_FLOOR: usize = 8 * 1024 * 1024;
/// Bounds on the machine-derived ceiling (see `trigger_ceiling_bytes`).
const TRIGGER_CEILING_MIN: usize = 64 * 1024 * 1024;
const TRIGGER_CEILING_MAX: usize = 512 * 1024 * 1024;
/// Wall-clock heartbeat: any allocation this long after the last collection
/// forces one, so long-idle processes deflate.
const HEARTBEAT: Duration = Duration::from_secs(30);

/// The heartbeat interval; `CARIBOU_GC_HEARTBEAT_MS` overrides it (safe;
/// for tests of the idle path).
pub fn heartbeat_interval() -> Duration {
    static V: OnceLock<Duration> = OnceLock::new();
    *V.get_or_init(|| {
        env_usize("CARIBOU_GC_HEARTBEAT_MS")
            .map(|ms| Duration::from_millis(ms as u64))
            .unwrap_or(HEARTBEAT)
    })
}

/// Throttle for malloc_zone_pressure_relief.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // macOS-only mechanism
const PRESSURE_RELIEF_MIN_INTERVAL: Duration = Duration::from_millis(500);

// ── Env-gated config (read once; getenv on the allocation path takes a
// process-wide lock) ────────────────────────────────────────────────────────
//
// The names stay `ASH_GC_*` and the reports keep their `[gc]`/`[ash]`
// prefixes through the transition; `CARIBOU_GC_*` aliases come later.

/// How many words [`spill_callee_saved`] writes.
const CALLEE_SAVED_WORDS: usize = 10;

/// Write the callee-saved general-purpose registers into `buf`, so a GC
/// pointer the compiler kept in one across a call is on the scanned stack.
/// aarch64: x19–x28; the float registers never hold a GC pointer.
///
/// `#[inline(never)]` so the store cannot be sunk past the scan that reads it.
#[cfg(target_arch = "aarch64")]
#[inline(never)]
fn spill_callee_saved(buf: &mut [usize; CALLEE_SAVED_WORDS]) {
    unsafe {
        std::arch::asm!(
            "stp x19, x20, [{p}, #0]",
            "stp x21, x22, [{p}, #16]",
            "stp x23, x24, [{p}, #32]",
            "stp x25, x26, [{p}, #48]",
            "stp x27, x28, [{p}, #64]",
            p = in(reg) buf.as_mut_ptr(),
            options(nostack, preserves_flags),
        );
    }
}

/// x86-64: rbx, rbp, r12–r15.
#[cfg(target_arch = "x86_64")]
#[inline(never)]
fn spill_callee_saved(buf: &mut [usize; CALLEE_SAVED_WORDS]) {
    unsafe {
        std::arch::asm!(
            "mov [{p} + 0], rbx",
            "mov [{p} + 8], rbp",
            "mov [{p} + 16], r12",
            "mov [{p} + 24], r13",
            "mov [{p} + 32], r14",
            "mov [{p} + 40], r15",
            p = in(reg) buf.as_mut_ptr(),
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline(never)]
fn spill_callee_saved(_buf: &mut [usize; CALLEE_SAVED_WORDS]) {}

// ── Mutator bump region (TLAB) ──────────────────────────────────────────────
//
// A mutator allocates through a private bump region carved from an ordinary
// Immix block: no lock, no per-object memset (zeroed at refill), trigger
// accounting per region. Oversized objects and stress mode take the locked
// path.
//
// Soundness:
// * the region's block is in `used_blocks` and `sweep` never frees a block
//   in `tlab_blocks`, so a collection mid-region is safe;
// * small objects never straddle a 128-byte line, so their start is found
//   within that line's metadata;
// * `ASH_GC_STRESS` disables the TLAB: stress promises a collection every
//   Nth allocation, and the bump path skips the counter.

thread_local! {
    /// This thread's bump region. Per thread, because the fiber pool runs VM
    /// code on every worker. `const`-initialized so the access carries no
    /// lazy-init branch, and one struct rather than one `thread_local!` per
    /// field so the allocation fast path pays a single TLS lookup.
    static TLAB: Tlab = const {
        Tlab {
            cur: Cell::new(0),
            limit: Cell::new(0),
            block: Cell::new(usize::MAX),
            objects: Cell::new(std::ptr::null()),
            heap_base: Cell::new(0),
            registered: Cell::new(false),
            deferred: Cell::new(false),
            polls: AtomicU64::new(0),
            site: AtomicU64::new(0),
        }
    };
}

/// This thread's bump region, plus whether it is a registered mutator.
struct Tlab {
    /// Bump cursor and the end of the region.
    cur: Cell<usize>,
    limit: Cell<usize>,
    /// Heap offset of the block this thread is bumping through, so a refill
    /// can hand the previous one back to the sweep.
    block: Cell<usize>,
    /// Stable side table, shared atomically with the stopped-world marker.
    /// A bump publishes its boundary before returning the new allocation.
    objects: Cell<*const std::sync::atomic::AtomicU8>,
    heap_base: Cell<usize>,
    /// How many times this thread has entered `gc_safepoint` with a stop
    /// pending. Atomic because the collector reads it, by address, to say
    /// whether a straggler is running safepoint code at all.
    polls: AtomicU64,
    /// The last blocking place this thread entered, as a `SITE_*` code. Written
    /// only on paths that can wait.
    site: AtomicU64,
    /// Whether this thread is registered with `MUTATOR_WORLD`. Here because the
    /// allocation fast path reads it.
    registered: Cell<bool>,
    /// This mutator's roots are complete only at its own safepoints, so a
    /// trigger that fires inside its allocation is recorded, never run
    /// there. See `set_deferred_collection`.
    deferred: Cell<bool>,
}

/// Largest object the bump region serves. At one line, nothing in the
/// region ever needs an `alloc_sizes` span entry.
const TLAB_MAX_OBJ: usize = LINE_SIZE;

/// The current thread's identity, cheap enough for a per-allocation check:
/// a thread-pointer register read where the platform has one, never zero.
#[inline(always)]
fn thread_self_fast() -> u64 {
    // A wasm thread's own `__tls_base` makes the address of any thread-local
    // distinct per thread and stable for its life. This is the identity every
    // mutator, TLAB and world stop keys on, so it must differ per thread.
    #[cfg(target_family = "wasm")]
    {
        thread_local! {
            static IDENTITY: u8 = const { 0 };
        }
        IDENTITY.with(|slot| slot as *const u8 as u64)
    }
    // One agent, one identity. A target with no threads still has to answer,
    // and a constant is the honest answer rather than a syscall that lies.
    #[cfg(not(any(unix, windows, target_family = "wasm")))]
    {
        1
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    unsafe {
        let tpidrro: u64;
        std::arch::asm!("mrs {}, TPIDRRO_EL0", out(reg) tpidrro, options(nomem, nostack, preserves_flags));
        tpidrro & !0x7
    }
    // Linux/x86_64: glibc's pthread_self is a load of the TCB self-pointer at
    // fs:0x10, so read it directly. A wrong value only sends allocation down
    // the locked path.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    unsafe {
        let tp: u64;
        std::arch::asm!(
            "mov {}, qword ptr fs:[0x10]",
            out(reg) tp,
            options(nomem, nostack, preserves_flags)
        );
        tp
    }
    // Linux/aarch64 keeps its thread pointer in TPIDR_EL0, same idea.
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    unsafe {
        let tp: u64;
        std::arch::asm!("mrs {}, TPIDR_EL0", out(reg) tp, options(nomem, nostack, preserves_flags));
        tp
    }
    // Windows: the thread id, a TEB field read behind a stub.
    #[cfg(windows)]
    unsafe {
        GetCurrentThreadId() as u64
    }
    #[cfg(all(
        unix,
        not(all(target_os = "macos", target_arch = "aarch64")),
        not(all(target_os = "linux", target_arch = "x86_64")),
        not(all(target_os = "linux", target_arch = "aarch64"))
    ))]
    unsafe {
        libc::pthread_self() as u64
    }
}

// ── Registered mutators and stop-the-world rendezvous ──────────────────────
//
// The heap lock cannot be the rendezvous lock: a collector owns it while
// waiting, and a mutator may be asleep trying to acquire it. The registry
// has its own mutex and condvar.

#[derive(Clone)]
struct MutatorSnapshot {
    thread: u64,
    stack_top: usize,
    stack_sp: usize,
    saved_regs: [usize; CALLEE_SAVED_WORDS],
    scan_ranges: Vec<(usize, usize)>,
}

struct MutatorRecord {
    thread: u64,
    /// How this thread became a mutator; reported when a world stop is slow.
    role: &'static str,
    stack_top: usize,
    stopped_sp: usize,
    saved_regs: [usize; CALLEE_SAVED_WORDS],
    blocking_depth: u32,
    parked: bool,
    scan_ranges: Vec<(usize, usize)>,
    staged_scan_ranges: Vec<(usize, usize)>,
    /// A live view of the mutator's range table: `(ranges, len)`, raw pointers
    /// into memory it owns while registered. Copied once per collection, at the
    /// snapshot, where the mutator is stopped and the table cannot move.
    scan_live: Option<(usize, usize)>,
    /// Address of this thread's safepoint counter (in its own `Tlab`, which
    /// outlives the record), and its value when the stop was requested: a
    /// straggler that has not moved it never ran `gc_safepoint`.
    polls: usize,
    polls_at_stop: u64,
    /// Where this thread last entered a place it could wait. See `mark_site`.
    site: usize,
}

#[derive(Default)]
struct MutatorWorldState {
    stop_requested: bool,
    collector: u64,
    mutators: Vec<MutatorRecord>,
}

struct MutatorWorld {
    state: std::sync::Mutex<MutatorWorldState>,
    changed: std::sync::Condvar,
}

static MUTATOR_WORLD: LazyLock<MutatorWorld> = LazyLock::new(|| MutatorWorld {
    state: std::sync::Mutex::new(MutatorWorldState::default()),
    changed: std::sync::Condvar::new(),
});
static GC_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// How long the collector waits for every mutator to reach a safepoint before
/// giving up on this collection: past it, a deferred collection beats a
/// frozen program.
const STOP_THE_WORLD_DEADLINE: std::time::Duration = std::time::Duration::from_millis(2000);
/// When the current stop was asked for, as nanoseconds since the process's
/// first collection; a thread arriving late reports where it was.
static GC_STOP_ASKED_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static GC_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

fn register_current_mutator(stack_top: usize, role: &'static str) {
    if stack_top == 0 {
        return;
    }
    let thread = thread_self_fast();
    let mut world = MUTATOR_WORLD.state.lock().unwrap();
    while world.stop_requested && world.collector != thread {
        world = MUTATOR_WORLD.changed.wait(world).unwrap();
    }
    if let Some(record) = world.mutators.iter_mut().find(|m| m.thread == thread) {
        record.stack_top = stack_top;
    } else {
        world.mutators.push(MutatorRecord {
            thread,
            role,
            stack_top,
            stopped_sp: 0,
            saved_regs: [0; CALLEE_SAVED_WORDS],
            blocking_depth: 0,
            parked: false,
            scan_ranges: Vec::new(),
            staged_scan_ranges: Vec::new(),
            scan_live: None,
            polls: TLAB.with(|t| &t.polls as *const AtomicU64 as usize),
            polls_at_stop: 0,
            site: TLAB.with(|t| &t.site as *const AtomicU64 as usize),
        });
    }
    TLAB.with(|t| t.registered.set(true));
}

/// Register the current OS worker using the platform's real stack boundary.
/// A guessed `sp + N` can cross an unmapped guard page and make conservative
/// scanning fault, especially with custom thread stack sizes.
#[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
pub fn gc_register_current_os_thread() {
    #[cfg(target_os = "macos")]
    let stack_top = unsafe { libc::pthread_get_stackaddr_np(libc::pthread_self()) as usize };

    #[cfg(target_os = "linux")]
    let stack_top = unsafe {
        let mut attr: libc::pthread_attr_t = mem::zeroed();
        let mut top = 0usize;
        if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) == 0 {
            let mut base: *mut c_void = ptr::null_mut();
            let mut size: libc::size_t = 0;
            if libc::pthread_attr_getstack(&attr, &mut base, &mut size) == 0 && !base.is_null() {
                top = base as usize + size;
            }
            libc::pthread_attr_destroy(&mut attr);
        }
        top
    };

    #[cfg(windows)]
    let stack_top = unsafe {
        let mut low = 0usize;
        let mut high = 0usize;
        windows_sys::Win32::System::Threading::GetCurrentThreadStackLimits(&mut low, &mut high);
        high
    };

    // A wasm thread's stack is a block anywhere in linear memory, of unknown
    // size. This runs in the thread's outermost frame, so every root it will
    // hold is below this local; anything above it may be past the end of
    // linear memory.
    #[cfg(target_family = "wasm")]
    let stack_top = {
        let anchor = 0usize;
        // This frame's own slots, and nothing above them.
        (&anchor as *const usize as usize) + mem::size_of::<usize>() * 8
    };

    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        windows,
        target_family = "wasm"
    )))]
    let stack_top = {
        let anchor = 0usize;
        (&anchor as *const usize as usize) + 1024 * 1024
    };

    if stack_top != 0 {
        register_current_mutator(stack_top, "os-worker");
    }
}

#[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
pub fn gc_unregister_current_os_thread() {
    unregister_current_mutator();
}

fn unregister_current_mutator() {
    let thread = thread_self_fast();
    let mut world = MUTATOR_WORLD.state.lock().unwrap();
    world.mutators.retain(|m| m.thread != thread);
    TLAB.with(|t| {
        t.registered.set(false);
        t.deferred.set(false);
    });
    MUTATOR_WORLD.changed.notify_all();
    drop(world);

    // The region goes back to the sweep with the thread that owned it.
    release_tlab_region(&mut gc_locked());
}

#[inline]
fn current_mutator_registered() -> bool {
    TLAB.with(|t| t.registered.get())
}

#[inline]
fn current_mutator_deferred() -> bool {
    TLAB.with(|t| t.deferred.get())
}

/// Park a registered mutator at a safepoint. The spill buffer stays in this
/// frame for the whole wait, so `stopped_sp` describes live memory until the
/// collector releases the world.
#[inline(never)]
pub fn gc_safepoint() {
    if !GC_STOP_REQUESTED.load(Ordering::Acquire) {
        return;
    }
    // Only ever reached with a stop pending, so the steady-state cost of this
    // is the branch above and nothing else.
    TLAB.with(|t| t.polls.fetch_add(1, Ordering::Relaxed));
    if !current_mutator_registered() {
        return;
    }
    mark_site(SITE_SAFEPOINT_WORLD_LOCK);
    let thread = thread_self_fast();
    let mut saved_regs = [0usize; CALLEE_SAVED_WORDS];
    spill_callee_saved(&mut saved_regs);
    let sp = ImmixAllocator::current_stack_addr().min(saved_regs.as_ptr() as usize);

    let mut world = MUTATOR_WORLD.state.lock().unwrap();
    if !world.stop_requested || world.collector == thread {
        return;
    }
    let Some(index) = world.mutators.iter().position(|m| m.thread == thread) else {
        return;
    };
    {
        let record = &mut world.mutators[index];
        record.stopped_sp = sp;
        record.saved_regs = saved_regs;
        record.parked = true;
    }
    // Capture the frames here but print after the world restarts: this thread
    // holds MUTATOR_WORLD, which the collector must retake to see the park, and
    // symbolication would lengthen the stop being reported.
    let late = if gc_stats_enabled() {
        let asked = GC_STOP_ASKED_NS.load(Ordering::Relaxed);
        let waited_ms = (GC_EPOCH.elapsed().as_nanos() as u64).saturating_sub(asked) as f64 / 1e6;
        (waited_ms > 20.0).then(|| (waited_ms, std::backtrace::Backtrace::force_capture()))
    } else {
        None
    };
    MUTATOR_WORLD.changed.notify_all();
    while world.stop_requested {
        world = MUTATOR_WORLD.changed.wait(world).unwrap();
    }
    if let Some(record) = world.mutators.iter_mut().find(|m| m.thread == thread) {
        record.parked = false;
        record.stopped_sp = 0;
    }
    mark_site(SITE_RUNNING);
    drop(world);
    if let Some((waited_ms, frames)) = late {
        eprintln!(
            "[gc] thread {:#x} reached a safepoint {:.1}ms after the stop was asked for; it was at:\n{}",
            thread, waited_ms, frames
        );
    }
}

/// Publish or retire the saved context used while a native call blocks its
/// OS worker. The collector may scan that context without waiting for a
/// poll; running HL code while marked blocking violates the contract.
pub fn gc_set_blocking(blocking: bool) -> bool {
    if !current_mutator_registered() {
        return false;
    }
    if blocking {
        gc_safepoint();
    }
    mark_site(if blocking {
        SITE_ENTER_BLOCKING
    } else {
        SITE_LEAVE_BLOCKING
    });
    let thread = thread_self_fast();
    let mut saved_regs = [0usize; CALLEE_SAVED_WORDS];
    spill_callee_saved(&mut saved_regs);
    let sp = ImmixAllocator::current_stack_addr().min(saved_regs.as_ptr() as usize);
    let mut world = MUTATOR_WORLD.state.lock().unwrap();
    let Some(index) = world.mutators.iter().position(|m| m.thread == thread) else {
        return false;
    };

    if blocking {
        let record = &mut world.mutators[index];
        record.blocking_depth = record.blocking_depth.saturating_add(1);
        record.stopped_sp = sp;
        record.saved_regs = saved_regs;
        MUTATOR_WORLD.changed.notify_all();
        return true;
    }
    if world.mutators[index].blocking_depth == 0 {
        return false;
    }
    world.mutators[index].blocking_depth -= 1;
    if world.mutators[index].blocking_depth != 0 {
        return true;
    }

    // A thread leaving its native blocking section while collection is in
    // progress joins the parked mutators before it may execute HL again.
    if world.stop_requested && world.collector != thread {
        world.mutators[index].stopped_sp = sp;
        world.mutators[index].saved_regs = saved_regs;
        world.mutators[index].parked = true;
        MUTATOR_WORLD.changed.notify_all();
        while world.stop_requested {
            world = MUTATOR_WORLD.changed.wait(world).unwrap();
        }
        if let Some(record) = world.mutators.iter_mut().find(|m| m.thread == thread) {
            record.parked = false;
            record.stopped_sp = 0;
        }
    } else {
        world.mutators[index].stopped_sp = 0;
    }
    mark_site(SITE_RUNNING);
    true
}

struct StoppedWorld {
    snapshots: Vec<MutatorSnapshot>,
    requested: bool,
    /// Whether every mutator actually stopped. False means the attempt was
    /// abandoned, and nothing may be scanned.
    stopped: bool,
}

impl Drop for StoppedWorld {
    fn drop(&mut self) {
        if !self.requested {
            return;
        }
        let mut world = MUTATOR_WORLD.state.lock().unwrap();
        world.stop_requested = false;
        world.collector = 0;
        GC_STOP_REQUESTED.store(false, Ordering::Release);
        MUTATOR_WORLD.changed.notify_all();
    }
}

/// The blocking places a thread can be, for the straggler report.
pub const SITE_SAFEPOINT_WORLD_LOCK: u64 = 1;
pub const SITE_ENTER_BLOCKING: u64 = 2;
pub const SITE_LEAVE_BLOCKING: u64 = 3;
pub const SITE_LOCK_INNER: u64 = 4;
pub const SITE_LOCK_CONDVAR: u64 = 5;
pub const SITE_TLAB_REFILL: u64 = 6;
/// Written by the scheduler's worker loop, compiled only where the pool has
/// OS threads. The name stays in `SITE_NAMES` so numbering matches.
#[cfg_attr(
    not(any(not(target_family = "wasm"), target_feature = "atomics")),
    allow(dead_code)
)]
pub const SITE_SCHEDULER_IDLE: u64 = 7;
pub const SITE_RUNNING: u64 = 0;

const SITE_NAMES: [&str; 8] = [
    "running",
    "safepoint-world-lock",
    "enter-blocking",
    "leave-blocking",
    "gclock-inner",
    "gclock-condvar",
    "tlab-refill",
    "scheduler-idle",
];

/// Record that this thread is entering (or has left) a place it can wait.
#[inline]
pub fn mark_site(site: u64) {
    TLAB.with(|t| t.site.store(site, Ordering::Relaxed));
}

fn read_site(at: usize) -> u64 {
    if at == 0 {
        return 0;
    }
    unsafe { (*(at as *const AtomicU64)).load(Ordering::Relaxed) }
}

fn read_polls(at: usize) -> u64 {
    if at == 0 {
        return 0;
    }
    unsafe { (*(at as *const AtomicU64)).load(Ordering::Relaxed) }
}

/// The scheduler's "poll now" signal, installed by [`set_poll_request_hook`];
/// zero when none is installed. Fibers running compiled code reach a
/// safepoint only when their poll epoch moves, and the epoch is the
/// scheduler's.
static POLL_REQUEST_HOOK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Install the scheduler's poll request; the collector calls `f` on every
/// stop request.
pub fn set_poll_request_hook(f: fn()) {
    POLL_REQUEST_HOOK.store(f as usize, Ordering::Release);
}

/// Ask every fiber to poll through the hook; a no-op when none is installed.
fn request_fiber_poll() {
    let raw = POLL_REQUEST_HOOK.load(Ordering::Acquire);
    if raw != 0 {
        // SAFETY: only `set_poll_request_hook` writes a non-zero value, and
        // it writes a `fn()`.
        let f = unsafe { mem::transmute::<usize, fn()>(raw) };
        f();
    }
}

fn stop_mutator_world() -> StoppedWorld {
    let collector = thread_self_fast();
    let mut world = MUTATOR_WORLD.state.lock().unwrap();
    let needs_stop = world.mutators.iter().any(|m| m.thread != collector);
    if needs_stop {
        world.stop_requested = true;
        world.collector = collector;
        GC_STOP_ASKED_NS.store(GC_EPOCH.elapsed().as_nanos() as u64, Ordering::Relaxed);
        GC_STOP_REQUESTED.store(true, Ordering::Release);
        for record in world.mutators.iter_mut() {
            record.polls_at_stop = read_polls(record.polls);
        }
        request_fiber_poll();
        // A mutator may already be sleeping in the GC-lock slow path. Wake it
        // so it can observe the stop request and publish its stack.
        GC_LOCK.wake_for_world_stop();
        // Bounded waits, so a slow mutator can be named.
        let began = Instant::now();
        let mut reported = false;
        let mut abandoned = false;
        while world
            .mutators
            .iter()
            .any(|m| m.thread != collector && !m.parked && m.blocking_depth == 0)
        {
            // A thread in a native call that never announced itself as blocking
            // reaches no safepoint until the call returns. It cannot be scanned while
            // running, so the only safe answer is to not collect.
            if began.elapsed() >= STOP_THE_WORLD_DEADLINE {
                abandoned = true;
                break;
            }
            let (guard, timed_out) = MUTATOR_WORLD
                .changed
                .wait_timeout(world, std::time::Duration::from_millis(20))
                .unwrap();
            world = guard;
            if timed_out.timed_out() && !reported && gc_stats_enabled() {
                reported = true;
                let stragglers: Vec<String> = world
                    .mutators
                    .iter()
                    .filter(|m| m.thread != collector && !m.parked && m.blocking_depth == 0)
                    .map(|m| format!("{} {:#x}", m.role, m.thread))
                    .collect();
                eprintln!(
                    "[gc] world stop waiting {:.1}ms on {} of {} mutators: {}",
                    began.elapsed().as_secs_f64() * 1e3,
                    stragglers.len(),
                    world.mutators.len(),
                    stragglers.join(", ")
                );
            }
        }
        if reported {
            eprintln!(
                "[gc] world stopped after {:.1}ms",
                began.elapsed().as_secs_f64() * 1e3
            );
        }
        if abandoned {
            let stragglers: Vec<String> = world
                .mutators
                .iter()
                .filter(|m| m.thread != collector && !m.parked && m.blocking_depth == 0)
                .map(|m| format!("{} {:#x}", m.role, m.thread))
                .collect();
            // The whole table, not just who is late: two records sharing a thread id,
            // or a straggler already marked blocking, are told apart here.
            if gc_stats_enabled() {
                let table: Vec<String> = world
                    .mutators
                    .iter()
                    .map(|m| {
                        format!(
                            "{} {:#x}{}{}{}",
                            m.role,
                            m.thread,
                            if m.thread == collector {
                                " collector"
                            } else {
                                ""
                            },
                            if m.parked { " parked" } else { "" },
                            if m.blocking_depth != 0 {
                                format!(" blocking={}", m.blocking_depth)
                            } else {
                                format!(
                                    " polls={} at={}",
                                    read_polls(m.polls).saturating_sub(m.polls_at_stop),
                                    SITE_NAMES[(read_site(m.site) as usize).min(7)]
                                )
                            },
                        )
                    })
                    .collect();
                eprintln!("[gc] mutators: {}", table.join(" | "));
            }
            GC_STATS.stops_abandoned.fetch_add(1, Ordering::Relaxed);
            // Once by default: a program that does this does it repeatedly,
            // and a line per collection would bury everything else.
            static SAID: std::sync::Once = std::sync::Once::new();
            let mut first = false;
            SAID.call_once(|| first = true);
            if first || gc_stats_enabled() {
                eprintln!(
                    "[gc] gave up stopping the world after {:.0}ms; {} of {} mutators \
                     never reached a safepoint ({}). Collection deferred. A native call \
                     that blocks without calling hl_blocking looks exactly like this.",
                    began.elapsed().as_secs_f64() * 1e3,
                    stragglers.len(),
                    world.mutators.len(),
                    stragglers.join(", ")
                );
            }
            return StoppedWorld {
                snapshots: Vec::new(),
                requested: needs_stop,
                stopped: false,
            };
        }
    }
    let snapshots = world
        .mutators
        .iter()
        .map(|m| MutatorSnapshot {
            thread: m.thread,
            stack_top: m.stack_top,
            stack_sp: m.stopped_sp,
            saved_regs: m.saved_regs,
            scan_ranges: match m.scan_live {
                // SAFETY: the mutator is stopped, and it owns this table for
                // as long as it is registered. A stop only lands at a
                // safepoint, never between an entry's write and the length
                // bump that publishes it.
                Some((ranges, len)) if ranges != 0 && len != 0 => unsafe {
                    let n = *(len as *const usize);
                    std::slice::from_raw_parts(ranges as *const (usize, usize), n)
                        .iter()
                        .copied()
                        .filter(|&(a, sz)| a != 0 && sz != 0)
                        .collect()
                },
                _ => m.scan_ranges.clone(),
            },
        })
        .collect();
    StoppedWorld {
        snapshots,
        requested: needs_stop,
        stopped: true,
    }
}

/// Copy the registered mutator thread handles into `out`, returning how many
/// were written. `thread_self_fast` matches `pthread_self`, so the sampling
/// profiler can signal them directly.
///
/// `try_lock`: the sampler must never block on the world lock.
///
/// # Safety
/// `out` must be valid for `cap` `u64` writes.
pub unsafe fn registered_threads(out: *mut u64, cap: usize) -> usize {
    if out.is_null() || cap == 0 {
        return 0;
    }
    let Ok(world) = MUTATOR_WORLD.state.try_lock() else {
        return 0;
    };
    let n = world.mutators.len().min(cap);
    for (i, m) in world.mutators.iter().take(n).enumerate() {
        unsafe { *out.add(i) = m.thread };
    }
    n
}

fn mutator_scan_range_count() -> usize {
    MUTATOR_WORLD
        .state
        .lock()
        .unwrap()
        .mutators
        .iter()
        .map(|m| m.scan_ranges.len())
        .sum()
}

fn clear_current_scan_ranges() {
    let thread = thread_self_fast();
    let mut world = MUTATOR_WORLD.state.lock().unwrap();
    if let Some(record) = world.mutators.iter_mut().find(|m| m.thread == thread) {
        record.staged_scan_ranges.clear();
    }
}

fn add_current_scan_range(start: usize, size: usize) {
    if start == 0 || size == 0 {
        return;
    }
    let thread = thread_self_fast();
    let mut world = MUTATOR_WORLD.state.lock().unwrap();
    if let Some(record) = world.mutators.iter_mut().find(|m| m.thread == thread) {
        record.staged_scan_ranges.push((start, size));
    }
}

fn publish_current_scan_ranges() {
    let thread = thread_self_fast();
    let mut world = MUTATOR_WORLD.state.lock().unwrap();
    if let Some(record) = world.mutators.iter_mut().find(|m| m.thread == thread) {
        record.scan_ranges = mem::take(&mut record.staged_scan_ranges);
    }
}

/// Replace this mutator's published scan set in one hold of the world lock.
/// Built in place: nothing reads `scan_ranges` without this mutex, and
/// `staged_scan_ranges` must stay empty after a publish so a later stray
/// `scan_roots_done` cannot republish freed frame buffers as roots.
fn set_current_scan_ranges(ranges: &[(usize, usize)]) {
    let thread = thread_self_fast();
    let mut world = MUTATOR_WORLD.state.lock().unwrap();
    if let Some(record) = world.mutators.iter_mut().find(|m| m.thread == thread) {
        record.scan_ranges.clear();
        record
            .scan_ranges
            .extend(ranges.iter().copied().filter(|&(a, s)| a != 0 && s != 0));
    }
}

/// TLAB on? `ASH_GC_TLAB=0` turns it off; stress mode does too. Safe either
/// way.
fn tlab_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| {
        gc_stress_every() == 0
            && !matches!(std::env::var("ASH_GC_TLAB").as_deref(), Ok("0") | Ok("off"))
    })
}

/// The allocation entry point: zeroed memory from the mutator's bump region
/// when it can, the locked path when it cannot.
pub fn gc_alloc(size: usize) -> Option<NonNull<u8>> {
    let aligned = (size.max(8) + 15) & !15;
    if aligned <= TLAB_MAX_OBJ && tlab_enabled() {
        // One TLS lookup for the whole sequence -- see `TLAB`.
        enum Step {
            Bumped(usize),
            Refill,
            Unregistered,
        }
        let step = TLAB.with(|t| {
            if !t.registered.get() {
                return Step::Unregistered;
            }
            let cur = t.cur.get();
            if cur != 0 {
                let mut p = cur;
                if (p & (LINE_SIZE - 1)) + aligned > LINE_SIZE {
                    p = (p + LINE_SIZE - 1) & !(LINE_SIZE - 1);
                }
                let np = p + aligned;
                if np <= t.limit.get() {
                    unsafe {
                        (*t.objects.get().add((p - t.heap_base.get()) / ALLOC_QUANTUM))
                            .store((aligned / ALLOC_QUANTUM) as u8, Ordering::Relaxed);
                    }
                    t.cur.set(np);
                    return Step::Bumped(p);
                }
            }
            Step::Refill
        });
        match step {
            // Pre-zeroed at refill.
            Step::Bumped(p) => return Some(unsafe { NonNull::new_unchecked(p as *mut u8) }),
            Step::Refill => return tlab_refill_then_alloc(aligned),
            Step::Unregistered => {}
        }
    }
    gc_locked_init().allocate(size)
}

/// Install `block` as this thread's bump region, releasing the previous one.
/// The set of in-use regions lives on the heap because `sweep` consults it
/// under the same lock; only the cursor is thread-local.
fn adopt_tlab_region(gc: &mut ImmixAllocator, block: usize, cur: usize, limit: usize) {
    gc.heap.tlab_blocks.insert(thread_self_fast(), block);
    TLAB.with(|t| {
        t.objects.set(gc.heap.objects.as_ptr());
        t.heap_base.set(gc.heap.memory.as_ptr() as usize);
        t.block.set(block);
        t.cur.set(cur);
        t.limit.set(limit);
    });
}

/// Give up this thread's bump region entirely (thread exit).
fn release_tlab_region(gc: &mut ImmixAllocator) {
    gc.heap.tlab_blocks.remove(&thread_self_fast());
    TLAB.with(|t| {
        t.block.set(usize::MAX);
        t.cur.set(0);
        t.limit.set(0);
    });
}

#[cold]
fn tlab_refill_then_alloc(aligned: usize) -> Option<NonNull<u8>> {
    mark_site(SITE_TLAB_REFILL);
    let mut gc = gc_locked();
    // A refill is a true safepoint, so a due trigger collects here instead of
    // deferring to the interpreter's next snapshot: same thread, conservative
    // stack scan, and the registered ranges are complete as of their last sync.
    set_collect_origin(2);
    gc.maybe_collect_at_safepoint();
    // Recycled lines first. Spans too small for the pending object are dropped
    // rather than re-queued; the list is rebuilt each sweep.
    let want_lines = aligned.div_ceil(LINE_SIZE).max(1);
    if recycle_lines() {
        while let Some((rblock, start, len)) = gc.heap.recycle_spans.pop() {
            if len < want_lines {
                continue;
            }
            let lo = rblock + start * LINE_SIZE;
            let span_bytes = len * LINE_SIZE;
            gc.clear_allocation_metadata(lo, span_bytes);
            let base = unsafe { gc.heap.memory.as_mut_ptr().add(lo) };
            // Zeroed for the same reason a fresh block is: every caller of
            // this path is promised zeroed memory.
            unsafe { std::ptr::write_bytes(base, 0, span_bytes) };
            gc.heap.bytes_since_gc += span_bytes;
            gc.heap.alloc_count += 1;
            GC_STATS
                .bytes_allocated
                .fetch_add(span_bytes as u64, Ordering::Relaxed);
            GC_STATS
                .lines_recycled
                .fetch_add(len as u64, Ordering::Relaxed);
            adopt_tlab_region(
                &mut gc,
                rblock,
                base as usize + aligned,
                base as usize + span_bytes,
            );
            gc.record_allocation(lo, aligned);
            return Some(unsafe { NonNull::new_unchecked(base) });
        }
    }

    let block = match gc.acquire_free_block() {
        Some(b) => b,
        None => {
            set_collect_origin(4);
            gc.collect_garbage();
            gc.acquire_free_block()?
        }
    };
    let base = unsafe { gc.heap.memory.as_mut_ptr().add(block) };
    unsafe { std::ptr::write_bytes(base, 0, BLOCK_SIZE) };
    // Coarse trigger accounting: the whole region counts when it is carved,
    // not per object. Slightly early triggers, never late ones.
    gc.heap.bytes_since_gc += BLOCK_SIZE;
    gc.heap.alloc_count += 1;
    GC_STATS
        .bytes_allocated
        .fetch_add(BLOCK_SIZE as u64, Ordering::Relaxed);
    adopt_tlab_region(
        &mut gc,
        block,
        base as usize + aligned,
        base as usize + BLOCK_SIZE,
    );
    gc.record_allocation(block, aligned);
    Some(unsafe { NonNull::new_unchecked(base) })
}

/// Trace flags, read once.
fn trace_alloc() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("ASH_GC_TRACE_ALLOC").is_ok())
}
fn trace_map() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("ASH_GC_TRACE_MAP").is_ok())
}

/// How much new allocation to allow per unit of live data before collecting.
/// Proportional, so a small live set still collects at a small interval; the
/// trade is collection count against pause length. `ASH_GC_GROWTH` overrides
/// it (a positive integer; safe).
fn growth_factor() -> usize {
    static V: OnceLock<usize> = OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("ASH_GC_GROWTH")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(4)
    })
}

/// Reuse the unmarked lines inside blocks a sweep kept; `ASH_GC_RECYCLE=0`
/// retains those blocks whole instead. Sound only while the trace marks every
/// live object, since a reused line is overwritten where block retention
/// would hide a missed root. Prove a corpus clean under `ASH_GC_STRESS`
/// before trusting it there.
fn recycle_lines() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| !matches!(std::env::var("ASH_GC_RECYCLE").as_deref(), Ok("0")))
}

/// `ASH_GC_HANDBACK=0` stops returning free blocks to the OS; RSS then holds
/// instead of falling. Safe.
fn handback_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| !matches!(std::env::var("ASH_GC_HANDBACK").as_deref(), Ok("0")))
}

/// `ASH_GC_OCCUPANCY=1`: per-collection report of how full the retained
/// blocks are; a decaying series is fragmentation. Safe.
fn occupancy_stats() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("ASH_GC_OCCUPANCY").is_ok())
}

/// `ASH_GC_NO_RECLAIM=1`: sweep retains every block (marks still reset).
/// Diagnostic; the heap only grows.
fn no_reclaim() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| matches!(std::env::var("ASH_GC_NO_RECLAIM").as_deref(), Ok("1")))
}

/// `ASH_GC_SWEEP_AUDIT=1`: check, for every block about to be freed, that no
/// root still points into it. Diagnostic; slow. Cached because the
/// freed-block branch consults it once per block inside the stop.
fn sweep_audit() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("ASH_GC_SWEEP_AUDIT").is_ok())
}

fn trace_freed() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("ASH_GC_TRACE_FREED").is_ok())
}
/// `ASH_GC_POISON=1`: fill every freed block with 0xA5, so a read of a
/// prematurely freed object is unmistakable. Diagnostic.
fn poison_freed() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("ASH_GC_POISON").is_ok())
}
/// `ASH_GC_QUARANTINE=1`: freed blocks are poisoned and never reused.
/// Diagnostic; the heap only grows, so pair it with a large `ASH_GC_HEAP_MB`.
fn quarantine_freed() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("ASH_GC_QUARANTINE").is_ok())
}
/// Why the current collection was started; set by every `collect_garbage`
/// caller just before the call, printed by the trace lines. A plain static
/// is sound under the GC lock.
static COLLECT_ORIGIN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
const ORIGIN_NAMES: [&str; 7] = [
    "?",
    "snapshot-done",    // scan_roots_done honoring a deferred trigger
    "tlab-safepoint",   // tlab_refill_then_alloc's maybe_collect_at_safepoint
    "hard-pressure",    // maybe_collect past the 4x deferral bound
    "exhaustion",       // allocate's no-free-block backstop
    "large-exhaustion", // allocate_large fallback
    "explicit",         // Gc.major / hlp_gc_major
];
fn set_collect_origin(o: u8) {
    COLLECT_ORIGIN.store(o, Ordering::Relaxed);
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

/// Report an allocation that could not be satisfied, and stop. Callers are
/// behind an `extern "C"` boundary where a panic cannot unwind, so say what
/// was being allocated and how full the heap was, then exit.
#[cold]
#[inline(never)]
pub fn out_of_memory(what: &str) -> ! {
    const MB: usize = 1024 * 1024;
    let live = GC_STATS.live_blocks.load(Ordering::Relaxed) as usize * BLOCK_SIZE;
    let external = GC_STATS.external_bytes.load(Ordering::Relaxed) as usize;
    let collections = GC_STATS.collections.load(Ordering::Relaxed);
    eprintln!(
        "[ash] out of memory allocating {what}\n\
         [ash]   heap cap {} MB, live {} MB, external {} MB, after {collections} collection(s)\n\
         [ash]   ASH_GC_HEAP_MB raises the cap. A heap that fills again at a\n\
         [ash]   higher cap is a leak rather than a heap that is too small.",
        heap_max_bytes() / MB,
        live / MB,
        external / MB,
    );
    // Not a panic: see above.
    std::process::exit(1);
}

fn heap_max_bytes() -> usize {
    static V: OnceLock<usize> = OnceLock::new();
    *V.get_or_init(|| {
        let bytes = match env_usize("ASH_GC_HEAP_MB") {
            Some(mb) => mb.max(32) * 1024 * 1024,
            None => (usable_ram_bytes() / HEAP_MAX_SHARE).clamp(HEAP_MAX_FLOOR, HEAP_MAX_CEILING),
        };
        (bytes / BLOCK_SIZE) * BLOCK_SIZE
    })
}

/// Adaptive-trigger floor in bytes (ASH_GC_TRIGGER_MB overrides).
fn trigger_floor_bytes() -> usize {
    static V: OnceLock<usize> = OnceLock::new();
    *V.get_or_init(|| {
        env_usize("ASH_GC_TRIGGER_MB")
            .map(|mb| (mb * 1024 * 1024).max(1024 * 1024))
            .unwrap_or(DEFAULT_TRIGGER_FLOOR)
    })
}

/// `ASH_GC_STATS=1`: per-collection trace lines and an end-of-run summary.
/// Safe.
fn gc_stats_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("ASH_GC_STATS")
            .map(|v| v != "0" && !v.is_empty())
            .unwrap_or(false)
    })
}

/// `ASH_GC_STRESS=N`: collect at every Nth allocation (1 = every one; 0 or
/// unset = off). Validation only; very slow.
fn gc_stress_every() -> usize {
    static V: OnceLock<usize> = OnceLock::new();
    *V.get_or_init(|| match std::env::var("ASH_GC_STRESS") {
        Ok(v) if v == "0" || v.is_empty() => 0,
        Ok(v) => v.trim().parse().unwrap_or(1),
        Err(_) => 0,
    })
}

// ── GC statistics (atomics — readable from atexit or any thread without
// the GC lock) ─────────────────────────────────────────────────────────────

struct GcStatCounters {
    collections: AtomicU64,
    blocks_reclaimed: AtomicU64,
    /// Lines served from a kept block's free spans rather than a fresh block.
    lines_recycled: AtomicU64,
    bytes_allocated: AtomicU64,
    external_bytes: AtomicU64,
    live_blocks: AtomicU64,
    pause_ns_total: AtomicU64,
    pause_ns_max: AtomicU64,
    /// Collections given up because a mutator never reached a safepoint.
    stops_abandoned: AtomicU64,
}

static GC_STATS: GcStatCounters = GcStatCounters {
    collections: AtomicU64::new(0),
    blocks_reclaimed: AtomicU64::new(0),
    lines_recycled: AtomicU64::new(0),
    bytes_allocated: AtomicU64::new(0),
    external_bytes: AtomicU64::new(0),
    live_blocks: AtomicU64::new(0),
    pause_ns_total: AtomicU64::new(0),
    pause_ns_max: AtomicU64::new(0),
    stops_abandoned: AtomicU64::new(0),
};

// ── Collection switch (`Gc.enable`) ─────────────────────────────────────────

/// Upstream's `gc_is_active`: consulted by the automatic trigger only, so
/// disabling never turns an allocation into a hard failure. An atomic because
/// `enable` is reached from anywhere, including under the GC lock.
static GC_ENABLED: AtomicBool = AtomicBool::new(true);

/// The singleton's trigger, rebased onto the cumulative counters so
/// [`should_collect`] needs no lock: the `bytes_allocated + external_bytes`
/// at which its next automatic collection is due, and `bytes_allocated` at
/// its last collection. Written under the lock by the collection that sets
/// `trigger_threshold`.
static NEXT_TRIGGER_AT: AtomicU64 = AtomicU64::new(INITIAL_TRIGGER_BYTES as u64);
static ALLOCATED_AT_COLLECT: AtomicU64 = AtomicU64::new(0);
/// The singleton's `collect_pending`, for [`collect_pending`]'s lock-free
/// read. Written under the lock beside the field.
static COLLECT_PENDING: AtomicBool = AtomicBool::new(false);

// ── Collector flags (`Gc.flags`) ────────────────────────────────────────────

/// Bit values of `hl.Gc.GcFlag`, fixed by the Haxe enum's ordinals.
const GC_FLAG_PROFILE: i32 = 1;

/// Upstream's `gc_flags`. Stored whole, so a read-modify-write round-trips
/// even for bits this collector does not act on.
static GC_FLAGS: AtomicI32 = AtomicI32::new(0);

/// True when `flag` is currently set. Cheap enough for the allocation path.
#[inline]
fn gc_flag(flag: i32) -> bool {
    GC_FLAGS.load(Ordering::Relaxed) & flag != 0
}

/// Pressure at which a disabled collector collects anyway: a
/// `Gc.enable(false)` with no matching re-enable must not turn into an
/// out-of-memory abort.
const TRIGGER_CEILING_SHARE: usize = 32;

/// Usable memory for this process: the cgroup limit when there is one, else
/// physical memory, else a modest guess.
fn usable_ram_bytes() -> usize {
    const FALLBACK: usize = 2 * 1024 * 1024 * 1024;
    #[cfg(target_os = "linux")]
    {
        // The limit that binds is this process's cgroup, named by
        // `/proc/self/cgroup`; limits are hierarchical, so the smallest along the
        // path wins.
        let mut limit = usize::MAX;
        if let Ok(selfcg) = std::fs::read_to_string("/proc/self/cgroup") {
            for line in selfcg.lines() {
                // v2: "0::/path". v1: "N:controllers:/path".
                let rel = match line.splitn(3, ':').nth(2) {
                    Some(r) => r.trim_start_matches('/'),
                    None => continue,
                };
                let mut dir = std::path::PathBuf::from("/sys/fs/cgroup");
                let mut probe = vec![dir.clone()];
                for seg in rel.split('/').filter(|s| !s.is_empty()) {
                    dir = dir.join(seg);
                    probe.push(dir.clone());
                }
                for d in probe {
                    for name in ["memory.max", "memory/memory.limit_in_bytes"] {
                        if let Ok(t) = std::fs::read_to_string(d.join(name)) {
                            if let Ok(n) = t.trim().parse::<usize>() {
                                // v1 uses a sentinel near usize::MAX for
                                // "no limit"; v2 writes the word "max",
                                // which fails the parse and is skipped.
                                if n > 0 && n < (1 << 60) {
                                    limit = limit.min(n);
                                }
                            }
                        }
                    }
                }
            }
        }
        if limit != usize::MAX {
            return limit;
        }
        if let Ok(t) = std::fs::read_to_string("/proc/meminfo") {
            for line in t.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    if let Some(kb) = rest.split_whitespace().next() {
                        if let Ok(kb) = kb.parse::<usize>() {
                            return kb * 1024;
                        }
                    }
                }
            }
        }
        FALLBACK
    }
    #[cfg(target_os = "macos")]
    {
        let mut out: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        let name = c"hw.memsize";
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                &mut out as *mut u64 as *mut c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 && out > 0 {
            out as usize
        } else {
            FALLBACK
        }
    }
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 && status.ullTotalPhys > 0 {
            return status.ullTotalPhys as usize;
        }
        FALLBACK
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        FALLBACK
    }
}

/// How much new allocation the collector allows between collections, at
/// most, derived from the machine; `ASH_GC_TRIGGER_MB` overrides it. Bounds
/// fixed headroom only: `collect_garbage` raises the effective ceiling to the
/// live set, so a large live set does not collect continuously.
fn trigger_ceiling_bytes() -> usize {
    static V: OnceLock<usize> = OnceLock::new();
    *V.get_or_init(|| {
        (usable_ram_bytes() / TRIGGER_CEILING_SHARE).clamp(TRIGGER_CEILING_MIN, TRIGGER_CEILING_MAX)
    })
}

/// Never defer a collection past this much pressure: running out of heap is
/// not recoverable, since the allocator's callers cannot unwind.
fn max_deferred_pressure() -> usize {
    heap_max_bytes() / 2
}

/// A GC disabled by the embedder still collects under this much pressure.
fn gc_disabled_max_pressure() -> usize {
    trigger_ceiling_bytes()
        .saturating_mul(4)
        .min(max_deferred_pressure())
}

/// Is a *triggered* collection allowed to run right now? `pressure` is the
/// byte total the trigger fired on.
fn triggered_collection_allowed(pressure: usize) -> bool {
    GC_ENABLED.load(Ordering::Relaxed) || pressure >= gc_disabled_max_pressure()
}

fn fmt_mb(bytes: u64) -> String {
    format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
}

fn print_gc_stats_report() {
    let n = GC_STATS.collections.load(Ordering::Relaxed);
    let freed = GC_STATS.blocks_reclaimed.load(Ordering::Relaxed);
    let alloc = GC_STATS.bytes_allocated.load(Ordering::Relaxed);
    let ext = GC_STATS.external_bytes.load(Ordering::Relaxed);
    let live = GC_STATS.live_blocks.load(Ordering::Relaxed);
    let pt = GC_STATS.pause_ns_total.load(Ordering::Relaxed);
    let pm = GC_STATS.pause_ns_max.load(Ordering::Relaxed);
    eprintln!("[gc] ---- GC stats ----");
    eprintln!(
        "[gc] sizing:           ram {} / ceiling {} / growth x{}",
        fmt_mb(usable_ram_bytes() as u64),
        fmt_mb(trigger_ceiling_bytes() as u64),
        growth_factor()
    );
    eprintln!("[gc] collections:      {}", n);
    // Any value but zero is a collection that did not happen because a mutator
    // would not stop.
    let abandoned = GC_STATS.stops_abandoned.load(Ordering::Relaxed);
    if abandoned > 0 {
        eprintln!("[gc] stops abandoned:  {abandoned}  <-- collections discarded");
    }
    eprintln!(
        "[gc] blocks reclaimed: {} ({})",
        freed,
        fmt_mb(freed * BLOCK_SIZE as u64)
    );
    let recycled = GC_STATS.lines_recycled.load(Ordering::Relaxed);
    if recycled > 0 {
        eprintln!(
            "[gc] lines recycled:   {} ({})",
            recycled,
            fmt_mb(recycled * LINE_SIZE as u64)
        );
    }
    eprintln!(
        "[gc] bytes allocated:  {} (+ external {})",
        fmt_mb(alloc),
        fmt_mb(ext)
    );
    eprintln!(
        "[gc] live at last gc:  {} blocks ({})",
        live,
        fmt_mb(live * BLOCK_SIZE as u64)
    );
    eprintln!(
        "[gc] pause total:      {:.1}ms, max {:.2}ms, total {}ns",
        pt as f64 / 1e6,
        pm as f64 / 1e6,
        pt
    );
}

extern "C" fn gc_stats_atexit() {
    print_gc_stats_report();
}

/// The C runtime's `atexit`. Only unix links the `libc` crate; the CRT and
/// wasi-libc provide it elsewhere.
#[cfg(unix)]
use libc::atexit;
#[cfg(not(unix))]
unsafe extern "C" {
    fn atexit(callback: extern "C" fn()) -> std::os::raw::c_int;
}

/// On-demand GC stats dump (also printed at exit when ASH_GC_STATS=1).
pub fn print_stats() {
    print_gc_stats_report();
}

/// The report the `atexit` handler prints, for a caller that exits without
/// running them. A no-op unless `ASH_GC_STATS` is set.
pub fn print_stats_if_enabled() {
    if gc_stats_enabled() {
        print_gc_stats_report();
    }
}

// ── macOS return-to-OS hooks ────────────────────────────────────────────────

#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// Asks all malloc zones to release free pages back to the OS.
    fn malloc_zone_pressure_relief(zone: *mut c_void, goal: usize) -> usize;
}

/// Demand-committed heap reservation: pages become resident on first touch,
/// and fully free blocks go back via madvise, so RSS tracks live data rather
/// than configured capacity.
struct HeapMemory {
    base: *mut u8,
    len: usize,
}

impl HeapMemory {
    #[cfg(unix)]
    fn new(len: usize) -> Self {
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        assert!(
            ptr != libc::MAP_FAILED,
            "GC heap reservation failed ({} bytes)",
            len
        );
        HeapMemory {
            base: ptr as *mut u8,
            len,
        }
    }

    /// MEM_RESERVE|MEM_COMMIT: committed pages are demand-zeroed, so none is
    /// resident until touched. Windows charges the whole reservation against the
    /// commit limit up front.
    #[cfg(windows)]
    fn new(len: usize) -> Self {
        let ptr = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                len,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        assert!(!ptr.is_null(), "GC heap reservation failed ({} bytes)", len);
        HeapMemory {
            base: ptr as *mut u8,
            len,
        }
    }

    #[inline(always)]
    fn as_ptr(&self) -> *const u8 {
        self.base
    }

    #[inline(always)]
    fn as_mut_ptr(&self) -> *mut u8 {
        self.base
    }
}

impl HeapMemory {
    /// The heap on a target whose memory is one linear address space: no `mmap`
    /// and no way to hand pages back, so the whole region is taken up front.
    #[cfg(not(any(unix, windows)))]
    fn new(len: usize) -> Self {
        // Block-aligned, and the alignment is load-bearing: the bump allocator finds
        // a line boundary from the absolute address while marking and sweeping index
        // lines from the offset into the heap, and those agree only if the base is a
        // multiple of LINE_SIZE.
        let layout = std::alloc::Layout::from_size_align(len, BLOCK_SIZE).expect("heap layout");
        // Zeroed, because the collector reads a block's header before
        // anything has written one.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "could not reserve {len} bytes for the heap");
        Self { base: ptr, len }
    }
}

impl Drop for HeapMemory {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::munmap(self.base as *mut c_void, self.len);
        }
        // MEM_RELEASE demands a zero size and the exact base VirtualAlloc returned.
        #[cfg(windows)]
        unsafe {
            VirtualFree(self.base as *mut c_void, 0, MEM_RELEASE);
        }
        #[cfg(not(any(unix, windows)))]
        unsafe {
            let layout = std::alloc::Layout::from_size_align_unchecked(self.len, 16);
            std::alloc::dealloc(self.base, layout);
        }
    }
}

pub static mut GC: OnceLock<ImmixAllocator> = OnceLock::new();
pub static HL_GLOBAL_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();

// ── Reentrant GC lock ───────────────────────────────────────────────────────
//
// The GC singleton is not thread-safe and is reached from several threads:
// every entry point that touches GC state holds this lock. Reentrant because
// GC operations nest on one thread (allocate → collect_garbage; a worker
// holding the lock across its init, which allocates).
//
// Owner+depth: `owner` is a per-thread token (0 = free). The owning thread
// bumps `depth`; others wait on `inner`/`cond` until `owner` is 0. `inner` is
// always held while `owner` transitions between 0 and non-zero, which gives
// every cross-thread happens-before edge.

struct ReentrantGcLock {
    owner: std::sync::atomic::AtomicU64,
    depth: std::sync::atomic::AtomicUsize,
    /// Threads inside the slow acquire path; the uncontended release skips the
    /// mutex and broadcast when it is zero.
    waiters: std::sync::atomic::AtomicUsize,
    inner: std::sync::Mutex<()>,
    cond: std::sync::Condvar,
}

static GC_LOCK: ReentrantGcLock = ReentrantGcLock {
    owner: std::sync::atomic::AtomicU64::new(0),
    depth: std::sync::atomic::AtomicUsize::new(0),
    waiters: std::sync::atomic::AtomicUsize::new(0),
    inner: std::sync::Mutex::new(()),
    cond: std::sync::Condvar::new(),
};

/// Unique, never-zero token for the current thread: `thread_self_fast`, a
/// register read on every supported platform, so the "0 means free" encoding
/// in `owner` holds.
#[inline(always)]
fn gc_thread_token() -> u64 {
    thread_self_fast()
}

#[allow(dead_code)]
fn gc_thread_token_unused() -> u64 {
    #[cfg(not(any(unix, windows)))]
    {
        1
    }
    #[cfg(unix)]
    unsafe {
        libc::pthread_self() as u64
    }
    #[cfg(windows)]
    unsafe {
        GetCurrentThreadId() as u64
    }
}

impl ReentrantGcLock {
    fn acquire(&self) {
        use std::sync::atomic::Ordering;
        let me = gc_thread_token();
        // Fast path: we already own the lock — only this thread can have
        // stored `me` into owner, so a plain load is sufficient.
        if self.owner.load(Ordering::Relaxed) == me {
            self.depth.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // A collector can own this lock while it waits for our stack. Park
        // before attempting the CAS; otherwise both sides wait forever.
        gc_safepoint();
        // Uncontended path: one CAS, no mutex.
        if self
            .owner
            .compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            self.depth.store(1, Ordering::Relaxed);
            mark_site(SITE_RUNNING);
            return;
        }
        // Contended: register as a waiter (SeqCst pairs with release's
        // owner-store/waiters-load — see the comment there), then sleep.
        self.waiters.fetch_add(1, Ordering::SeqCst);
        mark_site(SITE_LOCK_INNER);
        let mut g = self.inner.lock().unwrap();
        while self
            .owner
            .compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            if GC_STOP_REQUESTED.load(Ordering::Acquire) {
                drop(g);
                gc_safepoint();
                g = self.inner.lock().unwrap();
            } else {
                mark_site(SITE_LOCK_CONDVAR);
                g = self.cond.wait(g).unwrap();
            }
        }
        self.waiters.fetch_sub(1, Ordering::Relaxed);
        self.depth.store(1, Ordering::Relaxed);
        mark_site(SITE_RUNNING);
        drop(g);
    }

    /// Returns true if this dropped the last hold, leaving the lock free.
    fn release(&self) -> bool {
        use std::sync::atomic::Ordering;
        // The token serves only the assert; keep the call out of release builds.
        #[cfg(debug_assertions)]
        {
            let me = gc_thread_token();
            debug_assert_eq!(
                self.owner.load(Ordering::Relaxed),
                me,
                "GC lock released by non-owner thread"
            );
        }
        if self.depth.load(Ordering::Relaxed) > 1 {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        self.depth.store(0, Ordering::Relaxed);
        // SeqCst store then SeqCst load: either the releasing thread sees the
        // waiter's `waiters` increment and notifies under the mutex, or the
        // waiter's CAS loop (entered after its increment) sees owner == 0 and
        // takes the lock without needing the wakeup. Both orders are covered,
        // so the wakeup cannot be lost.
        self.owner.store(0, Ordering::SeqCst);
        if self.waiters.load(Ordering::SeqCst) != 0 {
            let _g = self.inner.lock().unwrap();
            self.cond.notify_all();
        }
        true
    }

    /// Depth held by the CURRENT thread (0 if it is not the owner).
    fn held_depth(&self) -> usize {
        use std::sync::atomic::Ordering;
        if self.owner.load(Ordering::Relaxed) == gc_thread_token() {
            self.depth.load(Ordering::Relaxed)
        } else {
            0
        }
    }

    /// Force the current thread's hold depth down to `target`. Used on the
    /// longjmp throw path: guards held by frames being jumped over never run
    /// their Drop, so the thrower restores the depth recorded at trap setup.
    fn unwind_to(&self, target: usize) {
        use std::sync::atomic::Ordering;
        let me = gc_thread_token();
        if self.owner.load(Ordering::Relaxed) != me {
            return;
        }
        if self.depth.load(Ordering::Relaxed) <= target {
            return;
        }
        if target > 0 {
            self.depth.store(target, Ordering::Relaxed);
        } else {
            let g = self.inner.lock().unwrap();
            self.depth.store(0, Ordering::Relaxed);
            self.owner.store(0, Ordering::Relaxed);
            drop(g);
            self.cond.notify_all();
        }
    }

    fn wake_for_world_stop(&self) {
        let _guard = self.inner.lock().unwrap();
        self.cond.notify_all();
    }
}

/// RAII guard for the reentrant GC lock.
pub struct GcGuard(());

impl Drop for GcGuard {
    fn drop(&mut self) {
        gc_lock_release();
    }
}

/// Acquire the reentrant GC lock. Every extern "C" entry point that touches
/// GC state must hold one of these (directly or via `gc_locked()`).
pub fn gc_guard() -> GcGuard {
    GC_LOCK.acquire();
    GcGuard(())
}

/// Lock-holding handle to the GC singleton. Derefs to `ImmixAllocator`;
/// the lock is held until the handle is dropped.
pub struct GcRef {
    gc: *mut ImmixAllocator,
    _guard: GcGuard,
}

impl std::ops::Deref for GcRef {
    type Target = ImmixAllocator;
    fn deref(&self) -> &ImmixAllocator {
        unsafe { &*self.gc }
    }
}

impl std::ops::DerefMut for GcRef {
    fn deref_mut(&mut self) -> &mut ImmixAllocator {
        unsafe { &mut *self.gc }
    }
}

/// Acquire the GC lock and return a handle to the (initialized) singleton.
pub fn gc_locked() -> GcRef {
    let guard = gc_guard();
    let gc =
        unsafe { (*(&raw mut GC)).get_mut().expect("GC not initialized") as *mut ImmixAllocator };
    GcRef { gc, _guard: guard }
}

/// Acquire the GC lock, initializing the singleton if needed.
pub fn gc_locked_init() -> GcRef {
    let guard = gc_guard();
    // Stable spelling of `OnceLock::get_mut_or_init`: initialise through the
    // shared path, then take the exclusive reference. Both run under the GC
    // lock, and `ImmixAllocator::new` never re-enters it, so the second call
    // finds exactly what the first installed.
    let gc = unsafe {
        (*(&raw const GC)).get_or_init(ImmixAllocator::new);
        (*(&raw mut GC)).get_mut().expect("GC initialized above") as *mut ImmixAllocator
    };
    GcRef { gc, _guard: guard }
}

/// Depth of the current thread's hold on the GC lock (0 = not held).
pub fn gc_lock_held_depth() -> usize {
    GC_LOCK.held_depth()
}

/// Restore the current thread's GC-lock depth to `target`, releasing
/// ownership entirely when `target` is 0. Longjmp throw path only.
pub fn gc_lock_unwind_to(target: usize) {
    GC_LOCK.unwind_to(target);
}

/// Manually acquire the GC lock (reentrant), for a caller holding it across a
/// whole init.
pub unsafe fn lock() {
    GC_LOCK.acquire();
}

/// Manually release one level of the GC lock.
pub unsafe fn unlock() {
    gc_lock_release();
}

struct ImmixHeap {
    memory: HeapMemory,
    free_blocks: Vec<usize>,
    used_blocks: HashSet<usize>,
    /// Runs of unmarked lines inside blocks a sweep kept, as
    /// `(block_addr, first_line, line_count)`. Filled only when `recycle_lines()`.
    recycle_spans: Vec<(usize, usize, usize)>,
    allocation_point: usize,
    current_block_end: usize,
    alloc_count: usize,
    /// For each line in the heap, stores the number of lines this allocation
    /// occupies if this is an allocation start, or 0 for continuation lines.
    /// Enables the GC to mark all lines of a multi-line object.
    alloc_sizes: Vec<u32>,
    /// One byte per 16-byte allocation quantum: 0 for no start, 1..8 for a
    /// small allocation's size, SPAN_OBJECT for a line-aligned span, plus the
    /// `OBJECT_KIND_*` bits. The high bit is the object's claim bit. Unlike a
    /// line mark, it never makes an unrelated neighbour reachable. Allocated
    /// once; TLAB pointers stay valid.
    objects: Vec<std::sync::atomic::AtomicU8>,
    /// GC-heap bytes allocated since the last collection.
    bytes_since_gc: usize,
    /// Off-heap bytes charged via `track_external` since the last collection.
    external_since_gc: usize,
    /// Collect when bytes_since_gc + external_since_gc reaches this.
    /// Adaptive: live*2 clamped to [floor, ceiling] after each collection.
    trigger_threshold: usize,
    /// Wall-clock heartbeat anchor.
    last_collect: Instant,
    /// Throttle anchor for malloc_zone_pressure_relief.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))] // macOS-only mechanism
    last_pressure_relief: Instant,
    /// Blocks currently madvised MADV_FREE_REUSABLE; must be MADV_FREE_REUSE'd
    /// before reuse so live data can't be discarded under memory pressure.
    reusable_blocks: HashSet<usize>,
    /// Which block each thread is bumping through; `sweep` never reclaims one.
    /// Keyed by thread because the recycled-span path can hand two threads spans
    /// of the same block.
    tlab_blocks: HashMap<u64, usize>,
    /// True once the interpreter has registered scan ranges. Its snapshot is
    /// complete only at publication, so byte-driven collections are deferred to
    /// the next `scan_roots_done`. JIT mode never sets this: its roots are the
    /// native stack. One mutator defers alone through `Tlab::deferred`.
    safepoint_mode: bool,
    /// A trigger fired while in safepoint mode; collect at the next snapshot.
    collect_pending: bool,
}
#[derive(Debug)]
struct Block {
    /// One claim bit per line, packed 64 to a word. Atomic so the mark phase can
    /// run on several threads; Relaxed throughout, since the world is stopped and
    /// a line's claim is established by the fetch_or alone.
    mark_bits: [AtomicU64; MARK_WORDS],
    /// True while any multi-line span is recorded in this block; a block without
    /// one skips the marker's walk-back.
    has_span: bool,
    /// Set by the marker the first time any line in this block is claimed, so
    /// sweep can skip a block nothing reached: its bits are already clear.
    any_marked: AtomicBool,
    /// Set when a traced object whose descriptor has a drop hook is allocated
    /// in this block, cleared by the drop pass once no traced object survives
    /// in it: only such a block needs that pass. A dead traced object without
    /// a drop hook keeps its start until its lines are reused, as a raw object
    /// does, and a stale pointer resolving to it only retains.
    has_drop: bool,
}

/// Claim a line for the marker. Returns true for the thread that set it, so a
/// line is pushed onto exactly one worklist however many threads race for it.
#[inline(always)]
fn claim_line(block: &Block, line_idx: usize) -> bool {
    // Load first: a line already marked is the common case and needs no
    // read-modify-write. The fetch_or decides the race for the rest.
    let word = &block.mark_bits[line_idx >> 6];
    let bit = 1u64 << (line_idx & 63);
    if word.load(Ordering::Relaxed) & bit != 0 {
        return false;
    }
    if word.fetch_or(bit, Ordering::Relaxed) & bit != 0 {
        return false;
    }
    // Only on a successful claim, and only when not already set, so the store
    // stays off the path for every line after a block's first.
    if !block.any_marked.load(Ordering::Relaxed) {
        block.any_marked.store(true, Ordering::Relaxed);
    }
    true
}

#[inline]
fn clear_marks(block: &Block) {
    for word in &block.mark_bits {
        word.store(0, Ordering::Relaxed);
    }
}

impl Block {
    #[inline(always)]
    fn is_marked(&self, line_idx: usize) -> bool {
        self.mark_bits[line_idx >> 6].load(Ordering::Relaxed) & (1u64 << (line_idx & 63)) != 0
    }

    /// Set a line's bit without reporting who won: for callers that mark a
    /// line they already know is theirs. `claim_line` is the racing form.
    #[inline(always)]
    fn set_mark(&self, line_idx: usize) {
        self.mark_bits[line_idx >> 6].fetch_or(1u64 << (line_idx & 63), Ordering::Relaxed);
    }

    #[inline]
    fn marked_line_count(&self) -> usize {
        self.mark_bits
            .iter()
            .map(|w| w.load(Ordering::Relaxed).count_ones() as usize)
            .sum()
    }
}

struct RootSet {
    globals: Vec<*mut hl::vdynamic>,
    stack_roots: Vec<*mut hl::vdynamic>,
    persistent_roots: HashSet<*mut hl::vdynamic>,
    /// Addresses of pointer slots a native library asked us to keep live via
    /// `hl_add_root`: upstream's `gc_roots` is a `void***`, re-read at every
    /// collection, so a library may overwrite the slot without telling us. The
    /// slot itself is virtually never inside our heap.
    root_slots: HashSet<usize>,
}

pub struct ImmixAllocator {
    heap: ImmixHeap,
    blocks: Vec<Block>,
    roots: Rc<RefCell<RootSet>>,
    globals_range: Option<(*const *mut c_void, usize)>,
    /// Address ranges scanned conservatively at every collection, as
    /// `(start, len)`: data sections of linked spokes, module variable arrays.
    root_ranges: Vec<(usize, usize)>,
    /// Handles plugins and adapters hold across calls; every live slot is a
    /// root.
    handles: HandleTable,
    /// Registered fiber stacks for conservative scanning. Each OS-thread
    /// mutator owns one id-0 main-stack descriptor; nonzero fiber ids are
    /// process-unique.
    fiber_stacks: Vec<FiberStackInfo>,
    /// Heap offsets of blocks allocated with `MEM_KIND_FINALIZER`. The kind
    /// bits say only that such a block is raw, so this table is how the
    /// collector tells a finalizable block from any other. Small: only hdlls
    /// allocate this way.
    finalizables: HashSet<usize>,
}

/// A counted reference to a heap object, held outside the heap. `Handle(0)`
/// is null; the rest index the table plus one.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Handle(u32);

impl Handle {
    pub const NULL: Handle = Handle(0);

    /// The raw word, for a C caller that stores handles as integers.
    pub const fn as_raw(self) -> u32 {
        self.0
    }

    pub const fn from_raw(raw: u32) -> Handle {
        Handle(raw)
    }

    pub const fn is_null(self) -> bool {
        self.0 == 0
    }
}

struct HandleSlot {
    ptr: usize,
    /// Zero for a free slot.
    refs: u32,
}

#[derive(Default)]
struct HandleTable {
    slots: Vec<HandleSlot>,
    free: Vec<u32>,
}

impl HandleTable {
    fn insert(&mut self, ptr: *mut u8) -> Handle {
        if ptr.is_null() {
            return Handle::NULL;
        }
        let slot = HandleSlot {
            ptr: ptr as usize,
            refs: 1,
        };
        let index = match self.free.pop() {
            Some(i) => {
                self.slots[i as usize] = slot;
                i
            }
            None => {
                self.slots.push(slot);
                (self.slots.len() - 1) as u32
            }
        };
        Handle(index + 1)
    }

    fn live(&mut self, h: Handle) -> Option<&mut HandleSlot> {
        let slot = self.slots.get_mut(h.0.checked_sub(1)? as usize)?;
        (slot.refs != 0).then_some(slot)
    }

    fn get(&self, h: Handle) -> *mut u8 {
        h.0.checked_sub(1)
            .and_then(|i| self.slots.get(i as usize))
            .filter(|s| s.refs != 0)
            .map_or(ptr::null_mut(), |s| s.ptr as *mut u8)
    }

    fn retain(&mut self, h: Handle) {
        if let Some(slot) = self.live(h) {
            slot.refs += 1;
        }
    }

    fn release(&mut self, h: Handle) {
        if let Some(slot) = self.live(h) {
            slot.refs -= 1;
            if slot.refs == 0 {
                slot.ptr = 0;
                self.free.push(h.0 - 1);
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FiberStackInfo {
    pub thread: u64,
    pub id: u32,
    pub base: usize,
    pub size: usize,
    /// SP recorded at the stack's last switch-out; 0 = never suspended.
    pub saved_sp: usize,
}

impl Default for ImmixAllocator {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve a candidate to its actual allocation, including interior pointers.
/// Small starts are at most one line away; only spans can cross a line.
fn allocation_at(
    blocks: &[Block],
    alloc_sizes: &[u32],
    objects: &[std::sync::atomic::AtomicU8],
    offset: usize,
) -> Option<(usize, usize)> {
    let quantum = offset / ALLOC_QUANTUM;
    let floor = quantum / (LINE_SIZE / ALLOC_QUANTUM) * (LINE_SIZE / ALLOC_QUANTUM);
    for start in (floor..=quantum).rev() {
        let code = objects.get(start)?.load(Ordering::Relaxed) & OBJECT_SIZE_MASK;
        if code == 0 {
            continue;
        }
        let begin = start * ALLOC_QUANTUM;
        let size = if code == SPAN_OBJECT {
            alloc_sizes[begin / LINE_SIZE] as usize * LINE_SIZE
        } else {
            code as usize * ALLOC_QUANTUM
        };
        return (offset - begin < size).then_some((begin, size));
    }
    let line = offset / LINE_SIZE;
    let mut start = line;
    loop {
        let b = start / LINES_PER_BLOCK;
        if !blocks.get(b)?.has_span {
            return None;
        }
        let floor = b * LINES_PER_BLOCK;
        while start > floor && alloc_sizes[start] == 0 {
            start -= 1;
        }
        if alloc_sizes[start] != 0 {
            let begin = start * LINE_SIZE;
            let size = alloc_sizes[start] as usize * LINE_SIZE;
            let code = objects[begin / ALLOC_QUANTUM].load(Ordering::Relaxed) & OBJECT_SIZE_MASK;
            return (code == SPAN_OBJECT && offset - begin < size).then_some((begin, size));
        }
        start = start.checked_sub(1)?;
    }
}

/// Claim the allocation containing `offset` for the open cycle: its object
/// bit and its lines. `Some((start, size))` for the claimer, `None` when it
/// was already claimed or `offset` is in no allocation.
fn claim_allocation(
    blocks: &[Block],
    alloc_sizes: &[u32],
    objects: &[std::sync::atomic::AtomicU8],
    offset: usize,
) -> Option<(usize, usize)> {
    let (start, size) = allocation_at(blocks, alloc_sizes, objects, offset)?;
    let slot = &objects[start / ALLOC_QUANTUM];
    if slot.load(Ordering::Relaxed) & OBJECT_MARK != 0
        || slot.fetch_or(OBJECT_MARK, Ordering::Relaxed) & OBJECT_MARK != 0
    {
        return None;
    }
    for line in start / LINE_SIZE..=(start + size - 1) / LINE_SIZE {
        claim_line(&blocks[line / LINES_PER_BLOCK], line % LINES_PER_BLOCK);
    }
    Some((start, size))
}

/// Claim OBJECTS, not lines. Two reachable objects on a shared line must
/// both be traced; an unreachable neighbour on that line must not be traced.
fn mark_allocation_shared(
    blocks: &[Block],
    alloc_sizes: &[u32],
    objects: &[std::sync::atomic::AtomicU8],
    offset: usize,
    out: &mut Vec<(usize, usize)>,
) {
    if let Some(claimed) = claim_allocation(blocks, alloc_sizes, objects, offset) {
        out.push(claimed);
    }
}

/// Trace only the allocation's bytes, never other objects sharing its lines.
/// The kind bits choose how: a `NoPtr` block holds no pointers; a traced
/// object is walked by its descriptor's hook, or conservatively past word
/// zero when it has none, since a descriptor is never a heap object.
#[inline]
fn scan_allocation_shared(
    blocks: &[Block],
    alloc_sizes: &[u32],
    objects: &[std::sync::atomic::AtomicU8],
    heap_start: usize,
    heap_end: usize,
    start: usize,
    size: usize,
    out: &mut Vec<(usize, usize)>,
) {
    let mut first = 0;
    match objects[start / ALLOC_QUANTUM].load(Ordering::Relaxed) & OBJECT_KIND_MASK {
        OBJECT_KIND_NOPTR => return,
        OBJECT_KIND_TRACED => {
            let obj = (heap_start + start) as *mut u8;
            let desc = unsafe { *(obj as *const *const TypeDesc) };
            if let Some(trace) = unsafe { desc.as_ref() }.and_then(|d| d.trace) {
                let mut tracer = Tracer {
                    blocks,
                    alloc_sizes,
                    objects,
                    heap_start,
                    heap_end,
                    out,
                };
                unsafe { trace(obj, &mut tracer) };
                return;
            }
            first = WORD;
        }
        OBJECT_KIND_RAW => {}
        // Reserved. Scanned as raw: that retains, never frees.
        _ => {}
    }
    for off in (first..size).step_by(WORD) {
        let val = unsafe { *((heap_start + start + off) as *const usize) };
        if val >= heap_start && val < heap_end {
            mark_allocation_shared(blocks, alloc_sizes, objects, val - heap_start, out);
        }
    }
}

/// What a trace hook marks through: the side tables and the worklist of the
/// marker that called it, serial loop or pool worker alike. Lives only for
/// the hook's call.
pub struct Tracer<'a> {
    blocks: &'a [Block],
    alloc_sizes: &'a [u32],
    objects: &'a [std::sync::atomic::AtomicU8],
    heap_start: usize,
    heap_end: usize,
    out: &'a mut Vec<(usize, usize)>,
}

impl Tracer<'_> {
    /// Claim the allocation `ptr` points into, interior pointers included,
    /// and queue it for tracing. Anything outside the heap is ignored.
    pub fn mark(&mut self, ptr: *const u8) {
        let addr = ptr as usize;
        if addr >= self.heap_start && addr < self.heap_end {
            mark_allocation_shared(
                self.blocks,
                self.alloc_sizes,
                self.objects,
                addr - self.heap_start,
                self.out,
            );
        }
    }

    /// [`Self::mark`] for a NaN-boxed `caribou_abi::Value`: an object payload
    /// is marked, every other value is ignored.
    pub fn mark_value(&mut self, bits: u64) {
        if let Some(obj) = caribou_abi::Value::from_bits(bits).as_object() {
            self.mark(obj as *const u8);
        }
    }
}

/// How many threads mark. `ASH_GC_MARK_THREADS` overrides (1 keeps the phase
/// on the collecting thread). Safe.
fn mark_threads() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        if let Ok(v) = std::env::var("ASH_GC_MARK_THREADS") {
            if let Ok(n) = v.parse::<usize>() {
                return n.max(1);
            }
        }
        // N-1, so the machine keeps a core for everything that is not marking.
        // One thread on wasm, where the parallel marker is not compiled.
        if cfg!(target_family = "wasm") {
            return 1;
        }
        std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).clamp(1, 8))
            .unwrap_or(1)
    })
}

// Parallel marking, which needs threads to mark with.
#[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
struct MarkQueue {
    work: std::sync::Mutex<Vec<(usize, usize)>>,
    ready: std::sync::Condvar,
    idle: std::sync::atomic::AtomicUsize,
    done: AtomicBool,
}

/// The slices one marking worker reads, as raw parts. The world is stopped
/// for the whole job and `MarkPool::run` does not return until every worker
/// has left it, so these outlive every read.
#[cfg(not(target_family = "wasm"))]
#[derive(Clone, Copy)]
struct MarkJob {
    blocks: (*const Block, usize),
    alloc_sizes: (*const u32, usize),
    objects: (*const std::sync::atomic::AtomicU8, usize),
    heap_start: usize,
    heap_end: usize,
    queue: *const MarkQueue,
    threads: usize,
}

// Read-only for the duration of a job, which runs entirely inside a stopped
// world. Nothing here is dereferenced outside `MarkPool::run`.
#[cfg(not(target_family = "wasm"))]
unsafe impl Send for MarkJob {}
#[cfg(not(target_family = "wasm"))]
unsafe impl Sync for MarkJob {}

/// Marking threads that outlive a collection.
///
/// A worker parks on `wake` between jobs rather than being created for one.
/// `seq` is what tells a waking worker whether the job it can see is one it
/// has already run, so a spurious wakeup does not re-trace the heap.
#[cfg(not(target_family = "wasm"))]
struct MarkPool {
    state: std::sync::Mutex<(u64, Option<MarkJob>)>,
    wake: std::sync::Condvar,
    left: std::sync::Mutex<usize>,
    finished: std::sync::Condvar,
    size: usize,
}

#[cfg(not(target_family = "wasm"))]
impl MarkPool {
    fn get() -> &'static MarkPool {
        static POOL: OnceLock<MarkPool> = OnceLock::new();
        POOL.get_or_init(|| {
            let size = mark_threads();
            MarkPool {
                state: std::sync::Mutex::new((0, None)),
                wake: std::sync::Condvar::new(),
                left: std::sync::Mutex::new(0),
                finished: std::sync::Condvar::new(),
                size,
            }
        })
    }

    /// Start the workers. Separate from `get` because the threads need the
    /// `&'static` the OnceLock only hands back after initialisation.
    fn start(&'static self) {
        static STARTED: std::sync::Once = std::sync::Once::new();
        STARTED.call_once(|| {
            for _ in 0..self.size {
                // A marking thread must not be scanned as a mutator, and it
                // never runs VM code, so it registers nothing with the GC.
                std::thread::Builder::new()
                    .name("ash-gc-mark".into())
                    .spawn(move || self.worker())
                    .expect("gc marking thread");
            }
        });
    }

    fn worker(&'static self) {
        let mut ran = 0u64;
        loop {
            let job = {
                let mut state = self.state.lock().expect("mark pool poisoned");
                while state.0 == ran || state.1.is_none() {
                    state = self.wake.wait(state).expect("mark pool poisoned");
                }
                ran = state.0;
                state.1.expect("a new sequence always carries a job")
            };
            mark_worker(&job);
            let mut left = self.left.lock().expect("mark pool poisoned");
            *left -= 1;
            if *left == 0 {
                self.finished.notify_all();
            }
        }
    }

    /// Run `job` on every worker and return once all of them have left it.
    fn run(&'static self, job: MarkJob) {
        self.start();
        {
            let mut left = self.left.lock().expect("mark pool poisoned");
            *left = self.size;
        }
        {
            let mut state = self.state.lock().expect("mark pool poisoned");
            state.0 += 1;
            state.1 = Some(job);
        }
        self.wake.notify_all();
        let mut left = self.left.lock().expect("mark pool poisoned");
        while *left > 0 {
            left = self.finished.wait(left).expect("mark pool poisoned");
        }
    }
}

/// One worker's share of a marking job.
#[cfg(not(target_family = "wasm"))]
fn mark_worker(job: &MarkJob) {
    // Safety: see `MarkJob`. The job runs inside a stopped world and the
    // caller outlives it.
    let blocks: &[Block] = unsafe { std::slice::from_raw_parts(job.blocks.0, job.blocks.1) };
    let alloc_sizes: &[u32] =
        unsafe { std::slice::from_raw_parts(job.alloc_sizes.0, job.alloc_sizes.1) };
    let objects: &[std::sync::atomic::AtomicU8] =
        unsafe { std::slice::from_raw_parts(job.objects.0, job.objects.1) };
    let queue: &MarkQueue = unsafe { &*job.queue };
    let threads = job.threads;
    let heap_start = job.heap_start;
    let heap_end = job.heap_end;
    const BATCH: usize = 64;
    const SPILL: usize = 512;
    let mut local: Vec<(usize, usize)> = Vec::with_capacity(SPILL * 2);
    loop {
        if local.is_empty() {
            let mut work = queue.work.lock().expect("mark queue poisoned");
            loop {
                if !work.is_empty() {
                    let take = work.len().min(BATCH);
                    let at = work.len() - take;
                    local.extend(work.drain(at..));
                    break;
                }
                if queue.done.load(Ordering::Relaxed) {
                    return;
                }
                // Everyone idle with an empty queue means the
                // trace is finished: a thread only reaches
                // here having drained its own local list.
                let idle = queue.idle.fetch_add(1, Ordering::Relaxed) + 1;
                if idle == threads {
                    queue.done.store(true, Ordering::Relaxed);
                    queue.ready.notify_all();
                    return;
                }
                // Timed, so a lost wakeup cannot strand anyone.
                let (w, _) = queue
                    .ready
                    .wait_timeout(work, std::time::Duration::from_micros(200))
                    .expect("mark queue poisoned");
                work = w;
                queue.idle.fetch_sub(1, Ordering::Relaxed);
            }
        }
        while let Some((start, size)) = local.pop() {
            scan_allocation_shared(
                blocks,
                alloc_sizes,
                objects,
                heap_start,
                heap_end,
                start,
                size,
                &mut local,
            );
            if local.len() >= SPILL {
                let half = local.len() / 2;
                let mut work = queue.work.lock().expect("mark queue poisoned");
                work.extend(local.drain(..half));
                drop(work);
                queue.ready.notify_all();
            }
        }
    }
}

impl ImmixAllocator {
    /// Whether `self` is the process heap `GC` holds, rather than a heap a
    /// test built on its own.
    fn is_singleton(&self) -> bool {
        unsafe { (*(&raw const GC)).get() }.is_some_and(|gc| ptr::eq(gc, self))
    }

    #[inline(always)]
    fn current_stack_addr() -> usize {
        // Portable stack probe: address of a local variable approximates current SP.
        let marker = 0u8;
        (&marker as *const u8) as usize
    }

    pub fn new() -> Self {
        Self::with_heap_size(heap_max_bytes())
    }

    fn with_heap_size(heap_size: usize) -> Self {
        let mut heap = ImmixHeap {
            memory: HeapMemory::new(heap_size),
            free_blocks: Vec::new(),
            used_blocks: HashSet::new(),
            recycle_spans: Vec::new(),
            allocation_point: 0,
            current_block_end: 0,
            alloc_count: 0,
            alloc_sizes: vec![0u32; heap_size / LINE_SIZE],
            objects: allocation_table(heap_size / ALLOC_QUANTUM),
            bytes_since_gc: 0,
            external_since_gc: 0,
            trigger_threshold: INITIAL_TRIGGER_BYTES,
            last_collect: Instant::now(),
            last_pressure_relief: Instant::now(),
            reusable_blocks: HashSet::new(),
            tlab_blocks: HashMap::new(),
            safepoint_mode: false,
            collect_pending: false,
        };

        if std::env::var("ASH_GC_TRACE_MAP").is_ok() {
            let base = heap.memory.base as usize;
            eprintln!(
                "[gc-map] heap reservation {:#x}..{:#x} ({} MB)",
                base,
                base + heap_size,
                heap_size >> 20
            );
        }

        // Reverse order so pop() hands out low addresses first — touched
        // pages stay contiguous at the heap base.
        for i in (0..heap_size).step_by(BLOCK_SIZE).rev() {
            heap.free_blocks.push(i);
        }

        // Zeroed rather than element-wise cloned, so the pages stay demand-committed
        // like the heap mapping. Sound because an all-zero Block is a valid Block.
        let block_count = heap_size / BLOCK_SIZE;
        let blocks: Vec<Block> = unsafe {
            let layout =
                std::alloc::Layout::array::<Block>(block_count).expect("block table layout");
            let ptr = std::alloc::alloc_zeroed(layout) as *mut Block;
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            Vec::from_raw_parts(ptr, block_count, block_count)
        };

        if gc_stats_enabled() {
            unsafe {
                atexit(gc_stats_atexit);
            }
        }

        ImmixAllocator {
            heap,
            blocks,
            roots: Rc::new(RefCell::new(RootSet {
                globals: Vec::new(),
                stack_roots: Vec::new(),
                persistent_roots: HashSet::new(),
                root_slots: HashSet::new(),
            })),
            fiber_stacks: Vec::new(),
            globals_range: None,
            root_ranges: Vec::new(),
            handles: HandleTable::default(),
            finalizables: HashSet::new(),
        }
    }

    /// Bytes of pressure at which the next automatic collection is due, as
    /// the last collection set it.
    pub fn trigger_threshold(&self) -> usize {
        self.heap.trigger_threshold
    }

    /// Whether an automatic collection is owed:
    /// 1. `ASH_GC_STRESS`: every Nth allocation.
    /// 2. Allocated + external bytes since the last collection >= threshold.
    /// 3. Wall-clock heartbeat, so long-idle processes deflate.
    ///
    /// Shared by the safepoint and allocation triggers so the two cannot drift.
    fn collection_due(&self, stress: usize, pressure: usize) -> bool {
        if stress > 0 {
            // alloc_count resets on every collection: collect on the Nth
            // allocation since the last one (N=1 → every allocation).
            return self.heap.alloc_count + 1 >= stress;
        }
        pressure >= self.heap.trigger_threshold
            // Heartbeat: clock read only every 1024 allocations.
            || (self.heap.alloc_count & 1023 == 0
                && self.heap.last_collect.elapsed() >= heartbeat_interval())
    }

    /// [`Self::collection_due`] checked at a point known to be a safepoint: a
    /// due trigger collects immediately instead of deferring to the next
    /// interpreter snapshot.
    pub(crate) fn maybe_collect_at_safepoint(&mut self) {
        if !current_mutator_registered() {
            return;
        }
        let stress = gc_stress_every();
        let pressure = self.heap.bytes_since_gc + self.heap.external_since_gc;
        let due = self.collection_due(stress, pressure);
        if !(due || self.heap.collect_pending) {
            return;
        }
        if !triggered_collection_allowed(pressure) {
            return;
        }
        self.collect_garbage();
    }

    fn maybe_collect(&mut self) {
        // No automatic collections before the runtime has entered user code
        // (`set_stack_top`): during bootstrap GC pointers sit in host-side structures
        // the scanner cannot see. The exhaustion backstop still applies.
        if !current_mutator_registered() {
            return;
        }
        let stress = gc_stress_every();
        let pressure = self.heap.bytes_since_gc + self.heap.external_since_gc;
        let due = self.collection_due(stress, pressure);
        if !due {
            return;
        }
        // A disabled collector still gives ground to runaway pressure; the
        // accumulated counters are not cleared here, so re-enabling collects
        // at the very next allocation.
        if !triggered_collection_allowed(pressure) {
            return;
        }
        if self.heap.safepoint_mode || current_mutator_deferred() {
            let hard = self
                .heap
                .trigger_threshold
                .saturating_mul(4)
                .max(trigger_ceiling_bytes())
                .min(max_deferred_pressure());
            if pressure < hard {
                self.heap.collect_pending = true;
                if self.is_singleton() {
                    COLLECT_PENDING.store(true, Ordering::Relaxed);
                }
                return;
            }
        }
        set_collect_origin(3);
        self.collect_garbage();
    }

    /// Take a block off the free list, un-madvising it first if its pages
    /// were marked reusable, and clearing any stale mark bits left by
    /// conservative scans of stale pointers into freed blocks.
    fn acquire_free_block(&mut self) -> Option<usize> {
        let addr = self.heap.free_blocks.pop()?;
        self.clear_allocation_metadata(addr, BLOCK_SIZE);
        self.blocks[addr / BLOCK_SIZE].has_span = false;
        self.blocks[addr / BLOCK_SIZE].has_drop = false;
        self.heap.used_blocks.insert(addr);
        self.reclaim_block_pages(addr);
        clear_marks(&self.blocks[addr / BLOCK_SIZE]);
        if trace_freed() {
            let base = self.heap.memory.as_ptr() as usize;
            eprintln!(
                "[gc-reuse] {:#x}..{:#x}",
                base + addr,
                base + addr + BLOCK_SIZE
            );
        }
        Some(addr)
    }

    /// A run of recycled lines of at least `size` bytes as `(start, end)`,
    /// its metadata cleared, for the locked path's bump region. The bytes are
    /// not zeroed: that path zeroes each object it hands out.
    fn take_recycled_span(&mut self, size: usize) -> Option<(usize, usize)> {
        if !recycle_lines() {
            return None;
        }
        let want_lines = size.div_ceil(LINE_SIZE).max(1);
        while let Some((block, start, len)) = self.heap.recycle_spans.pop() {
            if len < want_lines {
                continue;
            }
            let lo = block + start * LINE_SIZE;
            let span_bytes = len * LINE_SIZE;
            self.clear_allocation_metadata(lo, span_bytes);
            GC_STATS
                .lines_recycled
                .fetch_add(len as u64, Ordering::Relaxed);
            return Some((lo, lo + span_bytes));
        }
        None
    }

    /// Only called for free memory, under the allocation lock. In particular,
    /// a recycled span must forget its old large-object starts before a TLAB
    /// publishes new small objects in it.
    fn clear_allocation_metadata(&mut self, offset: usize, size: usize) {
        for slot in &mut self.heap.objects[offset / ALLOC_QUANTUM..(offset + size) / ALLOC_QUANTUM]
        {
            *slot.get_mut() = 0;
        }
        self.heap.alloc_sizes[offset / LINE_SIZE..(offset + size) / LINE_SIZE].fill(0);
    }

    fn record_allocation(&self, offset: usize, size: usize) {
        let code = if size <= LINE_SIZE {
            (size / ALLOC_QUANTUM) as u8
        } else {
            SPAN_OBJECT
        };
        self.heap.objects[offset / ALLOC_QUANTUM].store(code, Ordering::Relaxed);
    }

    /// Set the kind bits of a fresh allocation, under the lock that handed it
    /// out: no collection can have seen it yet. A traced object's word zero
    /// already holds its descriptor.
    fn set_allocation_kind(&mut self, offset: usize, kind: u8) {
        debug_assert_eq!(kind & !OBJECT_KIND_MASK, 0);
        self.heap.objects[offset / ALLOC_QUANTUM].fetch_or(kind, Ordering::Relaxed);
        if kind == OBJECT_KIND_TRACED {
            let obj = unsafe { self.heap.memory.as_ptr().add(offset) };
            let desc = unsafe { *(obj as *const *const TypeDesc) };
            if unsafe { desc.as_ref() }.is_some_and(|d| d.drop.is_some()) {
                self.blocks[offset / BLOCK_SIZE].has_drop = true;
            }
        }
    }

    /// MADV_FREE_REUSE a block whose pages were previously handed back via
    /// MADV_FREE_REUSABLE — without this, the kernel may discard the pages
    /// under memory pressure AFTER we've written live data into them.
    fn reclaim_block_pages(&mut self, addr: usize) {
        if self.heap.reusable_blocks.remove(&addr) {
            if trace_map() {
                let base = self.heap.memory.as_ptr() as usize;
                eprintln!(
                    "[gc-map] REUSE {:#x}..{:#x}",
                    base + addr,
                    base + addr + BLOCK_SIZE
                );
            }
            #[cfg(target_os = "macos")]
            unsafe {
                libc::madvise(
                    self.heap.memory.as_mut_ptr().add(addr) as *mut c_void,
                    BLOCK_SIZE,
                    libc::MADV_FREE_REUSE,
                );
            }
        }
    }

    /// Allocate process-lifetime memory (runtime type structures), pinned as a
    /// persistent root: it is referenced only from non-GC memory the scanner
    /// never sees.
    pub fn allocate_immortal(&mut self, size: usize) -> Option<NonNull<u8>> {
        let p = self.allocate(size)?;
        self.roots
            .borrow_mut()
            .persistent_roots
            .insert(p.as_ptr() as *mut hl::vdynamic);
        Some(p)
    }

    pub fn allocate(&mut self, size: usize) -> Option<NonNull<u8>> {
        let size = size.max(8);
        // 16-byte bump allocation, with allocations traced independently so
        // neighbours' pointers do not join unrelated objects into retention chains.
        //
        // Two placement rules keep the conservative marker sound:
        // * a small object never straddles a line, so its start can be found
        //   within that line;
        // * a multi-line object starts on a line boundary and its span is recorded
        //   in `alloc_sizes`, so interior pointers resolve back across a block
        //   boundary to the containing allocation.
        let aligned_size = (size + 15) & !15;

        self.maybe_collect();

        if aligned_size > BLOCK_SIZE {
            return self.allocate_large(size);
        }

        let multi_line = aligned_size > LINE_SIZE - (self.heap.allocation_point & (LINE_SIZE - 1));
        let mut point = self.heap.allocation_point;
        if aligned_size >= LINE_SIZE {
            // Line-aligned start; span recorded below.
            point = (point + LINE_SIZE - 1) & !(LINE_SIZE - 1);
        } else if multi_line {
            // Would straddle a line: skip to the next boundary.
            point = (point + LINE_SIZE - 1) & !(LINE_SIZE - 1);
        }

        if point + aligned_size > self.heap.current_block_end {
            // Recycled lines first, as the TLAB refill does, so a kept block's
            // free lines are reused rather than left until the block empties.
            // Spans too small for this object are dropped; the list is rebuilt
            // each sweep. Line-aligned, so either placement rule holds at it.
            let (start, end) = match self.take_recycled_span(aligned_size) {
                Some(region) => region,
                None => {
                    let new_block = match self.acquire_free_block() {
                        Some(b) => b,
                        None => {
                            // Exhaustion backstop trigger.
                            set_collect_origin(4);
                            self.collect_garbage();
                            self.acquire_free_block()? // None = out of memory
                        }
                    };
                    (new_block, new_block + BLOCK_SIZE)
                }
            };
            point = start;
            self.heap.current_block_end = end;
        }

        let result = unsafe {
            let ptr = self.heap.memory.as_mut_ptr().add(point);
            // Zeroed: callers require zeroed memory, and stale data in a reused block
            // would read as pointers.
            let reserved = if aligned_size >= LINE_SIZE {
                aligned_size.div_ceil(LINE_SIZE) * LINE_SIZE
            } else {
                aligned_size
            };
            std::ptr::write_bytes(ptr, 0, reserved);
            NonNull::new_unchecked(ptr)
        };

        if trace_alloc() {
            let base = self.heap.memory.as_ptr() as usize;
            eprintln!("[gc-alloc] {:#x} size={size}", base + point);
        }
        if aligned_size >= LINE_SIZE {
            // Multi-line span for the marker's walk-back. The span is rounded
            // up so its tail line is not shared: a small object packed after
            // it would make the walk-back ambiguous.
            let start_line = point / LINE_SIZE;
            let num_lines = aligned_size.div_ceil(LINE_SIZE);
            for b in start_line / LINES_PER_BLOCK..=(start_line + num_lines - 1) / LINES_PER_BLOCK {
                if let Some(blk) = self.blocks.get_mut(b) {
                    blk.has_span = true;
                }
            }
            self.heap.alloc_sizes[start_line] = num_lines as u32;
            for i in 1..num_lines {
                self.heap.alloc_sizes[start_line + i] = 0;
            }
            self.heap.allocation_point = point + num_lines * LINE_SIZE;
        } else {
            self.heap.allocation_point = point + aligned_size;
        }
        self.record_allocation(point, aligned_size);
        self.heap.alloc_count += 1;
        self.heap.bytes_since_gc += aligned_size;
        GC_STATS
            .bytes_allocated
            .fetch_add(aligned_size as u64, Ordering::Relaxed);

        Some(result)
    }

    pub fn allocate_large(&mut self, size: usize) -> Option<NonNull<u8>> {
        let blocks_needed = size.div_ceil(BLOCK_SIZE);
        // Find contiguous free blocks by sorting the free list and scanning for a run.
        self.heap.free_blocks.sort_unstable();

        let mut run_start = None;
        let mut run_len = 0;
        for i in 0..self.heap.free_blocks.len() {
            let block = self.heap.free_blocks[i];
            if run_len == 0 {
                run_start = Some(i);
                run_len = 1;
            } else {
                let prev = self.heap.free_blocks[i - 1];
                if block == prev + BLOCK_SIZE {
                    run_len += 1;
                } else {
                    run_start = Some(i);
                    run_len = 1;
                }
            }
            if run_len >= blocks_needed {
                // Found a contiguous run — remove these blocks from free list
                let start_idx = run_start.unwrap();
                let start_addr = self.heap.free_blocks[start_idx];
                let removed: Vec<usize> = self
                    .heap
                    .free_blocks
                    .drain(start_idx..start_idx + blocks_needed)
                    .collect();
                for block in removed {
                    self.clear_allocation_metadata(block, BLOCK_SIZE);
                    self.blocks[block / BLOCK_SIZE].has_span = false;
                    self.blocks[block / BLOCK_SIZE].has_drop = false;
                    self.heap.used_blocks.insert(block);
                    self.reclaim_block_pages(block);
                    clear_marks(&self.blocks[block / BLOCK_SIZE]);
                }
                self.heap.bytes_since_gc += blocks_needed * BLOCK_SIZE;
                GC_STATS
                    .bytes_allocated
                    .fetch_add((blocks_needed * BLOCK_SIZE) as u64, Ordering::Relaxed);
                // Record allocation size for GC multi-line marking
                let num_lines = size.div_ceil(LINE_SIZE);
                let start_line = start_addr / LINE_SIZE;
                for b in
                    start_line / LINES_PER_BLOCK..=(start_line + num_lines - 1) / LINES_PER_BLOCK
                {
                    if let Some(blk) = self.blocks.get_mut(b) {
                        blk.has_span = true;
                    }
                }
                self.heap.alloc_sizes[start_line] = num_lines as u32;
                for j in 1..num_lines {
                    self.heap.alloc_sizes[start_line + j] = 0;
                }
                self.record_allocation(start_addr, size);
                return Some(unsafe {
                    let ptr = self.heap.memory.as_mut_ptr().add(start_addr);
                    std::ptr::write_bytes(ptr, 0, blocks_needed * BLOCK_SIZE);
                    NonNull::new_unchecked(ptr)
                });
            }
        }

        // No contiguous run found — trigger GC and retry
        set_collect_origin(5);
        self.collect_garbage();
        if self.heap.free_blocks.len() >= blocks_needed {
            return self.allocate_large(size);
        }
        None // Out of memory
    }

    pub unsafe fn is_gc_ptr<T>(&self, ptr: *const T) -> bool {
        // Cast the pointer to a usize for address arithmetic
        let addr = ptr as usize;

        // Check if the address is within the heap
        if addr < self.heap.memory.as_ptr() as usize
            || addr >= (self.heap.memory.as_ptr() as usize + self.heap.memory.len)
        {
            return false;
        }

        // Calculate the block index
        let block_index = (addr - self.heap.memory.as_ptr() as usize) / BLOCK_SIZE;

        // Check if the block is in use
        if !self.heap.used_blocks.contains(&(block_index * BLOCK_SIZE)) {
            return false;
        }

        // Calculate the line index within the block
        let line_index = (addr % BLOCK_SIZE) / LINE_SIZE;

        // Check if the line is marked (i.e., in use)
        if !self.blocks[block_index].is_marked(line_index) {
            return false;
        }

        // If it's a vdynamic pointer, we need to check its internal pointer as well
        if std::mem::size_of::<T>() == std::mem::size_of::<hl::vdynamic>() {
            // Safety: We've already checked that this pointer is within our heap
            let vd = unsafe { &*(ptr as *const hl::vdynamic) };

            // Check the type pointer
            if !vd.t.is_null() && !unsafe { self.is_gc_ptr(vd.t) } {
                return false;
            }

            // Check the value pointer for certain types
            match unsafe { (*vd.t).kind } {
                hl::HOBJ | hl::HFUN | hl::HARRAY | hl::HVIRTUAL | hl::HDYNOBJ | hl::HBYTES
                    if !unsafe { self.is_gc_ptr(vd.v.ptr) } =>
                {
                    return false;
                }
                _ => {} // Other types don't have additional pointers to check
            }
        }

        true
    }

    fn mark_allocation(&self, offset: usize, out: &mut Vec<(usize, usize)>) {
        mark_allocation_shared(
            &self.blocks,
            &self.heap.alloc_sizes,
            &self.heap.objects,
            offset,
            out,
        );
    }

    /// Heap offset of `addr`, if it lies inside the reservation.
    #[inline]
    fn offset_of(&self, addr: usize) -> Option<usize> {
        let base = self.heap.memory.as_ptr() as usize;
        (addr >= base && addr < base + self.heap.memory.len).then(|| addr - base)
    }

    // ── For a hosted collector that claims and reclaims its own objects ──

    /// The allocation containing `addr` as `(start, size)`, interior
    /// pointers included: the marker's own lookup. `None` outside the heap,
    /// in free space, or in a forgotten allocation.
    pub fn allocation_containing(&self, addr: usize) -> Option<(usize, usize)> {
        let offset = self.offset_of(addr)?;
        let (start, size) = allocation_at(
            &self.blocks,
            &self.heap.alloc_sizes,
            &self.heap.objects,
            offset,
        )?;
        Some((self.heap.memory.as_ptr() as usize + start, size))
    }

    /// Claim the allocation containing `ptr` for the open cycle exactly as the
    /// marker would, object bit and lines; true for the claimer. The claim
    /// stands until the next sweep clears it, so a host that claims outside a
    /// collection must end with one, or withdraw its claims.
    ///
    /// The caller holds the lock and no marker runs, so this thread is the
    /// only writer of the bits: plain loads and stores, no read-modify-write.
    #[inline]
    pub fn claim_for_cycle(&self, ptr: *const u8) -> bool {
        let Some(offset) = self.offset_of(ptr as usize) else {
            return false;
        };
        let Some((start, size)) = allocation_at(
            &self.blocks,
            &self.heap.alloc_sizes,
            &self.heap.objects,
            offset,
        ) else {
            return false;
        };
        self.claim_serial(start, size)
    }

    /// [`Self::claim_for_cycle`] for a host that knows `start` is an
    /// allocation start: no resolve, and a standing claim stands. The
    /// allocation's size, `None` when nothing starts there.
    #[inline]
    pub fn claim_start(&self, start: *const u8) -> Option<usize> {
        let offset = self.offset_of(start as usize)?;
        let code = self.heap.objects[offset / ALLOC_QUANTUM].load(Ordering::Relaxed);
        let size = match code & OBJECT_SIZE_MASK {
            0 => return None,
            SPAN_OBJECT => self.heap.alloc_sizes[offset / LINE_SIZE] as usize * LINE_SIZE,
            quanta => quanta as usize * ALLOC_QUANTUM,
        };
        self.claim_serial(offset, size);
        Some(size)
    }

    /// The object bit and line bits of the allocation at heap offset
    /// `start`, without read-modify-writes; true for the claimer.
    #[inline]
    fn claim_serial(&self, start: usize, size: usize) -> bool {
        let slot = &self.heap.objects[start / ALLOC_QUANTUM];
        let code = slot.load(Ordering::Relaxed);
        if code & OBJECT_MARK != 0 {
            return false;
        }
        slot.store(code | OBJECT_MARK, Ordering::Relaxed);
        for line in start / LINE_SIZE..=(start + size - 1) / LINE_SIZE {
            let block = &self.blocks[line / LINES_PER_BLOCK];
            let word = &block.mark_bits[(line % LINES_PER_BLOCK) >> 6];
            let bit = 1u64 << (line & 63);
            let bits = word.load(Ordering::Relaxed);
            if bits & bit == 0 {
                word.store(bits | bit, Ordering::Relaxed);
                if !block.any_marked.load(Ordering::Relaxed) {
                    block.any_marked.store(true, Ordering::Relaxed);
                }
            }
        }
        true
    }

    /// Whether the allocation containing `ptr` is claimed in the open cycle.
    pub fn is_claimed(&self, ptr: *const u8) -> bool {
        self.allocation_containing(ptr as usize)
            .is_some_and(|(start, _)| {
                self.object_slot(start).load(Ordering::Relaxed) & OBJECT_MARK != 0
            })
    }

    /// Withdraw the object claim on the allocation containing `ptr`. Its line
    /// claims stay until the sweep: a stale line claim only retains.
    pub fn unclaim(&self, ptr: *const u8) {
        if let Some((start, _)) = self.allocation_containing(ptr as usize) {
            self.object_slot(start)
                .fetch_and(!OBJECT_MARK, Ordering::Relaxed);
        }
    }

    /// Forget the allocation that starts at `start`: no lookup resolves into
    /// it, no cycle traces or drops it, and its lines return with the next
    /// sweep. The bytes are left as they are; the caller has released what
    /// the object owned.
    pub fn forget_allocation(&mut self, start: *const u8) {
        let Some(offset) = self.offset_of(start as usize) else {
            return;
        };
        let code = *self.heap.objects[offset / ALLOC_QUANTUM].get_mut() & OBJECT_SIZE_MASK;
        debug_assert!(code != 0, "not an allocation start");
        if code == SPAN_OBJECT {
            self.heap.alloc_sizes[offset / LINE_SIZE] = 0;
        }
        *self.heap.objects[offset / ALLOC_QUANTUM].get_mut() = 0;
    }

    #[inline]
    fn object_slot(&self, start: usize) -> &std::sync::atomic::AtomicU8 {
        &self.heap.objects[(start - self.heap.memory.as_ptr() as usize) / ALLOC_QUANTUM]
    }

    /// Record a block allocated with `MEM_KIND_FINALIZER`; its word zero holds a
    /// `void (*)(void *)` called once the block dies. The caller writes the
    /// callback after this; a null word zero at collection time means it never did.
    pub fn register_finalizable(&mut self, ptr: *mut u8) {
        let heap_start = self.heap.memory.as_ptr() as usize;
        let addr = ptr as usize;
        if addr >= heap_start && addr < heap_start + self.heap.memory.len {
            self.finalizables.insert(addr - heap_start);
        }
    }

    /// Queue the finalizers of blocks the trace did not reach. Runs between
    /// marking and sweeping, while the mark bits stand. A dead block is
    /// resurrected for this cycle so its fields are intact when the callback runs
    /// after the world restarts, and leaves the table, so the next collection
    /// reclaims it and the callback runs at most once.
    fn take_dead_finalizers(&mut self) {
        if self.finalizables.is_empty() {
            return;
        }
        let table = mem::take(&mut self.finalizables);
        let mut dead = Vec::new();
        let mut keep = HashSet::with_capacity(table.len());
        for offset in table {
            match self.heap.objects.get(offset / ALLOC_QUANTUM) {
                Some(slot) if slot.load(Ordering::Relaxed) & OBJECT_MARK != 0 => {
                    keep.insert(offset);
                }
                Some(_) => dead.push(offset),
                // Out of range: the address was never one of ours, so there
                // is nothing to reclaim and nothing to call.
                None => {}
            }
        }
        self.finalizables = keep;
        if dead.is_empty() {
            return;
        }

        let mut newly = Vec::new();
        for &offset in &dead {
            self.mark_allocation(offset, &mut newly);
        }
        if !newly.is_empty() {
            self.conservative_trace(newly);
        }

        // Read word zero only now: it is a code address the trace neither followed
        // nor disturbed. Leave it set, as upstream does: a body may guard on its
        // own slot (`hl_mutex_free`), and removing the block from the table is what
        // stops a second call.
        let heap_start = self.heap.memory.as_ptr() as usize;
        let mut queued = 0usize;
        if let Ok(mut queue) = PENDING_FINALIZERS.lock() {
            for offset in dead {
                let slot = (heap_start + offset) as *mut usize;
                let raw = unsafe { *slot };
                if raw == 0 {
                    continue;
                }
                queue.push((slot as usize, unsafe {
                    mem::transmute::<usize, Finalizer>(raw)
                }));
                queued += 1;
            }
        }
        if queued != 0 {
            PENDING_FINALIZER_COUNT.fetch_add(queued, Ordering::Relaxed);
        }
    }

    /// Conservative mark: scan a memory range for values that look like heap pointers.
    /// For each match, claim the containing allocation and mark its lines.
    /// Returns newly claimed (heap offset, allocation size) pairs.
    fn conservative_scan_range(&mut self, start: usize, end: usize) -> Vec<(usize, usize)> {
        let heap_start = self.heap.memory.as_ptr() as usize;
        let heap_end = heap_start + self.heap.memory.len;
        let mut newly_marked = Vec::new();

        // Step by a machine word: the read is a `usize`, and an eight-byte stride
        // on a 32-bit target skips every other slot.
        let mut addr = start;
        while addr + WORD <= end {
            let raw = unsafe { *(addr as *const usize) };
            // Two candidate interpretations per word: the raw value, and, when the word
            // carries the interpreter's NaN-box pattern, the boxed 48-bit payload, so
            // live register buffers can be scanned directly. A junk word that decodes
            // in-bounds only over-retains.
            let consider = |val: usize, this: &mut Self, out: &mut Vec<(usize, usize)>| {
                if val >= heap_start && val < heap_end {
                    this.mark_allocation(val - heap_start, out);
                }
            };
            consider(raw, self, &mut newly_marked);
            // Mirrors ash_interp::values: NAN_TAG 0x7FF8...<<48, 3-bit tag in bits
            // 48-50, payload in bits 0-47. Only meaningful where a machine word is 64
            // bits; on a 32-bit target roots must be explicit.
            #[cfg(target_pointer_width = "64")]
            {
                const NAN_TAG: usize = 0x7FF8_0000_0000_0000;
                const NAN_MASK: usize = 0xFFF8_0000_0000_0000;
                const PAYLOAD_MASK: usize = 0x0000_FFFF_FFFF_FFFF;
                if raw & NAN_MASK == NAN_TAG {
                    consider(raw & PAYLOAD_MASK, self, &mut newly_marked);
                }
            }
            addr += WORD;
        }
        newly_marked
    }

    /// Transitively scan newly-marked allocations. Line bits only control
    /// recycling; the worklist and duplicate suppression are object-granular.
    fn conservative_trace(&mut self, initial: Vec<(usize, usize)>) {
        let heap_start = self.heap.memory.as_ptr() as usize;
        let heap_end = heap_start + self.heap.memory.len;
        let threads = mark_threads();
        // Below a few hundred roots the spawn costs more than the trace saves.
        if threads <= 1 || initial.len() < 256 {
            let blocks = &self.blocks;
            let alloc_sizes = &self.heap.alloc_sizes;
            let mut worklist = initial;
            while let Some((start, size)) = worklist.pop() {
                scan_allocation_shared(
                    blocks,
                    alloc_sizes,
                    &self.heap.objects,
                    heap_start,
                    heap_end,
                    start,
                    size,
                    &mut worklist,
                );
            }
            // Not needless: on every target but wasm the parallel marker
            // follows, and this is what skips it.
            #[allow(clippy::needless_return)]
            return;
        }

        // Compiled out on wasm rather than merely skipped: the spawn would
        // make the module import `pthread_create`, and mark_threads() is one
        // there, so the serial loop above is the whole collector.
        #[cfg(not(target_family = "wasm"))]
        {
            // The world is stopped, so no write barrier is needed and an object is
            // claimed exactly once however many threads reach it.
            let blocks: &[Block] = &self.blocks;
            let alloc_sizes: &[u32] = &self.heap.alloc_sizes;
            let objects = &self.heap.objects;
            let queue = MarkQueue {
                work: std::sync::Mutex::new(initial),
                ready: std::sync::Condvar::new(),
                idle: std::sync::atomic::AtomicUsize::new(0),
                done: AtomicBool::new(false),
            };
            MarkPool::get().run(MarkJob {
                blocks: (blocks.as_ptr(), blocks.len()),
                alloc_sizes: (alloc_sizes.as_ptr(), alloc_sizes.len()),
                objects: (objects.as_ptr(), objects.len()),
                heap_start,
                heap_end,
                queue: &queue as *const MarkQueue,
                threads,
            });
        }
    }

    /// Charge off-heap memory as allocation pressure for the trigger; reset after
    /// every collection.
    pub fn track_external(&mut self, bytes: usize) {
        self.heap.external_since_gc = self.heap.external_since_gc.saturating_add(bytes);
        GC_STATS
            .external_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn collect_garbage(&mut self) {
        let t0 = Instant::now();
        let stopped_world = stop_mutator_world();
        // Nothing may be scanned while a mutator is still running: its stack
        // is being written as it would be read. Dropping `stopped_world`
        // releases whoever did park, and the next allocation asks again.
        if !stopped_world.stopped {
            return;
        }
        if trace_freed() || std::env::var("ASH_GC_DEBUG_ROOTS").is_ok() {
            let seq = GC_STATS.collections.load(Ordering::Relaxed) + 1;
            let origin = ORIGIN_NAMES[COLLECT_ORIGIN.load(Ordering::Relaxed).min(6) as usize];
            let base = self.heap.memory.as_ptr() as usize;
            eprintln!(
                "[gc-collect] #{seq} origin={origin} heap={base:#x}..{:#x} ranges={} pending={}",
                base + self.heap.memory.len,
                stopped_world
                    .snapshots
                    .iter()
                    .map(|m| m.scan_ranges.len())
                    .sum::<usize>(),
                self.heap.collect_pending,
            );
        }
        // The pause is split per phase.
        let t_stop = t0.elapsed();
        let t_mark0 = Instant::now();
        self.mark_roots(&stopped_world.snapshots);
        // Between the two phases: this reads the mark bits, and sweep clears them.
        self.take_dead_finalizers();
        let t_mark = t_mark0.elapsed();
        let t_sweep0 = Instant::now();
        let freed_blocks = self.sweep(&stopped_world.snapshots);
        let t_sweep = t_sweep0.elapsed();
        let pause = t0.elapsed();

        let live_blocks = self.heap.used_blocks.len();
        let live_bytes = live_blocks * BLOCK_SIZE;

        // Adaptive threshold: next collection after ~live*growth bytes, bounded. An
        // explicit `ASH_GC_TRIGGER_MB` floor above the ceiling raises the ceiling;
        // without the `max` the clamp would panic with `min > max`.
        let floor = trigger_floor_bytes();
        // The ceiling bounds fixed headroom but must never sit below the live set,
        // or the collector fires at a fraction of what is live and reclaims less
        // each time. One live set of garbage keeps peak near 2x live.
        let ceiling = trigger_ceiling_bytes().max(live_bytes).max(floor);
        self.heap.trigger_threshold =
            (live_bytes.saturating_mul(growth_factor())).clamp(floor, ceiling);
        if self.is_singleton() {
            let allocated = GC_STATS.bytes_allocated.load(Ordering::Relaxed);
            let external = GC_STATS.external_bytes.load(Ordering::Relaxed);
            NEXT_TRIGGER_AT.store(
                allocated + external + self.heap.trigger_threshold as u64,
                Ordering::Relaxed,
            );
            ALLOCATED_AT_COLLECT.store(allocated, Ordering::Relaxed);
            COLLECT_PENDING.store(false, Ordering::Relaxed);
        }

        self.heap.bytes_since_gc = 0;
        self.heap.external_since_gc = 0;
        self.heap.alloc_count = 0;
        self.heap.collect_pending = false;
        // Reset so next allocation picks a fresh free block
        self.heap.allocation_point = 0;
        self.heap.current_block_end = 0;
        self.heap.last_collect = Instant::now();

        // Everything above mutates heap state a resumed mutator would read, the
        // bump cursor especially. Nothing below touches the heap, so the world can
        // restart before the zone walk and the stderr writes.
        drop(stopped_world);
        if gc_stats_enabled() {
            let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
            eprintln!(
                "[gc-split] stop={:.2}ms mark={:.2}ms sweep={:.2}ms total={:.2}ms",
                ms(t_stop),
                ms(t_mark),
                ms(t_sweep),
                ms(pause)
            );
        }

        // Ask the malloc zones to hand free pages back to the OS. Throttled: a
        // whole-zone walk.
        #[cfg(target_os = "macos")]
        if self.heap.last_pressure_relief.elapsed() >= PRESSURE_RELIEF_MIN_INTERVAL {
            unsafe {
                malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
            }
            self.heap.last_pressure_relief = Instant::now();
        }

        // Stats (atomics — readable without the GC lock).
        let n = GC_STATS.collections.fetch_add(1, Ordering::Relaxed) + 1;
        GC_STATS
            .blocks_reclaimed
            .fetch_add(freed_blocks as u64, Ordering::Relaxed);
        GC_STATS
            .live_blocks
            .store(live_blocks as u64, Ordering::Relaxed);
        let pause_ns = pause.as_nanos() as u64;
        GC_STATS
            .pause_ns_total
            .fetch_add(pause_ns, Ordering::Relaxed);
        GC_STATS.pause_ns_max.fetch_max(pause_ns, Ordering::Relaxed);

        // `Gc.flags.set(Profile)` asks for the same per-cycle census
        // `ASH_GC_STATS` prints, so it routes here rather than to a second
        // report that could drift from this one.
        if gc_stats_enabled() || gc_flag(GC_FLAG_PROFILE) {
            eprintln!(
                "[gc] #{} origin={} pause={:.2}ms freed={} blocks live={} blocks ({}) \
                 next-trigger={} free={} blocks",
                n,
                ORIGIN_NAMES[COLLECT_ORIGIN.load(Ordering::Relaxed).min(6) as usize],
                pause_ns as f64 / 1e6,
                freed_blocks,
                live_blocks,
                fmt_mb(live_bytes as u64),
                fmt_mb(self.heap.trigger_threshold as u64),
                self.heap.free_blocks.len(),
            );
        }
    }

    fn mark_roots(&mut self, mutators: &[MutatorSnapshot]) {
        let roots = self.roots.clone();
        let root_set = roots.borrow();

        // Mark explicit roots using conservative approach:
        // Claim allocations, then conservative_trace follows their pointers.
        let heap_start = self.heap.memory.as_ptr() as usize;
        let heap_end = heap_start + self.heap.memory.len;
        let mut all_newly_marked = Vec::new();

        for &global_ptr in &root_set.globals {
            let addr = global_ptr as usize;
            if addr >= heap_start && addr < heap_end {
                self.mark_allocation(addr - heap_start, &mut all_newly_marked);
            }
        }
        for &stack_ptr in &root_set.stack_roots {
            let addr = stack_ptr as usize;
            if addr >= heap_start && addr < heap_end {
                self.mark_allocation(addr - heap_start, &mut all_newly_marked);
            }
        }
        for &persistent_ptr in &root_set.persistent_roots {
            let addr = persistent_ptr as usize;
            if addr >= heap_start && addr < heap_end {
                self.mark_allocation(addr - heap_start, &mut all_newly_marked);
            }
        }
        for slot in self.handles.slots.iter().filter(|s| s.refs != 0) {
            if slot.ptr >= heap_start && slot.ptr < heap_end {
                self.mark_allocation(slot.ptr - heap_start, &mut all_newly_marked);
            }
        }
        // A native root is a slot, so read through it rather than marking its
        // address: the slot is usually malloc'd and outside the heap. Reading it
        // once per collection is upstream's semantics: a slot the library
        // overwrites between calls is correct at the next cycle.
        let slots: Vec<usize> = root_set.root_slots.iter().copied().collect();
        drop(root_set);
        for slot in slots {
            // conservative_scan_range does the read, the heap bounds check and
            // the line marking, so a slot holding null or a non-heap value is
            // ignored exactly as upstream ignores it.
            let newly = self.conservative_scan_range(slot, slot + std::mem::size_of::<usize>());
            all_newly_marked.extend(newly);
        }

        // Conservative scan of globals_data
        let dbg = std::env::var("ASH_GC_DEBUG_ROOTS").is_ok();
        if let Some((globals_ptr, count)) = self.globals_range {
            let start = globals_ptr as usize;
            let end = start + count * std::mem::size_of::<usize>();
            let newly_marked = self.conservative_scan_range(start, end);
            if dbg {
                eprintln!("[gc-roots]   globals marked {} lines", newly_marked.len());
            }
            all_newly_marked.extend(newly_marked);
        }
        // Registered root ranges. A data section need not start on a word.
        for i in 0..self.root_ranges.len() {
            let (start, len) = self.root_ranges[i];
            let newly_marked = self.conservative_scan_range(word_align_up(start), start + len);
            if dbg {
                eprintln!(
                    "[gc-roots]   root range {start:#x}+{len} marked {} lines",
                    newly_marked.len()
                );
            }
            all_newly_marked.extend(newly_marked);
        }

        // Conservative scan of interpreter-provided ranges
        for mutator in mutators {
            for &(start, size) in &mutator.scan_ranges {
                if size == 0 {
                    continue;
                }
                let end = start.saturating_add(size);
                if end > start {
                    let newly_marked = self.conservative_scan_range(start, end);
                    if dbg {
                        eprintln!(
                            "[gc-roots]   range {start:#x}+{size} marked {} lines",
                            newly_marked.len()
                        );
                    }
                    all_newly_marked.extend(newly_marked);
                }
            }
        }

        // Conservative scan of execution stacks. Collection runs on the allocating
        // context's stack, main thread or fiber, so the live probe SP is resolved
        // against the registry. The callee-saved registers are spilled into `buf`,
        // a local of this frame, and the probe is clamped to its address so the
        // spilled words lie inside the scanned range.
        let mut buf = [0usize; CALLEE_SAVED_WORDS];
        spill_callee_saved(&mut buf);
        let collector = thread_self_fast();
        let collector_probe = Self::current_stack_addr().min(buf.as_ptr() as usize);
        let fiber_stacks = self.fiber_stacks.clone();
        for mutator in mutators {
            let raw_sp = if mutator.thread == collector {
                collector_probe
            } else {
                mutator.stack_sp
            };
            if raw_sp == 0 {
                continue;
            }
            // Word-align the probe: conservative_scan_range walks words.
            let sp = word_align_up(raw_sp);
            let running_fiber = fiber_stacks
                .iter()
                .find(|f| {
                    f.thread == mutator.thread && f.size > 0 && sp >= f.base && sp < f.base + f.size
                })
                .map(|f| (f.id, f.base + f.size));
            if dbg {
                let top = running_fiber
                    .map(|(_, top)| top)
                    .unwrap_or(mutator.stack_top);
                eprintln!(
                    "[gc-roots] thread={:#x} sp={sp:#x} stack_top={top:#x} span={}KB ranges={} globals={:?}",
                    mutator.thread,
                    top.saturating_sub(sp) / 1024,
                    mutator.scan_ranges.len(),
                    self.globals_range.map(|(_, c)| c)
                );
                // Every stack this mutator owns, and whether it will be scanned.
                for f in fiber_stacks.iter().filter(|f| f.thread == mutator.thread) {
                    eprintln!(
                        "[gc-roots]   stack id={} base={:#x} size={} saved_sp={:#x}{}",
                        f.id,
                        f.base,
                        f.size,
                        f.saved_sp,
                        if f.saved_sp == 0 { "  <- SKIPPED" } else { "" }
                    );
                }
            }
            match running_fiber {
                Some((_, top)) => {
                    all_newly_marked.extend(self.conservative_scan_range(sp, top));
                }
                None => {
                    if mutator.stack_top > 0 && sp < mutator.stack_top {
                        all_newly_marked
                            .extend(self.conservative_scan_range(sp, mutator.stack_top));
                    }
                }
            }

            // All OTHER stacks owned by this mutator scan from their saved
            // switch-out SP. The id-0 descriptor is its suspended main stack.
            for f in fiber_stacks.iter().filter(|f| f.thread == mutator.thread) {
                if Some(f.id) == running_fiber.map(|(id, _)| id) || f.saved_sp == 0 {
                    continue;
                }
                let top = if f.size > 0 {
                    f.base + f.size
                } else {
                    if running_fiber.is_none() {
                        continue;
                    }
                    mutator.stack_top
                };
                // wasm only: a native fiber descriptor with no size is a real main stack
                // whose `saved_sp` is where the scan starts. On wasm the probe that
                // recorded `saved_sp` may sit above the frames it should cover, so the
                // range is taken from the top down, bounded by `WASM_STACK_WINDOW`:
                // scanning dead frames over-retains, starting above a live frame loses it.
                let window = if f.size == 0 && cfg!(target_family = "wasm") {
                    WASM_STACK_WINDOW
                } else {
                    0
                };
                let start = stack_scan_start(f.saved_sp, top, window);
                if start < top {
                    all_newly_marked.extend(self.conservative_scan_range(start, top));
                }
            }

            // Parked/blocked mutators copied their callee-saved registers
            // into registry-owned storage. The collector's registers are in
            // `buf`, which its live-stack scan already includes.
            if mutator.thread != collector {
                let start = mutator.saved_regs.as_ptr() as usize;
                let end = start + std::mem::size_of_val(&mutator.saved_regs);
                all_newly_marked.extend(self.conservative_scan_range(start, end));
            }
        }

        // Transitive conservative marking
        if !all_newly_marked.is_empty() {
            self.conservative_trace(all_newly_marked);
        }
    }

    pub fn mark_memory(&mut self, ptr: *mut u8, size: usize) {
        let heap_start = self.heap.memory.as_ptr() as usize;
        let heap_end = heap_start + self.heap.memory.len;
        let addr = ptr as usize;

        // Only mark memory within the heap range
        if addr < heap_start || addr >= heap_end {
            return;
        }

        let end_addr = (addr + size).min(heap_end);
        let mut current_addr = addr;

        while current_addr < end_addr {
            let offset = current_addr - heap_start;
            let block_index = offset / BLOCK_SIZE;
            let line_index = (offset % BLOCK_SIZE) / LINE_SIZE;

            if block_index < self.blocks.len() {
                self.blocks[block_index].set_mark(line_index);
                self.blocks[block_index]
                    .any_marked
                    .store(true, Ordering::Relaxed);
            }

            current_addr += LINE_SIZE;
        }
    }

    pub fn mark_object(&mut self, ptr: *mut hl::hl_type) {
        if ptr.is_null() {
            return;
        }

        let heap_start = self.heap.memory.as_ptr() as usize;
        let addr = ptr as usize;

        // Only mark objects within the heap
        if addr < heap_start || addr >= heap_start + self.heap.memory.len {
            return;
        }

        let offset = addr - heap_start;
        let block_index = offset / BLOCK_SIZE;
        let line_index = (offset % BLOCK_SIZE) / LINE_SIZE;

        if block_index < self.blocks.len() && !self.blocks[block_index].is_marked(line_index) {
            self.blocks[block_index].set_mark(line_index);

            // Mark children based on the type of object
            unsafe {
                match (*ptr).kind {
                    hl::HOBJ => {
                        let obj_ptr = (*ptr).detail.obj;
                        if !obj_ptr.is_null() {
                            let obj: &hl_type_obj = &*obj_ptr;
                            for i in 0..obj.nfields as usize {
                                if !obj.fields.is_null() {
                                    let field = &*obj.fields.add(i);
                                    self.mark_object(field.t);
                                }
                            }
                            if !obj.super_.is_null() {
                                self.mark_object(obj.super_);
                            }
                        }
                    }
                    hl::HFUN => {
                        let fun_ptr = (*ptr).detail.fun;
                        if !fun_ptr.is_null() {
                            let fun = &*fun_ptr;
                            for i in 0..fun.nargs as usize {
                                if !fun.args.is_null() {
                                    let arg = *fun.args.add(i);
                                    self.mark_object(arg);
                                }
                            }
                            if !fun.ret.is_null() {
                                self.mark_object(fun.ret);
                            }
                        }
                    }
                    hl::HENUM => {
                        let enum_ptr = (*ptr).detail.tenum;
                        if !enum_ptr.is_null() {
                            let enum_ = &*enum_ptr;
                            for i in 0..enum_.nconstructs as usize {
                                if !enum_.constructs.is_null() {
                                    let construct = &*enum_.constructs.add(i);
                                    for j in 0..construct.nparams as usize {
                                        if !construct.params.is_null() {
                                            let param = *construct.params.add(j);
                                            self.mark_object(param);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    hl::HNULL => {
                        let inner_type = (*ptr).detail.tparam;
                        if !inner_type.is_null() {
                            self.mark_object(inner_type);
                        }
                    }
                    _ => {} // Other types might not have child pointers
                }
            }
        }
    }

    pub fn mark_vdynamic(&mut self, vd_ptr: *mut hl::vdynamic) {
        if vd_ptr.is_null() {
            return;
        }

        // Only dereference pointers within the GC heap
        let heap_start = self.heap.memory.as_ptr() as usize;
        let heap_end = heap_start + self.heap.memory.len;
        let addr = vd_ptr as usize;
        if addr < heap_start || addr >= heap_end {
            return; // Not a GC-managed pointer, skip
        }

        unsafe {
            let vd = &*vd_ptr;
            self.mark_memory(vd_ptr as *mut u8, mem::size_of::<hl::vdynamic>());

            // Mark the type
            if !vd.t.is_null() {
                self.mark_object(vd.t);
            }

            // Depending on the type, we might need to mark more data
            // Mark the value based on its type
            if vd.t.is_null() {
                return;
            }
            match (*vd.t).kind {
                hl::HOBJ => {
                    let obj_ptr = vd.v.ptr as *mut hl::vobj;
                    if !obj_ptr.is_null() {
                        self.mark_object((*obj_ptr).t);
                    }
                }
                hl::HFUN => {
                    let fun_ptr = vd.v.ptr as *mut hl::vclosure;
                    if !fun_ptr.is_null() {
                        self.mark_object((*fun_ptr).t);
                        // Mark the function value and environment if present
                        if !(*fun_ptr).fun.is_null() {
                            self.mark_memory(
                                (*fun_ptr).fun as *mut u8,
                                mem::size_of::<*mut ::std::os::raw::c_void>(),
                            );
                        }
                        if (*fun_ptr).hasValue != 0 && !(*fun_ptr).value.is_null() {
                            self.mark_vdynamic((*fun_ptr).value as *mut hl::vdynamic);
                        }
                    }
                }
                hl::HARRAY => {
                    let array_ptr = vd.v.ptr as *mut hl::varray;
                    if !array_ptr.is_null() {
                        self.mark_object((*array_ptr).t);
                        if !(*array_ptr).at.is_null() {
                            self.mark_object((*array_ptr).at);
                        }
                        // Mark the full varray allocation (header + data payload).
                        // Child pointers inside data will be discovered by conservative_trace.
                        let size = (*array_ptr).size.max(0) as usize;
                        let esize = if (*array_ptr).at.is_null() {
                            HL_WSIZE
                        } else {
                            hl::type_size((*(*array_ptr).at).kind)
                        };
                        let total = mem::size_of::<hl::varray>() + size * esize;
                        self.mark_memory(array_ptr as *mut u8, total);
                    }
                }
                // Add more cases for other types as needed
                _ => {}
            }
        }
    }

    /// Run the drop hook of every unmarked traced object and forget its start,
    /// so no later cycle can resolve a stale pointer into it and trace or drop
    /// it again. Only in blocks flagged `has_drop`; see it for the rest. Marks
    /// are standing and the world is stopped. Before any block is reclaimed or
    /// poisoned: a hook reads its object, and a span may run on into a block
    /// the reclaim loop would visit first.
    fn drop_dead_traced(&mut self, used: &[usize]) {
        let base = self.heap.memory.as_ptr() as usize;
        for &block_addr in used {
            let block = &mut self.blocks[block_addr / BLOCK_SIZE];
            if !block.has_drop {
                continue;
            }
            let mut any_live = false;
            for q in block_addr / ALLOC_QUANTUM..(block_addr + BLOCK_SIZE) / ALLOC_QUANTUM {
                let slot = self.heap.objects[q].get_mut();
                let code = *slot;
                if code & OBJECT_KIND_MASK != OBJECT_KIND_TRACED {
                    continue;
                }
                if code & OBJECT_MARK != 0 {
                    any_live = true;
                    continue;
                }
                let obj = (base + q * ALLOC_QUANTUM) as *mut u8;
                let desc = unsafe { *(obj as *const *const TypeDesc) };
                if let Some(drop) = unsafe { desc.as_ref() }.and_then(|d| d.drop) {
                    unsafe { drop(obj) };
                }
                if code & OBJECT_SIZE_MASK == SPAN_OBJECT {
                    self.heap.alloc_sizes[q * ALLOC_QUANTUM / LINE_SIZE] = 0;
                }
                *slot = 0;
            }
            block.has_drop = any_live;
        }
    }

    /// Block-level collection: reclaim only entirely empty blocks. Partially
    /// occupied blocks are retained intact; dead lines are not zeroed, because
    /// conservative marking may miss a live object. Freed blocks' pages are
    /// returned to the OS. Returns the number of blocks reclaimed.
    fn sweep(&mut self, mutators: &[MutatorSnapshot]) -> usize {
        // Last cycle's spans die with last cycle's marks. Carrying them over
        // would hand out lines in a block this sweep is about to free, and
        // they are rebuilt below anyway.
        self.heap.recycle_spans.clear();
        let used_block_addrs: Vec<usize> = self.heap.used_blocks.iter().copied().collect();
        self.drop_dead_traced(&used_block_addrs);
        let mut freed: Vec<usize> = Vec::new();
        let (mut occ_blocks, mut occ_marked) = (0usize, 0usize);
        let mut occ_hist = [0usize; 6];
        // Audit exactly the objects the tracer visited. Dead neighbours on
        // a marked line can legitimately point into freed blocks.
        let audit_objects = sweep_audit().then(|| {
            let mut live = Vec::new();
            for &block in &used_block_addrs {
                for q in block / ALLOC_QUANTUM..(block + BLOCK_SIZE) / ALLOC_QUANTUM {
                    if self.heap.objects[q].load(Ordering::Relaxed) & OBJECT_MARK != 0 {
                        if let Some(object) = allocation_at(
                            &self.blocks,
                            &self.heap.alloc_sizes,
                            &self.heap.objects,
                            q * ALLOC_QUANTUM,
                        ) {
                            live.push(object);
                        }
                    }
                }
            }
            live
        });
        // Hoisted out of the per-block loop.
        let tlab_set: std::collections::HashSet<usize> =
            self.heap.tlab_blocks.values().copied().collect();
        // One buffer, reused across blocks.
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for block_addr in used_block_addrs {
            // The mutator's live bump region: marks still reset below for
            // the next cycle, but the block is never reclaimed under the
            // cursor.
            let is_tlab = tlab_set.contains(&block_addr);
            let block_index = block_addr / BLOCK_SIZE;
            let block = &mut self.blocks[block_index];
            // Nothing reached this block, so every bit is already clear and
            // the scan below could only confirm it.
            let touched = *block.any_marked.get_mut();
            if touched {
                for slot in &mut self.heap.objects
                    [block_addr / ALLOC_QUANTUM..(block_addr + BLOCK_SIZE) / ALLOC_QUANTUM]
                {
                    *slot.get_mut() &= !OBJECT_MARK;
                }
            }
            *block.any_marked.get_mut() = false;
            let mut is_empty = true;
            let mut marked_lines = 0usize;
            // Runs of unmarked lines, harvested in the pass that resets the
            // marks. Only for blocks this sweep KEEPS: an empty block goes
            // back whole, and the TLAB block is still being bumped through.
            spans.clear();
            let mut run_start: Option<usize> = None;
            // Plain reads and writes: sweep holds `&mut self`, so no marker is running.
            if touched {
                for (word_index, slot) in block.mark_bits.iter_mut().enumerate() {
                    let word = std::mem::replace(slot.get_mut(), 0);
                    // A word of 64 unmarked lines is the common case on a
                    // sparsely reached block; skipping it keeps the run that
                    // `run_start` is tracking open across the whole word.
                    if word == 0 {
                        if run_start.is_none() {
                            run_start = Some(word_index << 6);
                        }
                        continue;
                    }
                    for bit_index in 0..64 {
                        let line_index = (word_index << 6) | bit_index;
                        let was_marked = word & (1u64 << bit_index) != 0;
                        if was_marked {
                            is_empty = false;
                            marked_lines += 1;
                            if let Some(start) = run_start.take() {
                                spans.push((start, line_index - start));
                            }
                        } else if run_start.is_none() {
                            run_start = Some(line_index);
                        }
                    }
                }
                if let Some(start) = run_start.take() {
                    spans.push((start, LINES_PER_BLOCK - start));
                }
            }
            // An untouched block stays `is_empty` with no spans, which is what
            // the two branches below already do with a block nothing reached:
            // reclaim it whole, or keep it whole when it is a TLAB or when
            // reclamation is off. Neither reads `spans`.

            if occupancy_stats() && !is_empty {
                occ_blocks += 1;
                occ_marked += marked_lines;
                occ_hist[match marked_lines {
                    1 => 0,
                    2..=4 => 1,
                    5..=16 => 2,
                    17..=64 => 3,
                    65..=192 => 4,
                    _ => 5,
                }] += 1;
            }
            if !is_empty && !is_tlab && recycle_lines() {
                for (start, len) in spans.drain(..) {
                    self.heap.recycle_spans.push((block_addr, start, len));
                }
            }
            if is_empty && !is_tlab && !no_reclaim() {
                self.heap.used_blocks.remove(&block_addr);
                if trace_freed() {
                    let base = self.heap.memory.as_ptr() as usize;
                    let seq = GC_STATS.collections.load(Ordering::Relaxed) + 1;
                    eprintln!(
                        "[gc-freed] #{seq} {:#x}..{:#x}",
                        base + block_addr,
                        base + block_addr + BLOCK_SIZE
                    );
                }
                // Use-after-free detector: a block about to be freed while a root still
                // points into it means the tracer failed.
                if sweep_audit() {
                    let base = self.heap.memory.as_ptr() as usize;
                    let lo = base + block_addr;
                    let hi = lo + BLOCK_SIZE;
                    let audit = |src: &str, start: usize, end: usize| {
                        let mut p = start & !(WORD - 1);
                        while p + WORD <= end {
                            let w = unsafe { *(p as *const usize) };
                            if (lo..hi).contains(&w) {
                                eprintln!(
                                    "[gc-audit] FREED {lo:#x}..{hi:#x} but {src} @{p:#x} holds {w:#x}"
                                );
                            }
                            // Interpreter scan ranges hold NaN-boxed words; decode them as the marker
                            // does. Same 64-bit-word assumption.
                            #[cfg(target_pointer_width = "64")]
                            {
                                const NAN_TAG: usize = 0x7FF8_0000_0000_0000;
                                const NAN_MASK: usize = 0xFFF8_0000_0000_0000;
                                const PAYLOAD_MASK: usize = 0x0000_FFFF_FFFF_FFFF;
                                if w & NAN_MASK == NAN_TAG {
                                    let d = w & PAYLOAD_MASK;
                                    if (lo..hi).contains(&d) {
                                        eprintln!(
                                            "[gc-audit] FREED {lo:#x}..{hi:#x} but {src} @{p:#x} holds boxed {d:#x}"
                                        );
                                    }
                                }
                            }
                            p += WORD;
                        }
                    };
                    if let Some((gp, count)) = self.globals_range {
                        audit(
                            "globals",
                            gp as usize,
                            gp as usize + count * std::mem::size_of::<usize>(),
                        );
                    }
                    for &(start, len) in &self.root_ranges {
                        audit("root-range", word_align_up(start), start + len);
                    }
                    for slot in self.handles.slots.iter().filter(|s| s.refs != 0) {
                        if (lo..hi).contains(&slot.ptr) {
                            eprintln!(
                                "[gc-audit] FREED {lo:#x}..{hi:#x} but a handle holds {:#x}",
                                slot.ptr
                            );
                        }
                    }
                    for mutator in mutators {
                        for &(rs, sz) in &mutator.scan_ranges {
                            audit("range", rs, rs + sz);
                        }
                    }
                    // Every stopped machine/fiber stack too — the mark phase
                    // scanned the same ownership-qualified ranges.
                    let collector = thread_self_fast();
                    for mutator in mutators {
                        let raw_sp = if mutator.thread == collector {
                            Self::current_stack_addr()
                        } else {
                            mutator.stack_sp
                        };
                        if raw_sp == 0 {
                            continue;
                        }
                        let sp = word_align_up(raw_sp);
                        let running = self.fiber_stacks.iter().find(|f| {
                            f.thread == mutator.thread
                                && f.size > 0
                                && sp >= f.base
                                && sp < f.base + f.size
                        });
                        let top = running
                            .map(|f| f.base + f.size)
                            .unwrap_or(mutator.stack_top);
                        if sp < top {
                            audit("stack", sp, top);
                        }
                        for fiber in self
                            .fiber_stacks
                            .iter()
                            .filter(|f| f.thread == mutator.thread && f.saved_sp != 0)
                        {
                            if running.is_some_and(|active| active.id == fiber.id) {
                                continue;
                            }
                            let saved_sp = word_align_up(fiber.saved_sp);
                            let saved_top = if fiber.size > 0 {
                                fiber.base + fiber.size
                            } else {
                                mutator.stack_top
                            };
                            if saved_sp < saved_top {
                                audit("suspended-stack", saved_sp, saved_top);
                            }
                        }
                    }
                }
                if poison_freed() || quarantine_freed() {
                    unsafe {
                        std::ptr::write_bytes(
                            self.heap.memory.as_mut_ptr().add(block_addr),
                            0xA5,
                            BLOCK_SIZE,
                        );
                    }
                }
                if !quarantine_freed() {
                    self.heap.free_blocks.push(block_addr);
                }
                // Clear alloc_sizes for all lines in this freed block
                let base_line = block_index * LINES_PER_BLOCK;
                self.heap.alloc_sizes[base_line..base_line + LINES_PER_BLOCK].fill(0);
                for slot in &mut self.heap.objects
                    [block_addr / ALLOC_QUANTUM..(block_addr + BLOCK_SIZE) / ALLOC_QUANTUM]
                {
                    *slot.get_mut() = 0;
                }
                self.blocks[block_index].has_span = false;
                self.blocks[block_index].has_drop = false;
                freed.push(block_addr);
            }
        }

        // Second half of the detector: pointers into a freed block from retained
        // objects marked live this cycle. Dead objects are skipped; stale pointers
        // in garbage are expected. Diagnosis only.
        if !freed.is_empty() {
            if let Some(objects) = &audit_objects {
                let base = self.heap.memory.as_ptr() as usize;
                let seq = GC_STATS.collections.load(Ordering::Relaxed) + 1;
                let in_freed = |w: usize| -> bool {
                    if w < base || w >= base + self.heap.memory.len {
                        return false;
                    }
                    let off = (w - base) & !(BLOCK_SIZE - 1);
                    freed.contains(&off)
                };
                for &(offset, size) in objects {
                    let lo = base + offset;
                    let mut p = lo;
                    while p + WORD <= lo + size {
                        let w = unsafe { *(p as *const usize) };
                        if in_freed(w) {
                            eprintln!(
                                "[gc-audit] #{seq} live object word @{p:#x} points into freed block ({w:#x})"
                            );
                        }
                        p += WORD;
                    }
                }
            }
        }

        // Return fully free pages to the OS: MADV_FREE_REUSABLE on macOS,
        // MADV_DONTNEED elsewhere, one madvise per contiguous run. Safe because a
        // reacquired block is REUSEd and zeroed before live data is written. Only
        // when the process has been quiet for a heartbeat: a churn workload would
        // otherwise pay a REUSABLE+REUSE pair per block per cycle.
        let quiet = self.heap.last_collect.elapsed() >= heartbeat_interval();
        if quiet && !freed.is_empty() {
            let resident_target = 16;
            let surplus = self.heap.free_blocks.len().saturating_sub(resident_target);
            let mut hand_back: Vec<usize> = freed
                .iter()
                .copied()
                .take(surplus)
                .filter(|a| !self.heap.reusable_blocks.contains(a))
                .collect();
            if !hand_back.is_empty() && handback_enabled() {
                hand_back.sort_unstable();
                let base = self.heap.memory.as_mut_ptr();
                let mut run_start = hand_back[0];
                let mut run_len = BLOCK_SIZE;
                // Returns whether the range was actually handed back. On macOS
                // REUSABLE/REUSE is a ledger, so a block must be recorded as reusable only
                // when the advice succeeded, or the matching REUSE credits memory that was
                // never debited.
                let advise = |start: usize, len: usize| -> bool {
                    // A target with no such syscall hands the range back in
                    // ash's own bookkeeping and nowhere else, so nothing
                    // below reads any of these.
                    #[cfg(not(any(unix, windows)))]
                    let _ = (base, start, len);
                    #[cfg(unix)]
                    unsafe {
                        #[cfg(target_os = "macos")]
                        let advice = libc::MADV_FREE_REUSABLE;
                        #[cfg(not(target_os = "macos"))]
                        let advice = libc::MADV_DONTNEED;
                        return libc::madvise(base.add(start) as *mut c_void, len, advice) == 0;
                    }
                    // Windows: the pages leave the working set but stay committed; the
                    // contents go undefined, which is sound since a reacquired block is zeroed.
                    #[cfg(windows)]
                    unsafe {
                        DiscardVirtualMemory(base.add(start) as *mut c_void, len);
                        return true;
                    }
                    #[allow(unreachable_code)]
                    true
                };
                let mut runs: Vec<(usize, usize)> = Vec::new();
                for &addr in &hand_back[1..] {
                    if addr == run_start + run_len {
                        run_len += BLOCK_SIZE;
                    } else {
                        runs.push((run_start, run_len));
                        run_start = addr;
                        run_len = BLOCK_SIZE;
                    }
                }
                runs.push((run_start, run_len));
                // Only a range the kernel accepted is recorded, so the REUSE
                // that pairs with it is only ever issued against a range that
                // was really handed back.
                for (start, len) in runs {
                    if !advise(start, len) {
                        continue;
                    }
                    let mut addr = start;
                    while addr < start + len {
                        self.heap.reusable_blocks.insert(addr);
                        addr += BLOCK_SIZE;
                    }
                }
                if trace_map() {
                    let base = self.heap.memory.as_ptr() as usize;
                    for &addr in &hand_back {
                        eprintln!(
                            "[gc-map] HANDBACK {:#x}..{:#x}",
                            base + addr,
                            base + addr + BLOCK_SIZE
                        );
                    }
                }
            }
        }

        if occupancy_stats() && occ_blocks > 0 {
            let seq = GC_STATS.collections.load(Ordering::Relaxed) + 1;
            let pct = occ_marked as f64 / (occ_blocks * LINES_PER_BLOCK) as f64 * 100.0;
            eprintln!(
                "[gc-occ] #{seq} retained={occ_blocks} blocks ({:.1}MB) marked_lines={occ_marked} \
                 ({:.1}MB, {pct:.1}% full)  by-marked-lines: 1={} 2-4={} 5-16={} 17-64={} 65-192={} 193+={}",
                (occ_blocks * BLOCK_SIZE) as f64 / 1048576.0,
                (occ_marked * LINE_SIZE) as f64 / 1048576.0,
                occ_hist[0],
                occ_hist[1],
                occ_hist[2],
                occ_hist[3],
                occ_hist[4],
                occ_hist[5],
            );
        }

        freed.len()
    }

    pub fn register_global(&mut self, ptr: *mut hl::vdynamic) {
        self.roots.borrow_mut().globals.push(ptr);
    }

    pub fn push_stack_root(&mut self, ptr: *mut hl::vdynamic) {
        self.roots.borrow_mut().stack_roots.push(ptr);
    }

    pub fn pop_stack_root(&mut self) {
        self.roots.borrow_mut().stack_roots.pop();
    }

    pub fn register_persistent(&mut self, ptr: *mut hl::vdynamic) {
        self.roots.borrow_mut().persistent_roots.insert(ptr);
    }

    /// Root the object a native slot currently points at, and keep doing so as
    /// its contents change. See `RootSet::root_slots`.
    pub fn add_root_slot(&mut self, slot: usize) {
        self.roots.borrow_mut().root_slots.insert(slot);
    }

    pub fn remove_root_slot(&mut self, slot: usize) {
        self.roots.borrow_mut().root_slots.remove(&slot);
    }

    /// Which of the two root kinds an address was filed under: a slot is
    /// dereferenced on every mark, a persistent root is marked directly.
    pub fn has_root_slot(&self, slot: usize) -> bool {
        self.roots.borrow().root_slots.contains(&slot)
    }

    pub fn has_persistent(&self, ptr: *mut hl::vdynamic) -> bool {
        self.roots.borrow().persistent_roots.contains(&ptr)
    }

    pub fn unregister_persistent(&mut self, ptr: *mut hl::vdynamic) {
        self.roots.borrow_mut().persistent_roots.remove(&ptr);
    }

    /// A counted root for `ptr`; see [`handle_new`].
    pub fn handle_new(&mut self, ptr: *mut u8) -> Handle {
        self.handles.insert(ptr)
    }

    /// The pointer behind a live handle, null for a released or null one.
    pub fn handle_get(&self, h: Handle) -> *mut u8 {
        self.handles.get(h)
    }

    pub fn handle_retain(&mut self, h: Handle) {
        self.handles.retain(h);
    }

    pub fn handle_release(&mut self, h: Handle) {
        self.handles.release(h);
    }

    /// The address behind every live handle: the roots a hosted collector
    /// adds to its own, since a handle held by another language is the
    /// only reference to an object of its heap that crossed out.
    pub fn for_each_handle(&self, mut f: impl FnMut(*mut u8)) {
        for slot in self.handles.slots.iter().filter(|s| s.refs != 0) {
            f(slot.ptr as *mut u8);
        }
    }

    /// Scan `len` bytes from `start` conservatively at every collection until
    /// unregistered. The memory must outlive the registration.
    pub fn register_root_range(&mut self, start: *const u8, len: usize) {
        if !start.is_null() && len != 0 {
            self.root_ranges.push((start as usize, len));
        }
    }

    /// Remove one registration of exactly `(start, len)`.
    pub fn unregister_root_range(&mut self, start: *const u8, len: usize) {
        let range = (start as usize, len);
        if let Some(i) = self.root_ranges.iter().position(|r| *r == range) {
            self.root_ranges.swap_remove(i);
        }
    }

    pub fn clear_scan_ranges(&mut self) {
        self.heap.safepoint_mode = true;
        clear_current_scan_ranges();
    }

    /// Register an interpreter root snapshot. This is the interpreter's
    /// safepoint: the snapshot is complete at this instant, so a deferred
    /// collection trigger is honored here.
    pub fn add_scan_range(&mut self, ptr: *const c_void, size: usize) {
        self.heap.safepoint_mode = true;
        if !ptr.is_null() && size != 0 {
            add_current_scan_range(ptr as usize, size);
        }
        // No pending-collection consumption here: a snapshot spans several add
        // calls, one per interpreter frame, and the interpreter calls
        // `scan_roots_done` when the set is complete.
    }

    /// The snapshot is complete: a deferred collection is honored now.
    pub fn scan_roots_done(&mut self) {
        publish_current_scan_ranges();
        if self.heap.collect_pending {
            set_collect_origin(1);
            self.collect_garbage();
        }
    }

    pub fn alloc_virtual(&mut self, t: *mut hl::hl_type) -> Option<NonNull<hl::vvirtual>> {
        unsafe {
            let virt = (*t).detail.virt;
            if virt.is_null() {
                return None;
            }

            let data_size = (*virt).dataSize;
            let nfields = (*virt).nfields;
            let total_size = std::mem::size_of::<hl::vvirtual>()
                + (nfields as usize * std::mem::size_of::<*mut std::os::raw::c_void>())
                + (data_size as usize);

            let ptr = self.allocate(total_size)?;
            let v = ptr.as_ptr() as *mut hl::vvirtual;

            // Initialize vvirtual struct
            (*v).t = t;
            (*v).value = std::ptr::null_mut();
            (*v).next = std::ptr::null_mut();

            // Calculate pointers to fields and vdata
            let fields = v.offset(1) as *mut *mut std::os::raw::c_void;
            let vdata = fields.add(nfields as usize) as *mut u8;

            // Initialize fields: each vfield[i] points to vdata + indexes[i]
            // indexes may be null if the virtual type hasn't been initialized yet
            // (the interpreter doesn't call hlp_init_virtual during setup).
            if !(*virt).indexes.is_null() {
                for i in 0..nfields as usize {
                    // indexes[i] stores absolute byte offset from start of allocation (v),
                    // NOT relative to vdata. Use v as base, not vdata.
                    let offset = *(*virt).indexes.add(i) as usize;
                    *fields.add(i) = (v as *mut u8).add(offset) as *mut std::os::raw::c_void;
                }
            } else {
                // No indexes available — zero all field pointers
                std::ptr::write_bytes(fields, 0, nfields as usize);
            }

            // Zero out vdata
            std::ptr::write_bytes(vdata, 0, data_size as usize);

            Some(NonNull::new_unchecked(v))
        }
    }
}

pub fn register_root(ptr: *mut hl::vdynamic) {
    if ptr.is_null() {
        return;
    }
    let mut gc = gc_locked();
    gc.register_persistent(ptr);
}

pub fn zalloc(size: i32) -> *mut std::os::raw::c_void {
    if size < 0 {
        return ptr::null_mut();
    }

    let size_usize = size as usize;

    // gc_alloc returns zeroed memory, so zeroing again here is redundant
    // and would run outside the allocator's lock.
    match gc_alloc(size_usize) {
        Some(ptr) => ptr.as_ptr() as *mut std::os::raw::c_void,
        None => ptr::null_mut(),
    }
}

pub fn mark_size(data_size: i32) -> i32 {
    let data_size = data_size as usize;
    let ptr_count = data_size.div_ceil(HL_WSIZE);
    (((ptr_count + 31) >> 5) * std::mem::size_of::<i32>() as usize)
        .try_into()
        .unwrap()
}

/// Walk all live heap objects, calling `visitor(obj_ptr, type_ptr)` for each.
/// Used by hot-reload to propagate field updates to existing objects.
pub unsafe fn walk_heap(
    visitor: unsafe extern "C" fn(*mut hl::vdynamic, *mut hl::hl_type, *mut c_void),
    ctx: *mut c_void,
) {
    let _guard = gc_guard();
    let gc = match unsafe { (*(&raw mut GC)).get_mut() } {
        Some(g) => g,
        None => return,
    };
    let heap_base = gc.heap.memory.as_ptr() as usize;

    for &block_addr in &gc.heap.used_blocks {
        let block_offset = block_addr - heap_base;
        let first_line = block_offset / LINE_SIZE;

        let mut line = first_line;
        let block_end_line = first_line + LINES_PER_BLOCK;

        while line < block_end_line {
            let alloc_lines = gc.heap.alloc_sizes[line] as usize;
            if alloc_lines == 0 {
                line += 1;
                continue;
            }

            let obj_addr = heap_base + line * LINE_SIZE;
            let obj = obj_addr as *mut hl::vdynamic;

            // Validate: first field must be a type pointer
            unsafe {
                if !(*obj).t.is_null() {
                    visitor(obj, (*obj).t, ctx);
                }
            }

            line += alloc_lines;
        }
    }
}

/// Byte span the collector reserved for the allocation containing `ptr`, from
/// the same lookup the marker uses. Zero for foreign pointers, free space and
/// padding.
pub unsafe fn allocation_size(ptr: *const c_void) -> usize {
    let gc = gc_locked_init();
    let base = gc.heap.memory.as_ptr() as usize;
    let addr = ptr as usize;
    if addr < base || addr >= base + gc.heap.memory.len {
        return 0;
    }
    allocation_at(
        &gc.blocks,
        &gc.heap.alloc_sizes,
        &gc.heap.objects,
        addr - base,
    )
    .map_or(0, |(_, size)| size)
}

/// Give the collector a chance to stop this thread, so a long native loop
/// that never allocates can still be stopped. A thread the collector was
/// never told about passes straight through.
pub fn safepoint() {
    gc_safepoint();
}

/// Initialize the garbage collector. Must be called before any allocation.
pub fn init() {
    gc_locked_init();
}

/// Record the stack top for conservative scanning.
/// Called once at JIT entry before running user code.
pub unsafe fn set_stack_top(top: usize) {
    register_current_mutator(top, "runtime");
}

/// HashLink-compatible mutator registration for HDLL-created worker threads;
/// `set_stack_top` is the host-runtime spelling of the same registry entry.
pub unsafe fn register_thread(stack_top: *mut c_void) {
    // A thread a native library started reaches a safepoint only by calling
    // back into the runtime or marking itself blocking.
    register_current_mutator(stack_top as usize, "hdll");
}

pub fn unregister_thread() {
    let thread = thread_self_fast();
    unregister_current_mutator();
    let mut gc = gc_locked_init();
    gc.fiber_stacks.retain(|fiber| fiber.thread != thread);
}

/// Register the globals_data array for conservative scanning.
/// Called after init_constants with pointer to globals array and count.
pub unsafe fn set_globals(ptr: *const *mut c_void, count: usize) {
    let mut gc = gc_locked();
    gc.globals_range = Some((ptr, count));
}

/// Clear interpreter-provided conservative scan ranges.
pub fn scan_roots_done() {
    let mut gc = gc_locked();
    gc.scan_roots_done();
}

pub fn clear_scan_roots() {
    let mut gc = gc_locked();
    gc.clear_scan_ranges();
}

/// Add an interpreter-provided conservative scan range.
pub unsafe fn add_scan_root(ptr: *const c_void, size: usize) {
    let mut gc = gc_locked();
    gc.add_scan_range(ptr, size);
}

/// Hand the collector a live view of this mutator's scan-range table. Called
/// once; afterwards the mutator maintains the table and the collector reads
/// it when it stops the world.
///
/// # Safety
/// `ranges` and `len` must stay valid while the mutator is registered.
pub unsafe fn set_scan_roots_live(ranges: *const (usize, usize), len: *const usize) {
    // Same signal the copying publish gives, so the deferral branch runs and
    // collections stay batched.
    let mut gc = gc_locked();
    gc.heap.safepoint_mode = true;
    let thread = thread_self_fast();
    let mut world = MUTATOR_WORLD.state.lock().unwrap();
    if let Some(record) = world.mutators.iter_mut().find(|m| m.thread == thread) {
        record.scan_live = Some((ranges as usize, len as usize));
    }
}

pub unsafe fn set_scan_roots(ranges: *const (usize, usize), count: usize) {
    let mut gc = gc_locked();
    gc.heap.safepoint_mode = true;
    let ranges: &[(usize, usize)] = if ranges.is_null() || count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ranges, count) }
    };
    set_current_scan_ranges(ranges);
    // The world lock is released by now, and must be: honouring a deferred
    // collection reaches stop_mutator_world, which takes it again.
    if gc.heap.collect_pending {
        set_collect_origin(1);
        gc.collect_garbage();
    }
}

/// Charge off-heap memory (fiber stacks, native buffers, JIT structures) as
/// GC allocation pressure. The charge participates in the byte-driven
/// collection trigger and resets after every collection.
pub fn track_external(bytes: u64) {
    let mut gc = gc_locked_init();
    gc.track_external(bytes as usize);
}

/// Upstream `hl_gc_enable`: flip the automatic collector on or off. Only the
/// trigger is suppressed: `major`, the exhaustion backstop and the
/// runaway-pressure escape hatch still collect. Takes no lock.
pub fn enable(b: bool) {
    GC_ENABLED.store(b, Ordering::Relaxed);
}

/// `hl.Gc.flags` getter.
pub fn get_flags() -> i32 {
    GC_FLAGS.load(Ordering::Relaxed)
}

/// `hl.Gc.flags` setter. The word is stored whole. `Profile` prints the
/// per-cycle census; the other bits are stored and reported without acting,
/// since this collector has no minor/major split, no scan to skip and no
/// in-allocator dump.
pub fn set_flags(f: i32) {
    GC_FLAGS.store(f, Ordering::Relaxed);
}

/// Upstream `hl_gc_major`: collect now, whatever the trigger thinks and
/// whether or not the collector is enabled. The calling thread becomes the
/// collector; the lock is reentrant, so a caller already holding it gets its
/// collection.
pub fn major() {
    let mut gc = gc_locked_init();
    set_collect_origin(6); // "explicit" — see ORIGIN_NAMES
    gc.collect_garbage();
}

/// Upstream `hl_gc_stats`: three counters through out-params, as doubles.
///
/// * `total_allocated`: `GC_STATS.bytes_allocated`, cumulative, bytes handed
///   out (a TLAB refill charges its whole region); external memory excluded.
/// * `allocation_count`: 0. The TLAB fast path counts nothing, and
///   `heap.alloc_count` resets every collection, so it would shrink between
///   samples.
/// * `current_memory`: blocks currently handed out, at block granularity.
///
/// Any out-param may be NULL.
pub unsafe fn stats(
    total_allocated: *mut f64,
    allocation_count: *mut f64,
    current_memory: *mut f64,
) {
    if !total_allocated.is_null() {
        unsafe { *total_allocated = GC_STATS.bytes_allocated.load(Ordering::Relaxed) as f64 };
    }
    if !allocation_count.is_null() {
        unsafe { *allocation_count = 0.0 };
    }
    if !current_memory.is_null() {
        let gc = gc_locked_init();
        unsafe { *current_memory = (gc.heap.used_blocks.len() * BLOCK_SIZE) as f64 };
    }
}

/// Upstream `hl_gc_profile`: the `Profile` flag as a function, on the same
/// `GC_FLAGS` word as [`set_flags`]. `false` clears that bit and nothing else.
pub fn profile(b: bool) {
    if b {
        GC_FLAGS.fetch_or(GC_FLAG_PROFILE, Ordering::Relaxed);
    } else {
        GC_FLAGS.fetch_and(!GC_FLAG_PROFILE, Ordering::Relaxed);
    }
}

/// Upstream `hl_gc_get_live_objects`. Returns -1, upstream's "cannot answer",
/// and leaves `arr` untouched: nothing here records an allocation's type, and
/// a marked line is not an object address. 0 would claim there are none.
pub fn get_live_objects(_t: *mut hl_type, _arr: *mut hl::varray) -> i32 {
    -1
}

/// Read a NUL-terminated UTF-8 C string, bounded. UTF-8 because
/// `hl.Gc.dumpMemory` passes `fileName.toUtf8()`.
unsafe fn c_utf8_path(p: *const hl::vbyte) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut len = 0usize;
    while len < 4096 && unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    if len == 0 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(p, len) };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Upstream `hl_gc_dump_memory`: mark, then write the heap out as
/// self-describing text under its own magic, not HashLink's `HMD1`. Nothing is
/// swept, and the mark bits are left standing: a mark bit only ever retains a
/// block, and the next sweep clears them all.
pub unsafe fn dump_memory(filename: *mut hl::vbyte) {
    use std::io::Write;

    let path = unsafe { c_utf8_path(filename) }.unwrap_or_else(|| "hlmemory.dump".to_string());
    let Ok(file) = std::fs::File::create(&path) else {
        eprintln!("[gc] dump_memory: cannot create {path}");
        return;
    };
    let mut out = std::io::BufWriter::new(file);

    let mut gc = gc_locked_init();
    let stopped_world = stop_mutator_world();

    // Before the mutator has entered user code there is no stack to scan
    // conservatively, and marking from a partial root set would report
    // live data as garbage.
    let marked = !stopped_world.snapshots.is_empty();
    if marked {
        gc.mark_roots(&stopped_world.snapshots);
    }

    let heap_base = gc.heap.memory.as_ptr() as usize;
    let heap_len = gc.heap.memory.len;
    let roots = gc.roots.borrow();

    let mut w = |line: String| {
        let _ = writeln!(out, "{line}");
    };
    w("ASHMEM1 ash-immix".into());
    w(format!("pointer-size {}", mem::size_of::<usize>()));
    w(format!("heap-base {heap_base:#x}"));
    w(format!("heap-size {heap_len}"));
    w(format!("block-size {BLOCK_SIZE}"));
    w(format!("line-size {LINE_SIZE}"));
    w(format!("blocks-total {}", heap_len / BLOCK_SIZE));
    w(format!("blocks-used {}", gc.heap.used_blocks.len()));
    w(format!("blocks-free {}", gc.heap.free_blocks.len()));
    w(format!("blocks-reusable {}", gc.heap.reusable_blocks.len()));
    if gc.heap.tlab_blocks.is_empty() {
        w("tlab-blocks none".into());
    } else {
        let mut blocks: Vec<usize> = gc.heap.tlab_blocks.values().copied().collect();
        blocks.sort_unstable();
        let rendered: Vec<String> = blocks
            .iter()
            .map(|b| format!("{:#x}", heap_base + b))
            .collect();
        w(format!("tlab-blocks {}", rendered.join(" ")));
    }
    w(format!("alloc-count {}", gc.heap.alloc_count));
    w(format!("bytes-since-gc {}", gc.heap.bytes_since_gc));
    w(format!("external-since-gc {}", gc.heap.external_since_gc));
    w(format!("trigger-threshold {}", gc.heap.trigger_threshold));
    w(format!(
        "collector-enabled {}",
        GC_ENABLED.load(Ordering::Relaxed)
    ));
    w(format!("safepoint-mode {}", gc.heap.safepoint_mode));
    w(format!(
        "collections {}",
        GC_STATS.collections.load(Ordering::Relaxed)
    ));
    w(format!(
        "blocks-reclaimed {}",
        GC_STATS.blocks_reclaimed.load(Ordering::Relaxed)
    ));
    w(format!(
        "bytes-allocated {}",
        GC_STATS.bytes_allocated.load(Ordering::Relaxed)
    ));
    w(format!(
        "external-bytes {}",
        GC_STATS.external_bytes.load(Ordering::Relaxed)
    ));
    w(format!(
        "pause-ns-total {}",
        GC_STATS.pause_ns_total.load(Ordering::Relaxed)
    ));
    w(format!(
        "pause-ns-max {}",
        GC_STATS.pause_ns_max.load(Ordering::Relaxed)
    ));
    w(format!("roots-globals {}", roots.globals.len()));
    w(format!("roots-stack {}", roots.stack_roots.len()));
    w(format!("roots-persistent {}", roots.persistent_roots.len()));
    w(format!("scan-ranges {}", mutator_scan_range_count()));
    w(format!("marked {marked}"));

    // One line per retained block: address, live lines, live bytes. Line marks
    // are the finest liveness recorded.
    w("# block <addr> <live-lines> <live-bytes>".into());
    let mut used: Vec<usize> = gc.heap.used_blocks.iter().copied().collect();
    used.sort_unstable();
    for block_addr in used {
        let live = gc.blocks[block_addr / BLOCK_SIZE].marked_line_count();
        w(format!(
            "block {:#x} {live} {}",
            heap_base + block_addr,
            live * LINE_SIZE
        ));
    }
    w("end".into());

    drop(roots);
    let _ = out.flush();
}

// ── Fiber-stack registry ────────────────────────────────────────────────────

pub unsafe fn gc_register_fiber_stack(id: u32, base: usize, size: usize) {
    let thread = thread_self_fast();
    let mut gc = gc_locked();
    // Lazily register the main-stack descriptor the first time a fiber
    // appears, so mark_roots can scan the suspended main stack.
    if !gc
        .fiber_stacks
        .iter()
        .any(|f| f.thread == thread && f.id == 0)
    {
        gc.fiber_stacks.push(FiberStackInfo {
            thread,
            id: 0,
            base: 0,
            size: 0,
            saved_sp: 0,
        });
    }
    gc.fiber_stacks.push(FiberStackInfo {
        thread,
        id,
        base,
        size,
        saved_sp: 0,
    });
}

pub unsafe fn gc_update_fiber_sp(id: u32, sp: usize) {
    let thread = thread_self_fast();
    let mut gc = gc_locked();
    if let Some(f) = gc
        .fiber_stacks
        .iter_mut()
        .find(|f| f.id == id && (id != 0 || f.thread == thread))
    {
        f.saved_sp = sp;
    }
}

/// Must be called BEFORE the fiber's stack memory is freed.
pub unsafe fn gc_unregister_fiber_stack(id: u32) {
    let thread = thread_self_fast();
    let mut gc = gc_locked();
    gc.fiber_stacks
        .retain(|f| f.id != id || (id == 0 && f.thread != thread));
}

pub unsafe fn gc_add_persistent(ptr: *mut hl::vdynamic) {
    let gc = gc_locked();
    gc.roots.borrow_mut().persistent_roots.insert(ptr);
}

pub unsafe fn gc_remove_persistent(ptr: *mut hl::vdynamic) {
    let gc = gc_locked();
    gc.roots.borrow_mut().persistent_roots.remove(&ptr);
}

// ── Handles and root ranges ─────────────────────────────────────────────────

/// Root `ptr` behind a handle with one reference: what a plugin or adapter
/// holds across calls instead of a raw pointer the scanner cannot see. A null
/// pointer gives `Handle::NULL`. Initialises the heap on first use.
pub fn handle_new(ptr: *mut u8) -> Handle {
    gc_locked_init().handle_new(ptr)
}

/// The pointer behind `h`; null once every reference is released, or for
/// `Handle::NULL`. The caller must hold the result where a collection can
/// see it, or keep the handle.
pub fn handle_get(h: Handle) -> *mut u8 {
    if h.is_null() {
        return ptr::null_mut();
    }
    gc_locked_init().handle_get(h)
}

/// One more reference to `h`'s slot.
pub fn handle_retain(h: Handle) {
    if h.is_null() {
        return;
    }
    gc_locked_init().handle_retain(h);
}

/// One reference fewer; at zero the slot is free and the object no longer
/// rooted by it.
pub fn handle_release(h: Handle) {
    if h.is_null() {
        return;
    }
    gc_locked_init().handle_release(h);
}

/// Scan `[start, start + len)` conservatively at every collection until
/// [`unregister_root_range`]: a linked spoke's data section, a module's
/// variable array. The memory must stay mapped while registered.
pub unsafe fn register_root_range(start: *const u8, len: usize) {
    gc_locked_init().register_root_range(start, len);
}

pub unsafe fn unregister_root_range(start: *const u8, len: usize) {
    gc_locked_init().unregister_root_range(start, len);
}

// ── For a hosted collector ──────────────────────────────────────────────────
//
// A runtime that keeps its own collector over this heap marks its objects
// with the same claim the marker uses, then reclaims what it did not claim and
// ends with a collection here, whose sweep clears the claims and returns the
// lines. Each takes the lock; a host inside a cycle holds it already.

/// The allocation containing `addr` as `(start, size)`, interior pointers
/// included; `None` for anything that is not inside a live allocation.
pub fn containing_allocation(addr: usize) -> Option<(usize, usize)> {
    gc_locked_init().allocation_containing(addr)
}

/// Claim the allocation containing `ptr` for the open cycle; true for the
/// claimer, false when it already was or `ptr` is in no allocation.
pub fn claim_for_cycle(ptr: *const u8) -> bool {
    gc_locked_init().claim_for_cycle(ptr)
}

/// Whether the allocation containing `ptr` is claimed in the open cycle.
pub fn is_claimed(ptr: *const u8) -> bool {
    gc_locked_init().is_claimed(ptr)
}

/// Forget the allocation that starts at `start`, whose owner has released
/// what it held: its lines return with the next sweep.
///
/// # Safety
/// `start` must be the start of an allocation nothing will read again.
pub unsafe fn free_allocation(start: *const u8) {
    gc_locked_init().forget_allocation(start);
}

/// Whether the automatic trigger is due: allocated plus external bytes since
/// the last collection have reached the threshold, or `ASH_GC_STRESS` is set
/// and anything was allocated. No lock, and no heartbeat.
pub fn should_collect() -> bool {
    let allocated = GC_STATS.bytes_allocated.load(Ordering::Relaxed);
    if gc_stress_every() > 0 {
        return allocated > ALLOCATED_AT_COLLECT.load(Ordering::Relaxed);
    }
    allocated + GC_STATS.external_bytes.load(Ordering::Relaxed)
        >= NEXT_TRIGGER_AT.load(Ordering::Relaxed)
}

/// Whether a trigger fired inside a deferring mutator's allocation and no
/// collection has run since: someone should collect at their next
/// safepoint. No lock.
pub fn collect_pending() -> bool {
    COLLECT_PENDING.load(Ordering::Relaxed)
}

/// Whether a collector is waiting for the world to stop. A registered
/// mutator that sees this parks by calling [`gc_safepoint`]. No lock.
pub fn stop_requested() -> bool {
    GC_STOP_REQUESTED.load(Ordering::Acquire)
}

/// Whether the current thread is a registered mutator.
pub fn thread_registered() -> bool {
    current_mutator_registered()
}

/// Defer the current mutator's byte-driven trigger: while set, a trigger
/// that fires inside this thread's allocation records [`collect_pending`]
/// instead of collecting there, and the collection runs at the next
/// safepoint any mutator reaches. For a hosted collector whose roots are
/// complete only where it polls. Cleared by `unregister_thread`.
pub fn set_deferred_collection(on: bool) {
    TLAB.with(|t| t.deferred.set(on));
}

/// Collections completed so far, abandoned stops excluded.
pub fn collections() -> u64 {
    GC_STATS.collections.load(Ordering::Relaxed)
}

/// `hl_gc_alloc_gen`: `size` zeroed bytes of the kind in `flags`. Word zero
/// belongs to the caller except for `Typed`, which receives `t`; `Finalizer`
/// blocks are recorded so the callback the caller stores there runs. The
/// kind is recorded beside the size: `NoPtr` is never scanned, and `Typed`
/// with `mem::TRACED` is traced and dropped through the `TypeDesc` at `t`.
/// Initialises the heap on first use, as HashLink does.
pub unsafe fn alloc_gen(t: *mut hl_type, size: usize, flags: u32) -> *mut c_void {
    use caribou_abi::mem::{AllocKind, TRACED};
    let kind = AllocKind::from_flags(flags);
    // A typed, raw or pointer-free allocation whose descriptor has no drop
    // hook needs the lock for nothing: it bumps through the thread's buffer
    // as `gc_alloc` does, and its kind is one byte in the side table, which
    // nothing reads until this thread next parks. A drop hook is recorded
    // per block, under the lock; a finalizer is registered there too.
    let traced = kind == AllocKind::Typed && flags & TRACED != 0;
    let has_drop = traced && unsafe { (*(t as *const TypeDesc)).drop.is_some() };
    if kind != AllocKind::Finalizer && !has_drop {
        let Some(ptr) = gc_alloc(size) else {
            return ptr::null_mut();
        };
        let p = ptr.as_ptr();
        if kind == AllocKind::Typed {
            unsafe { (*(p as *mut hl::vdynamic)).t = t };
        }
        let mark = match kind {
            AllocKind::Typed if traced => OBJECT_KIND_TRACED,
            AllocKind::NoPtr => OBJECT_KIND_NOPTR,
            _ => 0,
        };
        if mark != 0 {
            // The side table through the buffer's own pointer to it, which a
            // thread has once it has refilled; else under the lock.
            let (objects, base) = TLAB.with(|tl| (tl.objects.get(), tl.heap_base.get()));
            if !objects.is_null() {
                let index = (p as usize - base) / ALLOC_QUANTUM;
                unsafe { (*objects.add(index)).fetch_or(mark, Ordering::Relaxed) };
            } else {
                let mut gc = gc_locked_init();
                let offset = p as usize - gc.heap.memory.as_ptr() as usize;
                gc.set_allocation_kind(offset, mark);
            }
        }
        return p as *mut c_void;
    }
    let mut gc = gc_locked_init();
    let Some(ptr) = gc.allocate(size) else {
        return ptr::null_mut();
    };
    let p = ptr.as_ptr();
    let offset = p as usize - gc.heap.memory.as_ptr() as usize;
    match kind {
        AllocKind::Typed => {
            unsafe { (*(p as *mut hl::vdynamic)).t = t };
            if flags & TRACED != 0 {
                gc.set_allocation_kind(offset, OBJECT_KIND_TRACED);
            }
        }
        AllocKind::NoPtr => gc.set_allocation_kind(offset, OBJECT_KIND_NOPTR),
        AllocKind::Finalizer => gc.register_finalizable(p),
        AllocKind::Raw => {}
    }
    p as *mut c_void
}

#[cfg(test)]
mod tests {
    #[test]
    fn widened_stack_scan_start_preserves_word_alignment() {
        // A byte-aligned anchor below a word-aligned suspended SP.
        assert_eq!(stack_scan_start(0x7fddd4, 0x7fffff, 1024 * 1024), 0x700000);
        // A probe below the window still wins; a zero window is the native
        // and ordinary fiber-stack case. Widening must not underflow.
        assert_eq!(
            stack_scan_start(0x600001, 0x7fffff, 1024 * 1024),
            word_align_up(0x600001)
        );
        assert_eq!(
            stack_scan_start(0x7fddd4, 0x7fffff, 0),
            word_align_up(0x7fddd4)
        );
        assert_eq!(stack_scan_start(128, 255, 1024 * 1024), 0);
    }

    #[test]
    fn widened_main_stack_scan_traces_callback_chain() {
        // Exercise every possible byte alignment of the stack-top anchor,
        // using the actual collector and a root below the saved probe.
        for residue in 0..WORD {
            let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
            let owner = gc.allocate(16).unwrap();
            let callback = gc.allocate(16).unwrap();
            let unrelated = gc.allocate(16).unwrap();
            unsafe {
                owner
                    .cast::<usize>()
                    .as_ptr()
                    .write(callback.as_ptr() as usize)
            };
            let mut stack = [0usize; 16];
            stack[8] = owner.as_ptr() as usize;
            let base = stack.as_ptr() as usize;
            let top = base + 15 * WORD + residue;
            let start = stack_scan_start(base + 14 * WORD, top, 8 * WORD);
            // Assert before scanning: a misaligned start would be an unaligned dereference.
            assert_eq!(start % WORD, 0, "stack-top remainder {residue}");
            assert!((base + 7 * WORD..=base + 8 * WORD).contains(&start));
            let roots = gc.conservative_scan_range(start, top);
            gc.conservative_trace(roots);
            assert!(object_marked(&gc, offset(&gc, owner)));
            assert!(object_marked(&gc, offset(&gc, callback)));
            assert!(!object_marked(&gc, offset(&gc, unrelated)));
        }
    }

    #[test]
    fn tlab_bumps_publish_object_bounds_and_skip_line_tails() {
        if !tlab_enabled() {
            return; // Stress mode intentionally disables this allocation path.
        }
        // A fresh thread owns the test's TLS; no pointer into this isolated
        // heap survives into another test or the process-wide allocator.
        std::thread::spawn(|| {
            let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
            let block = gc.acquire_free_block().unwrap();
            let base = gc.heap.memory.as_ptr() as usize;
            adopt_tlab_region(&mut gc, block, base + block, base + block + BLOCK_SIZE);
            TLAB.with(|t| t.registered.set(true));
            let a = gc_alloc(80).unwrap();
            let b = gc_alloc(64).unwrap();
            let c = gc_alloc(16).unwrap();
            assert_eq!(offset(&gc, a), block);
            assert_eq!(offset(&gc, b), block + LINE_SIZE);
            assert_eq!(offset(&gc, c), block + LINE_SIZE + 64);
            let find = |at| allocation_at(&gc.blocks, &gc.heap.alloc_sizes, &gc.heap.objects, at);
            assert_eq!(find(block + 79), Some((block, 80)));
            assert_eq!(find(block + 80), None, "the skipped tail is not an object");
            assert_eq!(find(block + LINE_SIZE + 63), Some((block + LINE_SIZE, 64)));
            assert_eq!(
                find(block + LINE_SIZE + 64),
                Some((block + LINE_SIZE + 64, 16))
            );
            release_tlab_region(&mut gc);
            TLAB.with(|t| t.registered.set(false));
        })
        .join()
        .unwrap();
    }

    fn offset(gc: &ImmixAllocator, p: NonNull<u8>) -> usize {
        p.as_ptr() as usize - gc.heap.memory.as_ptr() as usize
    }

    fn object_marked(gc: &ImmixAllocator, at: usize) -> bool {
        gc.heap.objects[at / ALLOC_QUANTUM].load(Ordering::Relaxed) & OBJECT_MARK != 0
    }

    #[test]
    fn packed_neighbours_do_not_form_a_retention_chain() {
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let mut buffers = Vec::new();
        let mut neighbours = Vec::new();
        for _ in 0..64 {
            buffers.push(gc.allocate(80).unwrap());
            gc.allocate(16).unwrap(); // empty array
            neighbours.push(gc.allocate(16).unwrap()); // next Haxe Array
            gc.allocate(16).unwrap();
        }
        for i in 0..63 {
            unsafe {
                neighbours[i]
                    .cast::<usize>()
                    .as_ptr()
                    .write(buffers[i + 1].as_ptr() as usize)
            };
        }
        let mut work = Vec::new();
        // An interior conservative pointer, like a numeric word overlapping
        // the wasm heap, may retain ONE buffer, not every neighbour's graph.
        gc.mark_allocation(offset(&gc, buffers[0]) + 7, &mut work);
        gc.conservative_trace(work);
        assert!(object_marked(&gc, offset(&gc, buffers[0])));
        for i in 0..64 {
            assert!(!object_marked(&gc, offset(&gc, neighbours[i])));
            if i > 0 {
                assert!(!object_marked(&gc, offset(&gc, buffers[i])));
            }
        }

        // A second REAL root on that already-marked line must still trace.
        let mut work = Vec::new();
        gc.mark_allocation(offset(&gc, neighbours[0]), &mut work);
        gc.conservative_trace(work);
        assert!(object_marked(&gc, offset(&gc, buffers[1])));
        assert!(!object_marked(&gc, offset(&gc, neighbours[1])));
        assert!(!object_marked(&gc, offset(&gc, buffers[2])));
    }

    #[test]
    fn allocation_lookup_checks_bounds_and_cross_block_spans() {
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 8);
        let p = gc.allocate(160).unwrap();
        let start = offset(&gc, p);
        let small = gc.allocate(16).unwrap();
        let next = offset(&gc, small);
        let find = |at| allocation_at(&gc.blocks, &gc.heap.alloc_sizes, &gc.heap.objects, at);
        assert_eq!(find(start + 159), Some((start, 256)));
        assert_eq!(find(next + 15), Some((next, 16)));
        assert_eq!(find(next + 16), None, "not an earlier span or a neighbour");
        let large = gc.allocate(BLOCK_SIZE + 144).unwrap();
        let begin = offset(&gc, large);
        let mut work = Vec::new();
        gc.mark_allocation(begin + BLOCK_SIZE + 140, &mut work);
        assert_eq!(work, vec![(begin, BLOCK_SIZE + 256)]);
        assert!(gc.blocks[begin / BLOCK_SIZE + 1].is_marked(1));
        assert!(!gc.blocks[begin / BLOCK_SIZE + 1].is_marked(2));
    }

    #[test]
    fn reused_lines_forget_old_spans_and_object_claims_reset() {
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let p = gc.allocate(256).unwrap();
        let start = offset(&gc, p);
        gc.clear_allocation_metadata(start, 256);
        // A TLAB refilling this span records small starts, not the old span.
        gc.record_allocation(start + 32, 16);
        let mut work = Vec::new();
        gc.mark_allocation(start + 36, &mut work);
        assert_eq!(work, vec![(start + 32, 16)]);
        assert!(!gc.blocks[start / BLOCK_SIZE].is_marked(1));
        gc.sweep(&[]);
        assert!(!object_marked(&gc, start + 32));
        let mut again = Vec::new();
        gc.mark_allocation(start + 36, &mut again);
        assert_eq!(again, work);
        gc.clear_allocation_metadata(start, BLOCK_SIZE);
        assert_eq!(
            allocation_at(
                &gc.blocks,
                &gc.heap.alloc_sizes,
                &gc.heap.objects,
                start + 36
            ),
            None
        );
    }

    #[test]
    fn parallel_markers_claim_each_object_once_even_on_the_same_line() {
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let a = gc.allocate(16).unwrap();
        let b = gc.allocate(16).unwrap();
        let (a, b) = (offset(&gc, a), offset(&gc, b));
        let blocks = &gc.blocks;
        let sizes = &gc.heap.alloc_sizes;
        let objects = &gc.heap.objects;
        let total = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(move || {
                        let mut work = Vec::new();
                        mark_allocation_shared(blocks, sizes, objects, a, &mut work);
                        mark_allocation_shared(blocks, sizes, objects, b, &mut work);
                        work.len()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|w| w.join().unwrap())
                .sum::<usize>()
        });
        assert_eq!(total, 2);
    }

    /// The bump allocator finds a line boundary from the absolute address and the
    /// sweep from the offset into the heap; they agree only while the base is a
    /// multiple of `LINE_SIZE`.
    #[test]
    fn the_heap_base_is_line_aligned() {
        let heap = HeapMemory::new(BLOCK_SIZE * 4);
        let base = heap.as_ptr() as usize;
        assert_eq!(
            base % LINE_SIZE,
            0,
            "heap base {base:#x} is not a multiple of LINE_SIZE ({LINE_SIZE}); \
             the bump path and the sweep would disagree about line boundaries \
             by {} bytes",
            base % LINE_SIZE
        );
    }

    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn collector_rendezvous_with_registered_os_mutator() {
        init();
        let main_stack_anchor = 0usize;
        unsafe {
            set_stack_top((&main_stack_anchor as *const usize as usize) + mem::size_of::<usize>())
        };

        let ready = Arc::new(AtomicBool::new(false));
        let finish = Arc::new(AtomicBool::new(false));
        let worker_ready = Arc::clone(&ready);
        let worker_finish = Arc::clone(&finish);
        let worker = std::thread::spawn(move || {
            let stack_anchor = 0usize;
            unsafe {
                register_thread(
                    ((&stack_anchor as *const usize as usize) + mem::size_of::<usize>())
                        as *mut c_void,
                )
            };
            worker_ready.store(true, Ordering::Release);
            while !worker_finish.load(Ordering::Acquire) {
                gc_safepoint();
                std::hint::spin_loop();
            }
            unregister_thread();
        });

        // Registered mutators, so both waits below are visible to a collector:
        // otherwise a collection asked for by another test stalls until its
        // deadline fires.
        while !ready.load(Ordering::Acquire) {
            gc_safepoint();
            std::thread::yield_now();
        }
        {
            let mut gc = gc_locked();
            set_collect_origin(6);
            gc.collect_garbage();
        }
        finish.store(true, Ordering::Release);
        // `join` cannot poll, so publish this thread as blocked for its
        // duration: the collector then scans it where it stands instead of
        // waiting for a safepoint it will not reach until the worker exits.
        let was_blocking = gc_set_blocking(true);
        worker.join().unwrap();
        gc_set_blocking(was_blocking);
        unregister_thread();
    }

    /// The four `hl.Gc` reporting primitives, held to their documented contract.
    ///
    /// One test rather than four: the collector is process-global and the
    /// harness runs tests on separate threads, so the assertions run in sequence
    /// on one thread, holding the GC lock for the whole body so no other test can
    /// collect or carve a block between two samples. The lock is reentrant, so
    /// each primitive's own `gc_locked_init` nests inside the hold.
    ///
    /// This thread registers as a mutator because the body keeps GC pointers in
    /// its own frame across a collection.
    #[test]
    fn gc_reporting_prims_keep_their_documented_contract() {
        init();
        let stack_anchor = 0usize;
        unsafe {
            set_stack_top((&stack_anchor as *const usize as usize) + mem::size_of::<usize>())
        };

        // Both are process-global and have to be put back even when an assertion
        // fails. The whole flag word is saved because the body sets a neighbouring
        // bit too.
        let saved_flags = get_flags();
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(gc_reporting_prims_body));
        set_flags(saved_flags);
        unregister_thread();
        // The registered stack top points just past this local, so it has to
        // outlive every collection the body runs.
        std::hint::black_box(&stack_anchor);
        if let Err(payload) = outcome {
            std::panic::resume_unwind(payload);
        }
    }

    /// Sequenced body of
    /// [`gc_reporting_prims_keep_their_documented_contract`], split out so the
    /// caller restores the globals it touches on the unwind path too.
    ///
    /// `inline(never)` is load-bearing: the scan covers `[sp, stack_top)`, so the
    /// root array must be in a frame below the registered anchor.
    #[inline(never)]
    fn gc_reporting_prims_body() {
        // 4KB is above `TLAB_MAX_OBJ`, so every chunk takes the locked path
        // and is charged its own bytes instead of 32KB at a time; 64 of them
        // is well under `INITIAL_TRIGGER_BYTES`, so the loop does not collect.
        const CHUNK: usize = 4096;
        const CHUNKS: usize = 64;
        const LIVE_BYTES: usize = CHUNK * CHUNKS;

        let gc = gc_locked_init();

        // ── stats writes all three out-params ──────────────────────
        // NaN is a poison none of the three can produce, so `is_finite` is
        // proof the store happened rather than that the value looks plausible.
        let (mut total0, mut count0, mut current0) = (f64::NAN, f64::NAN, f64::NAN);
        unsafe { stats(&mut total0, &mut count0, &mut current0) };
        assert!(
            total0.is_finite() && total0 >= 0.0,
            "total_allocated not written: {total0}"
        );
        assert!(count0.is_finite(), "allocation_count not written: {count0}");
        assert!(
            current0.is_finite() && current0 >= 0.0,
            "current_memory not written: {current0}"
        );

        // Asserted as the documented constant, so implementing it for real fails
        // here and the contract gets updated on purpose.
        assert_eq!(
            count0, 0.0,
            "allocation_count is documented as a constant 0.0"
        );

        // ── NULL is legal for any out-param ───────────────────────────────
        // An HDLL caller that wants one field passes NULL for the others; that
        // must not be a store to address 0.
        unsafe {
            stats(ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
            let mut one = f64::NAN;
            stats(&mut one, ptr::null_mut(), ptr::null_mut());
            assert!(one.is_finite(), "total_allocated alone: {one}");
            let mut one = f64::NAN;
            stats(ptr::null_mut(), &mut one, ptr::null_mut());
            assert_eq!(one, 0.0, "allocation_count alone: {one}");
            let mut one = f64::NAN;
            stats(ptr::null_mut(), ptr::null_mut(), &mut one);
            assert!(one.is_finite(), "current_memory alone: {one}");
        }

        // ── total_allocated across real allocation ────────────────────────
        // The pointers live in a stack array, not a Vec: conservative marking
        // scans this frame, and a Vec's buffer is on the malloc heap, which it
        // does not scan.
        let mut live = [ptr::null_mut::<u8>(); CHUNKS];
        for slot in live.iter_mut() {
            let p = gc_alloc(CHUNK).expect("GC heap exhausted allocating a test chunk");
            unsafe { p.as_ptr().write_bytes(0xA5, CHUNK) };
            *slot = p.as_ptr();
        }

        let (mut total1, mut count1, mut current1) = (f64::NAN, f64::NAN, f64::NAN);
        unsafe { stats(&mut total1, &mut count1, &mut current1) };
        assert_eq!(
            count1, 0.0,
            "allocation_count is documented as a constant 0.0"
        );
        // The contract is non-decreasing: the counter is cumulative and never
        // reset. No exact byte total is asserted — the allocator charges in
        // 32KB regions on the TLAB path, and other threads add to the same
        // counter — only that this much allocation moved it at least this far.
        assert!(
            total1 >= total0,
            "total_allocated went backwards: {total0} -> {total1}"
        );
        assert!(
            total1 - total0 >= LIVE_BYTES as f64,
            "{LIVE_BYTES} bytes of above-TLAB_MAX_OBJ allocation moved total_allocated by only {}",
            total1 - total0
        );

        // ── current_memory: non-zero, block-granular, under the ceiling ───
        // The ceiling is the heap reservation itself; `used_blocks` is a
        // subset of it by construction, so a figure above it would mean the
        // count and the unit had come apart.
        let ceiling = gc.heap.memory.len as f64;
        assert!(
            current1 > 0.0,
            "current_memory is 0 with {LIVE_BYTES} bytes live"
        );
        assert!(
            current1 <= ceiling,
            "current_memory {current1} exceeds the heap reservation {ceiling}"
        );
        assert!(
            current1 >= LIVE_BYTES as f64,
            "current_memory {current1} is under the {LIVE_BYTES} bytes the heap is holding"
        );
        assert_eq!(
            current1 as usize % BLOCK_SIZE,
            0,
            "current_memory is used blocks, so it is a multiple of BLOCK_SIZE: {current1}"
        );

        // ── major runs a real cycle ────────────────────────────────
        //
        // Retried: a cycle whose world stop misses `STOP_THE_WORLD_DEADLINE` is
        // abandoned, and this binary runs its tests in parallel, so another test's
        // thread can be the one that fails to park.
        let collections_before = GC_STATS.collections.load(Ordering::Relaxed);
        let mut collections_after = collections_before;
        for _ in 0..8 {
            major();
            collections_after = GC_STATS.collections.load(Ordering::Relaxed);
            if collections_after > collections_before {
                break;
            }
            std::thread::yield_now();
        }
        assert!(
            collections_after > collections_before,
            "major ran no cycle in 8 attempts: \
             collections {collections_before} -> {collections_after}"
        );
        // ...and it ran on its own account rather than riding a trigger that
        // happened to fire: 6 is `ORIGIN_NAMES`' "explicit", and nothing else
        // can have collected while this thread holds the lock.
        assert_eq!(
            COLLECT_ORIGIN.load(Ordering::Relaxed),
            6,
            "major should collect with the explicit origin"
        );

        let (mut total2, mut count2, mut current2) = (f64::NAN, f64::NAN, f64::NAN);
        unsafe { stats(&mut total2, &mut count2, &mut current2) };
        assert_eq!(
            count2, 0.0,
            "allocation_count is documented as a constant 0.0"
        );
        assert!(
            total2 >= total1,
            "a collection must not reset total_allocated: {total1} -> {total2}"
        );
        // Nothing can have carved a block while this thread holds the GC lock,
        // so a cycle can only hand blocks back.
        assert!(
            current2 <= current1,
            "current_memory grew across a collection: {current1} -> {current2}"
        );
        // And the chunks are still referenced from this frame on a registered
        // mutator, so the cycle keeps their blocks.
        assert!(
            current2 >= LIVE_BYTES as f64,
            "a collection dropped below the {LIVE_BYTES} still-reachable bytes: {current2}"
        );
        std::hint::black_box(&live);

        // ── profile sets and clears exactly one bit ────────────────
        // The neighbouring bit is what makes this a test: upstream's
        // `hl_gc_profile(false)` clears every other flag instead.
        const NEIGHBOUR: i32 = 1 << 4; // a bit `set_flags` stores without acting on
        set_flags(NEIGHBOUR);
        assert_eq!(
            get_flags() & GC_FLAG_PROFILE,
            0,
            "test setup left Profile set"
        );

        profile(true);
        assert_ne!(
            get_flags() & GC_FLAG_PROFILE,
            0,
            "gc_profile(true) did not set Profile"
        );
        assert_ne!(
            get_flags() & NEIGHBOUR,
            0,
            "gc_profile(true) disturbed a flag other than Profile"
        );

        profile(false);
        assert_eq!(
            get_flags() & GC_FLAG_PROFILE,
            0,
            "gc_profile(false) left Profile set, which is upstream's missing `~`"
        );
        assert_ne!(
            get_flags() & NEIGHBOUR,
            0,
            "gc_profile(false) cleared a flag other than Profile, which is upstream's `&= GC_PROFILE`"
        );

        // ── get_live_objects cannot answer, and says so ────────────
        // -1 is upstream's "cannot answer"; 0 would claim none are live.
        let mut ty: hl_type = unsafe { mem::zeroed() };
        let mut arr: hl::varray = unsafe { mem::zeroed() };
        assert_eq!(
            get_live_objects(&mut ty, &mut arr),
            -1,
            "gc_get_live_objects must report -1 (cannot answer), never 0 (none live)"
        );
        // `arr` is documented as left untouched, so it is never partially
        // filled behind a -1.
        assert_eq!(arr.size, 0, "gc_get_live_objects wrote into arr");
        // The arguments are not dereferenced, so a caller passing NULL for the
        // array it did not bother to allocate gets the same answer.
        assert_eq!(
            get_live_objects(ptr::null_mut(), ptr::null_mut()),
            -1,
            "gc_get_live_objects must tolerate NULL arguments"
        );

        drop(gc);
    }

    /// `PENDING_FINALIZERS` is process-wide and `run_pending_finalizers`
    /// drains ALL of it, so two of these tests running at once would each run
    /// the other's callbacks -- and the one that asserts its callback did NOT
    /// run would see the other's increment. They take this in turn instead.
    static FINALIZER_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn finalizer_test_turn() -> std::sync::MutexGuard<'static, ()> {
        FINALIZER_TEST.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Callback for the finalizer tests. Records the block it was handed
    /// without dereferencing it, so a queue left over from a failed assert
    /// cannot fault a later test.
    static FINALIZED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static FINALIZED_BLOCK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    unsafe extern "C" fn record_finalized(block: *mut c_void) {
        FINALIZED_BLOCK.store(block as usize, Ordering::SeqCst);
        FINALIZED.fetch_add(1, Ordering::SeqCst);
    }

    fn unmark(gc: &ImmixAllocator, at: usize) {
        gc.heap.objects[at / ALLOC_QUANTUM].fetch_and(!OBJECT_MARK, Ordering::Relaxed);
    }

    fn word0(p: NonNull<u8>) -> usize {
        unsafe { *(p.as_ptr() as *const usize) }
    }

    #[test]
    fn an_unreachable_finalizable_block_is_resurrected_then_finalized_once() {
        let _turn = finalizer_test_turn();
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let live = gc.allocate(16).unwrap();
        let dead = gc.allocate(16).unwrap();
        for p in [live, dead] {
            unsafe { (p.as_ptr() as *mut usize).write(record_finalized as *const () as usize) };
            gc.register_finalizable(p.as_ptr());
        }
        let (live_at, dead_at) = (offset(&gc, live), offset(&gc, dead));
        let before = FINALIZED.load(Ordering::SeqCst);

        // Only `live` is reachable.
        let mut work = Vec::new();
        gc.mark_allocation(live_at, &mut work);
        gc.conservative_trace(work);
        gc.take_dead_finalizers();

        assert!(
            gc.finalizables.contains(&live_at) && !gc.finalizables.contains(&dead_at),
            "the pass must keep the reachable block and take the unreachable one"
        );
        assert!(
            object_marked(&gc, dead_at),
            "an unreachable finalizable block must be marked, or sweep recycles \
             the lines its finalizer is about to read"
        );
        assert_eq!(
            word0(dead),
            record_finalized as *const () as usize,
            "word zero must still be set when the callback runs: upstream \
             finalizers guard on it"
        );
        assert_eq!(
            word0(live),
            record_finalized as *const () as usize,
            "a reachable block keeps its callback for a later cycle"
        );
        assert_eq!(
            FINALIZED.load(Ordering::SeqCst),
            before,
            "nothing may run inside the collector"
        );

        run_pending_finalizers();
        assert_eq!(FINALIZED.load(Ordering::SeqCst), before + 1);
        assert_eq!(
            FINALIZED_BLOCK.load(Ordering::SeqCst),
            dead.as_ptr() as usize,
            "the callback receives the block, as upstream's finalizers expect"
        );

        // The block is out of the table, so the cycle that actually reclaims
        // it -- when it is no longer marked -- must not call it again.
        unmark(&gc, dead_at);
        gc.take_dead_finalizers();
        run_pending_finalizers();
        assert_eq!(
            FINALIZED.load(Ordering::SeqCst),
            before + 1,
            "a finalizer ran twice for one block"
        );

        // And the block that survived the first pass is finalized when it
        // does die, with the callback the first pass left alone.
        unmark(&gc, live_at);
        gc.take_dead_finalizers();
        run_pending_finalizers();
        assert_eq!(FINALIZED.load(Ordering::SeqCst), before + 2);
        assert_eq!(
            FINALIZED_BLOCK.load(Ordering::SeqCst),
            live.as_ptr() as usize
        );
    }

    /// The `hl_mutex_free` idiom: do the work only if word zero is still set,
    /// then clear it, so an explicit free and a collection cannot both run it.
    static GUARDED_RAN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    unsafe extern "C" fn guarded_finalize(block: *mut c_void) {
        let slot = block as *mut usize;
        unsafe {
            if *slot == 0 {
                return;
            }
            *slot = 0;
        }
        GUARDED_RAN.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn a_finalizer_that_guards_on_its_own_slot_still_runs() {
        let _turn = finalizer_test_turn();
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let block = gc.allocate(16).unwrap();
        unsafe { (block.as_ptr() as *mut usize).write(guarded_finalize as *const () as usize) };
        gc.register_finalizable(block.as_ptr());
        let before = GUARDED_RAN.load(Ordering::SeqCst);

        gc.take_dead_finalizers();
        run_pending_finalizers();
        assert_eq!(
            GUARDED_RAN.load(Ordering::SeqCst),
            before + 1,
            "the collector cleared word zero before calling, so the body \
             took itself for already freed"
        );
        assert_eq!(word0(block), 0, "the body clears its own slot");

        // Explicitly freeing first is the other half of the same contract:
        // a null slot means the collector has nothing to call.
        let other = gc.allocate(16).unwrap();
        unsafe { (other.as_ptr() as *mut usize).write(guarded_finalize as *const () as usize) };
        gc.register_finalizable(other.as_ptr());
        unsafe { guarded_finalize(other.as_ptr() as *mut c_void) };
        let after_explicit = GUARDED_RAN.load(Ordering::SeqCst);
        gc.take_dead_finalizers();
        run_pending_finalizers();
        assert_eq!(
            GUARDED_RAN.load(Ordering::SeqCst),
            after_explicit,
            "a block freed explicitly must not be finalized again"
        );
    }

    #[test]
    fn a_finalizable_block_reached_from_another_object_survives() {
        let _turn = finalizer_test_turn();
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let holder = gc.allocate(16).unwrap();
        let held = gc.allocate(16).unwrap();
        unsafe { (held.as_ptr() as *mut usize).write(record_finalized as *const () as usize) };
        gc.register_finalizable(held.as_ptr());
        // The only reference to `held` is a field of `holder`.
        unsafe { (holder.as_ptr() as *mut usize).write(held.as_ptr() as usize) };
        let before = FINALIZED.load(Ordering::SeqCst);

        let mut work = Vec::new();
        gc.mark_allocation(offset(&gc, holder), &mut work);
        gc.conservative_trace(work);
        gc.take_dead_finalizers();
        run_pending_finalizers();

        assert_eq!(
            FINALIZED.load(Ordering::SeqCst),
            before,
            "a block the trace reached through a field is not garbage"
        );
        assert!(gc.finalizables.contains(&offset(&gc, held)));
    }

    /// The allocator path ash's `hl_gc_alloc_gen` takes, in the shape its
    /// `hl_compat` gives it: one `allocate`, then the kind bits decide what
    /// happens to word zero -- a type for `KIND_DYNAMIC`, a table entry for
    /// `KIND_FINALIZER`, nothing for the rest.
    unsafe fn alloc_gen(t: *mut hl_type, size: i32, flags: u32) -> *mut c_void {
        unsafe { super::alloc_gen(t, size as usize, flags) }
    }

    #[test]
    fn alloc_gen_records_the_finalizer_kind_and_leaves_word_zero_alone() {
        use caribou_abi::mem::{KIND_FINALIZER, KIND_NOPTR};
        let mut ty: hl_type = unsafe { mem::zeroed() };

        // Held across the allocation and the check: this thread registered no
        // stack top, so a collection on another thread would finalize the block
        // before the assertion could see it.
        let _lock = gc_guard();
        let block = unsafe { alloc_gen(&mut ty, 32, KIND_FINALIZER) } as usize;
        let plain = unsafe { alloc_gen(&mut ty, 32, KIND_NOPTR) } as usize;

        let gc = gc_locked_init();
        let heap_start = gc.heap.memory.as_ptr() as usize;
        assert!(
            gc.finalizables.contains(&(block - heap_start)),
            "a MEM_KIND_FINALIZER allocation must be recorded; nothing else \
             carries the kind past this call"
        );
        assert!(
            !gc.finalizables.contains(&(plain - heap_start)),
            "only the finalizer kind is recorded"
        );
        drop(gc);

        assert_eq!(
            unsafe { *(block as *const usize) },
            0,
            "word zero belongs to the caller's finalizer field"
        );
    }

    fn kind_of(gc: &ImmixAllocator, at: usize) -> u8 {
        gc.heap.objects[at / ALLOC_QUANTUM].load(Ordering::Relaxed) & OBJECT_KIND_MASK
    }

    const fn plain_type() -> hl_type {
        hl_type {
            kind: hl::HOBJ,
            detail: hl::hl_type_detail {
                obj: ptr::null_mut(),
            },
            vobj_proto: ptr::null_mut(),
            mark_bits: ptr::null_mut(),
        }
    }

    #[test]
    fn alloc_gen_records_the_noptr_and_traced_kinds() {
        use caribou_abi::mem::{KIND_DYNAMIC, KIND_NOPTR, KIND_RAW, TRACED};
        static HOOKLESS: TypeDesc = TypeDesc::new(plain_type());
        let mut ty: hl_type = unsafe { mem::zeroed() };
        let _lock = gc_guard();
        let noptr = unsafe { alloc_gen(&mut ty, 32, KIND_NOPTR) } as usize;
        let raw = unsafe { alloc_gen(&mut ty, 32, KIND_RAW) } as usize;
        let typed = unsafe { alloc_gen(&mut ty, 32, KIND_DYNAMIC) } as usize;
        let desc = &HOOKLESS as *const TypeDesc as *mut hl_type;
        let traced = unsafe { alloc_gen(desc, 32, KIND_DYNAMIC | TRACED) } as usize;

        let gc = gc_locked_init();
        let heap_start = gc.heap.memory.as_ptr() as usize;
        assert_eq!(kind_of(&gc, noptr - heap_start), OBJECT_KIND_NOPTR);
        assert_eq!(kind_of(&gc, raw - heap_start), OBJECT_KIND_RAW);
        assert_eq!(
            kind_of(&gc, typed - heap_start),
            OBJECT_KIND_RAW,
            "a bare hl_type has no hooks to read: Typed alone stays conservative"
        );
        assert_eq!(kind_of(&gc, traced - heap_start), OBJECT_KIND_TRACED);
        assert!(
            !gc.blocks[(traced - heap_start) / BLOCK_SIZE].has_drop,
            "a hookless descriptor gives the sweep nothing to drop"
        );
        assert_eq!(unsafe { *(traced as *const usize) }, desc as usize);
        assert_eq!(unsafe { *(typed as *const usize) }, &raw mut ty as usize);
        assert_eq!(unsafe { *(noptr as *const usize) }, 0);
    }

    #[test]
    fn kind_bits_leave_size_decoding_intact() {
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 16);
        for kind in [OBJECT_KIND_RAW, OBJECT_KIND_NOPTR, OBJECT_KIND_TRACED] {
            for (size, reserved) in [
                (16, 16),
                (80, 80),
                (256, 256),
                (BLOCK_SIZE + 144, BLOCK_SIZE + 256),
            ] {
                let p = gc.allocate(size).unwrap();
                let start = offset(&gc, p);
                gc.set_allocation_kind(start, kind);
                assert_eq!(kind_of(&gc, start), kind);
                let find = |gc: &ImmixAllocator, at| {
                    allocation_at(&gc.blocks, &gc.heap.alloc_sizes, &gc.heap.objects, at)
                };
                assert_eq!(find(&gc, start), Some((start, reserved)));
                assert_eq!(find(&gc, start + size - 1), Some((start, reserved)));
                assert_eq!(find(&gc, start + reserved), None, "{kind:#x} {size}");
                // The claim bit beside the kind bits changes nothing either.
                gc.heap.objects[start / ALLOC_QUANTUM].fetch_or(OBJECT_MARK, Ordering::Relaxed);
                assert_eq!(find(&gc, start + size - 1), Some((start, reserved)));
                assert_eq!(kind_of(&gc, start), kind);
                unmark(&gc, start);
                // A pad allocation, so the next span does not start where
                // this one's reserved tail ends.
                gc.allocate(16).unwrap();
            }
        }
    }

    /// `holder` points at `target` in word zero and nothing else does.
    fn holder_and_target(gc: &mut ImmixAllocator, kind: u8) -> (usize, usize) {
        let holder = gc.allocate(16).unwrap();
        let target = gc.allocate(16).unwrap();
        unsafe { (holder.as_ptr() as *mut usize).write(target.as_ptr() as usize) };
        let (holder, target) = (offset(gc, holder), offset(gc, target));
        gc.set_allocation_kind(holder, kind);
        let mut work = Vec::new();
        gc.mark_allocation(holder, &mut work);
        gc.conservative_trace(work);
        (holder, target)
    }

    #[test]
    fn a_noptr_block_retains_nothing_it_points_at() {
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let (holder, target) = holder_and_target(&mut gc, OBJECT_KIND_NOPTR);
        assert!(object_marked(&gc, holder));
        assert!(
            !object_marked(&gc, target),
            "a NoPtr block is never scanned"
        );
    }

    #[test]
    fn a_raw_block_still_retains_conservatively() {
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let (holder, target) = holder_and_target(&mut gc, OBJECT_KIND_RAW);
        assert!(object_marked(&gc, holder));
        assert!(object_marked(&gc, target));
    }

    /// A traced object shaped like WrenLift's: a descriptor word, then
    /// pointers the conservative scan cannot see, in containers outside the
    /// heap. `items` are raw addresses, `boxed` are NaN-boxed values.
    #[repr(C)]
    struct Holder {
        desc: *const TypeDesc,
        items: Vec<usize>,
        boxed: Vec<u64>,
    }

    /// Shared by every test that drops a `Holder`; they take turns, as the
    /// finalizer tests do.
    static HOLDER_DROPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static HOLDER_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn holder_test_turn() -> std::sync::MutexGuard<'static, ()> {
        HOLDER_TEST.lock().unwrap_or_else(|e| e.into_inner())
    }

    unsafe extern "C" fn trace_holder(obj: *mut u8, tracer: *mut Tracer) {
        let holder = unsafe { &*(obj as *const Holder) };
        let tracer = unsafe { &mut *tracer };
        for &p in &holder.items {
            tracer.mark(p as *const u8);
        }
        for &bits in &holder.boxed {
            tracer.mark_value(bits);
        }
    }

    unsafe extern "C" fn drop_holder(obj: *mut u8) {
        unsafe { ptr::drop_in_place(obj as *mut Holder) };
        HOLDER_DROPS.fetch_add(1, Ordering::SeqCst);
    }

    static HOLDER_DESC: TypeDesc = TypeDesc {
        trace: Some(trace_holder),
        drop: Some(drop_holder),
        ..TypeDesc::new(plain_type())
    };

    /// A `Holder` referencing `child` by address and `boxed` by value.
    /// Returns the three heap offsets: holder, child, boxed.
    fn traced_holder(gc: &mut ImmixAllocator) -> (usize, usize, usize) {
        let holder = gc.allocate(mem::size_of::<Holder>()).unwrap();
        let child = gc.allocate(16).unwrap();
        let boxed = gc.allocate(16).unwrap();
        unsafe {
            ptr::write(
                holder.as_ptr() as *mut Holder,
                Holder {
                    desc: &HOLDER_DESC,
                    items: vec![child.as_ptr() as usize + 4],
                    boxed: vec![
                        caribou_abi::Value::object(boxed.as_ptr() as *const c_void).to_bits(),
                        caribou_abi::Value::number(1.5).to_bits(),
                        caribou_abi::Value::null().to_bits(),
                    ],
                },
            )
        };
        let at = offset(gc, holder);
        gc.set_allocation_kind(at, OBJECT_KIND_TRACED);
        (at, offset(gc, child), offset(gc, boxed))
    }

    #[test]
    fn a_traced_object_marks_through_its_hook_and_is_dropped_once_when_dead() {
        let _turn = holder_test_turn();
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let (holder, child, boxed) = traced_holder(&mut gc);
        assert!(gc.blocks[holder / BLOCK_SIZE].has_drop);
        let sibling = gc.allocate(16).unwrap();
        let sibling = offset(&gc, sibling);
        let before = HOLDER_DROPS.load(Ordering::SeqCst);

        let mut work = Vec::new();
        gc.mark_allocation(holder, &mut work);
        gc.conservative_trace(work);
        assert!(object_marked(&gc, holder));
        assert!(object_marked(&gc, child), "marked through the hook");
        assert!(object_marked(&gc, boxed), "marked through mark_value");
        assert!(!object_marked(&gc, sibling), "nothing referenced it");
        gc.sweep(&[]);
        assert_eq!(
            HOLDER_DROPS.load(Ordering::SeqCst),
            before,
            "a live object is never dropped"
        );

        // Now only the sibling is reachable, so its block is kept and the
        // holder dies on it.
        let mut work = Vec::new();
        gc.mark_allocation(sibling, &mut work);
        gc.conservative_trace(work);
        gc.sweep(&[]);
        assert_eq!(HOLDER_DROPS.load(Ordering::SeqCst), before + 1);
        let find = |at| allocation_at(&gc.blocks, &gc.heap.alloc_sizes, &gc.heap.objects, at);
        assert_eq!(find(holder + 8), None, "a dropped object is forgotten");
        assert_eq!(find(sibling), Some((sibling, 16)));
        assert!(!gc.blocks[holder / BLOCK_SIZE].has_drop);

        // A stale pointer into it resolves to nothing, so no later cycle can
        // trace or drop it again.
        let mut work = Vec::new();
        gc.mark_allocation(holder, &mut work);
        assert!(work.is_empty());
        gc.sweep(&[]);
        assert_eq!(HOLDER_DROPS.load(Ordering::SeqCst), before + 1);
    }

    #[test]
    fn a_dead_traced_span_forgets_its_span_entry() {
        let _turn = holder_test_turn();
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let span = gc.allocate(LINE_SIZE * 3).unwrap();
        let at = offset(&gc, span);
        unsafe {
            ptr::write(
                span.as_ptr() as *mut Holder,
                Holder {
                    desc: &HOLDER_DESC,
                    items: Vec::new(),
                    boxed: Vec::new(),
                },
            )
        };
        gc.set_allocation_kind(at, OBJECT_KIND_TRACED);
        let keeper = gc.allocate(16).unwrap();
        let keeper = offset(&gc, keeper);
        let before = HOLDER_DROPS.load(Ordering::SeqCst);
        let mut work = Vec::new();
        gc.mark_allocation(keeper, &mut work);
        gc.sweep(&[]);
        assert_eq!(HOLDER_DROPS.load(Ordering::SeqCst), before + 1);
        assert_eq!(gc.heap.alloc_sizes[at / LINE_SIZE], 0);
        let mut work = Vec::new();
        gc.mark_allocation(at + LINE_SIZE + 8, &mut work);
        assert!(work.is_empty());
    }

    /// With no drop hook the sweep has nothing to run, so the block skips
    /// the drop pass: the dead object keeps its start, and a stale pointer
    /// retains it as it would a raw object, until its lines are reused.
    #[test]
    fn a_dead_traced_object_without_a_drop_hook_waits_for_line_reuse() {
        static TRACE_ONLY: TypeDesc = TypeDesc {
            trace: Some(trace_holder),
            ..TypeDesc::new(plain_type())
        };
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let obj = gc.allocate(mem::size_of::<Holder>()).unwrap();
        unsafe {
            ptr::write(
                obj.as_ptr() as *mut Holder,
                Holder {
                    desc: &TRACE_ONLY,
                    items: Vec::new(),
                    boxed: Vec::new(),
                },
            )
        };
        let at = offset(&gc, obj);
        gc.set_allocation_kind(at, OBJECT_KIND_TRACED);
        assert!(!gc.blocks[at / BLOCK_SIZE].has_drop);
        let keeper = gc.allocate(16).unwrap();
        let keeper = offset(&gc, keeper);
        let mut work = Vec::new();
        gc.mark_allocation(keeper, &mut work);
        gc.sweep(&[]);
        let find = |gc: &ImmixAllocator, at| {
            allocation_at(&gc.blocks, &gc.heap.alloc_sizes, &gc.heap.objects, at)
        };
        let size = find(&gc, at).expect("dead but still an allocation").1;
        let mut work = Vec::new();
        gc.mark_allocation(at + 8, &mut work);
        assert_eq!(work, vec![(at, size)], "a stale pointer retains it");
        gc.sweep(&[]);
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn a_trace_hook_runs_on_the_marking_pool_too() {
        let _turn = holder_test_turn();
        // The pool is process-wide; the GC lock keeps another test's
        // collection from sharing it.
        let _lock = gc_guard();
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let (holder, child, boxed) = traced_holder(&mut gc);
        let sibling = gc.allocate(16).unwrap();
        let sibling = offset(&gc, sibling);
        let mut initial = Vec::new();
        gc.mark_allocation(holder, &mut initial);
        let heap_start = gc.heap.memory.as_ptr() as usize;
        let queue = MarkQueue {
            work: std::sync::Mutex::new(initial),
            ready: std::sync::Condvar::new(),
            idle: std::sync::atomic::AtomicUsize::new(0),
            done: AtomicBool::new(false),
        };
        let threads = mark_threads();
        MarkPool::get().run(MarkJob {
            blocks: (gc.blocks.as_ptr(), gc.blocks.len()),
            alloc_sizes: (gc.heap.alloc_sizes.as_ptr(), gc.heap.alloc_sizes.len()),
            objects: (gc.heap.objects.as_ptr(), gc.heap.objects.len()),
            heap_start,
            heap_end: heap_start + gc.heap.memory.len,
            queue: &queue as *const MarkQueue,
            threads,
        });
        assert!(object_marked(&gc, child));
        assert!(object_marked(&gc, boxed));
        assert!(!object_marked(&gc, sibling));
        // Leave nothing for the drop pass to read after the test's frame.
        gc.sweep(&[]);
        gc.sweep(&[]);
    }

    #[test]
    fn a_handle_roots_its_object_until_released() {
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let p = gc.allocate(16).unwrap();
        let at = offset(&gc, p);
        let h = gc.handle_new(p.as_ptr());
        assert!(!h.is_null());
        assert_eq!(gc.handle_get(h), p.as_ptr());
        gc.mark_roots(&[]);
        assert!(object_marked(&gc, at));
        gc.sweep(&[]);

        // Two references: the first release keeps it.
        gc.handle_retain(h);
        gc.handle_release(h);
        gc.mark_roots(&[]);
        assert!(object_marked(&gc, at));
        gc.sweep(&[]);

        gc.handle_release(h);
        assert!(gc.handle_get(h).is_null());
        gc.mark_roots(&[]);
        assert!(!object_marked(&gc, at));
        // The slot is reused, the stale handle stays dead.
        let again = gc.handle_new(p.as_ptr());
        assert_eq!(again, h, "the freed slot is handed out again");
        assert_eq!(gc.handle_get(Handle::NULL), ptr::null_mut());
        assert_eq!(gc.handle_new(ptr::null_mut()), Handle::NULL);
        assert_eq!(Handle::from_raw(h.as_raw()), h);
    }

    #[test]
    fn handle_entry_points_reach_the_process_heap() {
        let _lock = gc_guard();
        let p = gc_locked_init().allocate(16).unwrap().as_ptr();
        let h = handle_new(p);
        assert_eq!(handle_get(h), p);
        handle_release(h);
        assert!(handle_get(h).is_null());
    }

    /// A deferred release waits for the outermost lock release, as a drop
    /// hook's would; a nested release does not run it.
    #[test]
    fn a_deferred_release_runs_when_the_lock_is_next_free() {
        let p;
        let h;
        {
            let _lock = gc_guard();
            p = gc_locked_init().allocate(16).unwrap().as_ptr();
            h = handle_new(p);
            handle_release_deferred(h);
            assert_eq!(handle_get(h), p, "still held under the lock");
        }
        assert!(handle_get(h).is_null(), "released with the lock");
        handle_release_deferred(Handle::NULL);
        assert!(!deferred_releases_pending());
    }

    #[test]
    fn a_root_range_is_scanned_until_unregistered() {
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let p = gc.allocate(16).unwrap();
        let at = offset(&gc, p);
        // The pointer sits past a byte-misaligned start, as in a data section.
        let mut section = [0u8; 4 * WORD];
        let slot = word_align_up(section.as_ptr() as usize + 1);
        unsafe { (slot as *mut usize).write(p.as_ptr() as usize) };
        let start = section.as_ptr().wrapping_add(1);
        gc.register_root_range(start, section.len() - 1);
        gc.mark_roots(&[]);
        assert!(object_marked(&gc, at));
        gc.sweep(&[]);

        gc.unregister_root_range(start, section.len() - 1);
        gc.mark_roots(&[]);
        assert!(!object_marked(&gc, at));
        std::hint::black_box(&mut section);
    }

    #[test]
    fn a_stop_request_reaches_the_installed_poll_hook() {
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn count() {
            CALLS.fetch_add(1, Ordering::SeqCst);
        }
        // Nothing installed is a no-op, not a call through zero. The hook is
        // process-global and this is the only test that installs one; another
        // test's collection may call it too, so the count can only grow.
        request_fiber_poll();
        set_poll_request_hook(count);
        let before = CALLS.load(Ordering::SeqCst);
        request_fiber_poll();
        assert!(CALLS.load(Ordering::SeqCst) > before);
    }

    /// `gc_locked_init` initialises once and every later hold, initialising or
    /// not, is the same allocator: the contract `get_mut_or_init` gave.
    #[test]
    fn locked_init_installs_one_allocator_and_hands_it_back_thereafter() {
        let first = &*gc_locked_init() as *const ImmixAllocator;
        let again = &*gc_locked_init() as *const ImmixAllocator;
        let plain = &*gc_locked() as *const ImmixAllocator;
        assert_eq!(first, again);
        assert_eq!(first, plain);
    }

    /// A hosted collector's claim is the marker's: object bit and lines, once
    /// per cycle, and the sweep clears it. A forgotten allocation resolves to
    /// nothing and its lines come back.
    #[test]
    fn a_hosted_collector_claims_and_forgets_through_the_side_table() {
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let small = gc.allocate(48).unwrap();
        let span = gc.allocate(LINE_SIZE * 3).unwrap();
        let base = gc.heap.memory.as_ptr() as usize;

        let small_addr = small.as_ptr() as usize;
        assert_eq!(gc.allocation_containing(small_addr), Some((small_addr, 48)));
        assert_eq!(
            gc.allocation_containing(small_addr + 47),
            Some((small_addr, 48))
        );
        let span_addr = span.as_ptr() as usize;
        assert_eq!(
            gc.allocation_containing(span_addr + 2 * LINE_SIZE + 5),
            Some((span_addr, 3 * LINE_SIZE))
        );
        assert_eq!(gc.allocation_containing(base + gc.heap.memory.len), None);

        assert!(!gc.is_claimed(small.as_ptr()));
        assert!(gc.claim_for_cycle(small.as_ptr()));
        assert!(!gc.claim_for_cycle(small.as_ptr().wrapping_add(8)));
        assert!(gc.is_claimed(small.as_ptr()));
        assert!(object_marked(&gc, offset(&gc, small)));
        let line = offset(&gc, small) / LINE_SIZE;
        assert!(gc.blocks[line / LINES_PER_BLOCK].is_marked(line % LINES_PER_BLOCK));
        gc.unclaim(small.as_ptr());
        assert!(!gc.is_claimed(small.as_ptr()));
        assert!(gc.claim_for_cycle(small.as_ptr()));

        // By start: the size, whether or not the claim stood already; a
        // span's lines are all claimed; an interior address is no start.
        assert_eq!(gc.claim_start(small.as_ptr()), Some(48));
        assert_eq!(gc.claim_start(span.as_ptr()), Some(3 * LINE_SIZE));
        assert!(gc.is_claimed(span.as_ptr()));
        let first = offset(&gc, span) / LINE_SIZE;
        for line in first..first + 3 {
            assert!(gc.blocks[line / LINES_PER_BLOCK].is_marked(line % LINES_PER_BLOCK));
        }
        assert_eq!(gc.claim_start(span.as_ptr().wrapping_add(LINE_SIZE)), None);
        assert_eq!(gc.claim_start(ptr::null()), None);
        gc.unclaim(span.as_ptr());

        // The unclaimed span dies with the sweep; the claimed object survives
        // it with its claim cleared for the next cycle.
        gc.forget_allocation(span.as_ptr());
        assert_eq!(gc.allocation_containing(span_addr), None);
        assert_eq!(gc.allocation_containing(span_addr + LINE_SIZE), None);
        let freed_before = gc.heap.free_blocks.len();
        gc.sweep(&[]);
        assert!(!gc.is_claimed(small.as_ptr()));
        assert_eq!(gc.allocation_containing(small_addr), Some((small_addr, 48)));
        assert!(
            gc.heap.free_blocks.len() >= freed_before,
            "sweep lost track of the free list"
        );
        assert!(!gc.claim_for_cycle(ptr::null()));
        assert!(!gc.is_claimed(ptr::null()));
    }

    /// The locked path bumps through a kept block's free lines before it
    /// takes a fresh block, as the TLAB refill does.
    #[test]
    fn the_locked_path_allocates_into_recycled_lines() {
        if !recycle_lines() {
            return;
        }
        let mut gc = ImmixAllocator::with_heap_size(BLOCK_SIZE * 4);
        let keep = gc.allocate(LINE_SIZE).unwrap();
        for _ in 0..8 {
            gc.allocate(LINE_SIZE).unwrap();
        }
        let block = offset(&gc, keep) / BLOCK_SIZE * BLOCK_SIZE;
        assert!(gc.claim_for_cycle(keep.as_ptr()));
        gc.sweep(&[]);
        assert!(
            !gc.heap.recycle_spans.is_empty(),
            "the kept block has no free run"
        );
        // What a collection does with the cursor before the world restarts.
        gc.heap.allocation_point = 0;
        gc.heap.current_block_end = 0;
        let next = gc.allocate(LINE_SIZE * 2).unwrap();
        assert_eq!(
            offset(&gc, next) / BLOCK_SIZE * BLOCK_SIZE,
            block,
            "a fresh block was taken while the kept block had free lines"
        );
        assert_ne!(offset(&gc, next), offset(&gc, keep));
        assert_eq!(
            gc.allocation_containing(keep.as_ptr() as usize),
            Some((keep.as_ptr() as usize, LINE_SIZE))
        );
    }

    /// The lock-free trigger mirrors the singleton's: due once the threshold
    /// of allocation and external pressure is reached, reset by a collection.
    #[test]
    fn the_lockless_trigger_follows_the_singletons_collections() {
        if gc_stress_every() > 0 {
            return; // Stress mode answers from the allocation count alone.
        }
        // Held throughout, so no other hold of the singleton collects.
        let mut gc = gc_locked_init();
        let before = collections();
        gc.collect_garbage();
        assert_eq!(collections(), before + 1);
        assert!(!should_collect(), "a fresh collection leaves nothing due");
        let threshold = gc.heap.trigger_threshold;
        gc.track_external(threshold);
        assert!(should_collect(), "pressure at the threshold is due");
        gc.collect_garbage();
        assert!(!should_collect());
    }
}
