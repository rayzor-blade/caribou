//! The scheduler slots: ash's C-shaped entry points over `caribou::sched`.
//!
//! A Haxe thread is a stackful task on the calling world, or on a pool
//! world when its body is compiled. What stays ash's -- the interpreter's
//! switch hook and the per-thread exception cells -- is reached through the
//! two exports the seam keeps for a replacement scheduler, taken from the
//! hosted ash_std image at install time.

use std::any::Any;
use std::cell::Cell;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use ash_std::error::TrapContext;
use ash_std::fiber::FiberSwitchHook;
use ash_std::hl::vdynamic;
use ash_std::rt::{FiberBody, RT_NO_TIMEOUT, RT_THREAD_COMPILED, Waiter};
use caribou::sched::{self, DEFAULT_STACK_SIZE, HostState, TaskId};

pub(crate) type SwitchHookGetter = unsafe extern "C" fn() -> Option<FiberSwitchHook>;
pub(crate) type ExcSwap = unsafe extern "C" fn(*mut *mut TrapContext, *mut *mut vdynamic);

/// The hosted image's back-references. Set once by `install_into`; the
/// linked ash_std's until then.
pub(crate) struct AshHooks {
    pub(crate) switch_hook: SwitchHookGetter,
    pub(crate) exc_swap: ExcSwap,
}

static HOOKS: OnceLock<AshHooks> = OnceLock::new();

pub(crate) fn set_hooks(hooks: AshHooks) {
    let _ = HOOKS.set(hooks);
}

fn hooks() -> &'static AshHooks {
    HOOKS.get_or_init(|| AshHooks {
        switch_hook: ash_std::rt::hlp_rt_switch_hook,
        exc_swap: ash_std::rt::hlp_rt_exc_swap,
    })
}

// ── Identity ────────────────────────────────────────────────────────────

/// Ash keys fibers by a u32; the core's task ids are sequential u64s, so
/// the low half is the whole id until four billion tasks have been spawned
/// (the same truncation the core makes for fiber-stack ids).
fn ash_id(task: TaskId) -> u32 {
    task.0 as u32
}

fn core_id(id: u32) -> TaskId {
    TaskId(u64::from(id))
}

/// Ash's handle encoding: never null, never a valid pointer.
fn handle(id: u32) -> *mut c_void {
    ((id as usize) << 4 | 1) as *mut c_void
}

fn to_ash(waiter: sched::Waiter) -> Waiter {
    Waiter {
        scheduler_id: waiter.world(),
        fiber_id: ash_id(waiter.task()),
        token: waiter.token(),
    }
}

fn to_core(waiter: Waiter) -> sched::Waiter {
    sched::Waiter::from_parts(waiter.scheduler_id, core_id(waiter.fiber_id), waiter.token)
}

fn deadline_from(timeout_ns: u64) -> Option<Instant> {
    (timeout_ns != RT_NO_TIMEOUT).then(|| Instant::now() + Duration::from_nanos(timeout_ns))
}

// ── Per-task state ──────────────────────────────────────────────────────

/// What the scheduler swaps around a task's turns: the exception state ash
/// keeps in thread-local cells, and the context `thread_create` was handed.
/// The swap is symmetric, so entering and leaving are the same exchange.
struct ExcHost {
    trap: *mut TrapContext,
    exc: *mut vdynamic,
    ctx: *mut c_void,
}

impl ExcHost {
    fn new(ctx: *mut c_void) -> Box<Self> {
        Box::new(Self {
            trap: std::ptr::null_mut(),
            exc: std::ptr::null_mut(),
            ctx,
        })
    }

    fn swap(&mut self) {
        // SAFETY: both cells are this thread's, and the scheduler calls this
        // only on the thread that runs the task.
        unsafe { (hooks().exc_swap)(&mut self.trap, &mut self.exc) };
    }
}

impl HostState for ExcHost {
    fn swap_in(&mut self) {
        self.swap();
    }

    fn swap_out(&mut self) {
        self.swap();
    }
}

/// The core's hook, per world: hands ash's registered hook the switch as
/// fiber ids. Not on a pool world, where ash's own scheduler kept the
/// interpreter's hook away from compiled bodies too.
fn switch_bridge(from: TaskId, to: TaskId) {
    if sched::is_pool_worker() {
        return;
    }
    // SAFETY: the getter and the hook are ash's exports for this purpose.
    if let Some(hook) = unsafe { (hooks().switch_hook)() } {
        unsafe { hook(ash_id(from), ash_id(to)) };
    }
}

thread_local! {
    /// Whether this world has its main-context host and switch hook.
    static WORLD_READY: Cell<bool> = const { Cell::new(false) };
}

/// The main context's exception state must be swapped out before the first
/// task on a world steps, so this runs where a task is created, before it
/// can run. A pool world's main context never holds Haxe state, so there
/// the task's own first run is early enough.
fn ensure_world_ready() {
    if WORLD_READY.with(Cell::get) {
        return;
    }
    WORLD_READY.with(|ready| ready.set(true));
    sched::attach_host_state(TaskId::NONE, ExcHost::new(std::ptr::null_mut()));
    sched::set_switch_hook(switch_bridge);
}

// ── Threads ─────────────────────────────────────────────────────────────

/// The OS thread running the program; claimed by the first asker if
/// `mark_main_thread` never ran.
static MAIN_THREAD: OnceLock<std::thread::ThreadId> = OnceLock::new();
static NEXT_FOREIGN_OWNER: AtomicU32 = AtomicU32::new(1);
static FOREIGN_THREAD_SEEN: AtomicBool = AtomicBool::new(false);

thread_local! {
    static FOREIGN_OWNER: Cell<u32> = const { Cell::new(0) };
}

pub unsafe extern "C" fn mark_main_thread() {
    let _ = MAIN_THREAD.set(std::thread::current().id());
    // The main thread drives its world from the first park on, as in ash.
    let _ = sched::world_id();
}

pub unsafe extern "C" fn is_main_thread() -> bool {
    *MAIN_THREAD.get_or_init(|| std::thread::current().id()) == std::thread::current().id()
}

pub unsafe extern "C" fn foreign_threads_seen() -> bool {
    FOREIGN_THREAD_SEEN.load(Ordering::Acquire)
}

pub unsafe extern "C" fn new_waiter() -> Waiter {
    to_ash(sched::new_waiter())
}

pub unsafe extern "C" fn wake(waiter: Waiter) -> bool {
    sched::wake(to_core(waiter))
}

pub unsafe extern "C" fn park(waiter: Waiter, timeout_ns: u64) -> bool {
    sched::park(to_core(waiter), deadline_from(timeout_ns))
}

pub unsafe extern "C" fn sleep_ns(ns: u64) {
    sched::sleep_until(Instant::now() + Duration::from_nanos(ns));
}

pub unsafe extern "C" fn block_yield() {
    sched::block_yield();
}

pub unsafe extern "C" fn schedule_step() -> bool {
    sched::schedule_step()
}

/// A compiled body goes to the pool; anything else, or a pool with no
/// worker to take it, runs on the calling world and gets one turn at once,
/// ash's rule that a thread reaches its first blocking point before
/// `thread_create` returns.
pub unsafe extern "C" fn thread_create(
    body: FiberBody,
    ctx: *mut c_void,
    flags: u32,
) -> *mut c_void {
    ensure_world_ready();
    let ctx = ctx as usize;
    let run = move || {
        ensure_world_ready();
        sched::attach_host_state(sched::current_task(), ExcHost::new(ctx as *mut c_void));
        // SAFETY: `body` and `ctx` are what ash handed `thread_create`.
        unsafe { body(ctx as *mut c_void) };
    };
    let (id, local) = if flags & RT_THREAD_COMPILED != 0 {
        // A task the pool took is installed on its world at that world's
        // next turn; one placed here is in this world's table already.
        let before = sched::live_tasks();
        let id = sched::spawn_fiber_on_pool(DEFAULT_STACK_SIZE, run);
        (id, sched::live_tasks() > before)
    } else {
        (sched::spawn_fiber(DEFAULT_STACK_SIZE, run), true)
    };
    if local {
        sched::schedule_step();
    }
    handle(ash_id(id))
}

pub unsafe extern "C" fn fiber_poll() {
    sched::poll();
}

pub unsafe extern "C" fn fibers_active() -> bool {
    sched::any_live_tasks()
}

pub unsafe extern "C" fn current_id() -> u32 {
    ash_id(sched::current_task())
}

pub unsafe extern "C" fn current_handle() -> *mut c_void {
    match ash_id(sched::current_task()) {
        0 => std::ptr::null_mut(),
        id => handle(id),
    }
}

/// The running task's id, or a per-thread id tagged above the task id
/// space for a thread with no task, so two such threads never compare
/// equal as lock owners.
pub unsafe extern "C" fn current_owner() -> u64 {
    let task = sched::current_task();
    if task.is_task() {
        return u64::from(ash_id(task));
    }
    FOREIGN_OWNER.with(|slot| {
        let mut id = slot.get();
        if id == 0 {
            id = NEXT_FOREIGN_OWNER.fetch_add(1, Ordering::Relaxed).max(1);
            slot.set(id);
            if !unsafe { is_main_thread() } {
                FOREIGN_THREAD_SEEN.store(true, Ordering::Release);
            }
        }
        (1u64 << 32) | u64::from(id)
    })
}

pub unsafe extern "C" fn current_ctx() -> *mut c_void {
    let task = sched::current_task();
    if !task.is_task() {
        return std::ptr::null_mut();
    }
    sched::with_host_state(task, |host| {
        let host: &mut dyn Any = host;
        host.downcast_ref::<ExcHost>()
            .map_or(std::ptr::null_mut(), |host| host.ctx)
    })
    .unwrap_or(std::ptr::null_mut())
}

/// The core tells the heap itself when the depth crosses zero, so this
/// reports no change and ash's caller does not tell it again.
pub unsafe extern "C" fn update_gc_blocking_depth(blocking: bool) -> bool {
    if blocking {
        sched::enter_blocking();
    } else {
        sched::leave_blocking();
    }
    false
}

pub unsafe extern "C" fn is_gc_blocking() -> bool {
    sched::is_blocking()
}

pub unsafe extern "C" fn request_fiber_poll() {
    sched::request_poll();
}

pub unsafe extern "C" fn fiber_poll_epoch_address() -> *const u64 {
    sched::poll_epoch_address()
}

pub unsafe extern "C" fn is_worker_lane() -> bool {
    sched::is_pool_worker()
}
