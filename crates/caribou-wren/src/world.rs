//! The world slots: wren_lift's tasks and waits on the core's scheduler,
//! so a Wren fiber or thread is a task of the world Haxe's are, parked
//! and woken by the same tokens, placed on the same pool.
//!
//! A Wren task is a task the core steps on its own stack: its step is
//! wren_lift's `task_step`, one run of the fiber to its next park or
//! yield, and the park it asked for is recorded on the core's running
//! task through `world_park_request` before the fiber yields through
//! wren_lift's own switch. That switch is the task's `Suspend` too, so a
//! Haxe call from a Wren task parks and yields through it. A token
//! crosses as the number alone, so each is bound to the calling context
//! when it is used (`sched::adopt`).
//!
//! wren_lift's collector needs every thread with a view safe or polling,
//! and the seam's thread slots say so of a thread in a wait or a native
//! call. A thread in the core's world is neither: it runs any language's
//! tasks and idles in the core. So the view follows the core's own
//! transitions, and the seam hears nothing of them: the view is running
//! while the thread runs, safe in the core's blocking regions (the
//! blocking hook), safe at a core safepoint while wren_lift's world asks
//! for a stop (the safepoint hook, reached because `host_poll` bumps the
//! core's poll epoch), and each context carries the view's state across
//! the core's switches (`ViewState`, a host state on every task and the
//! main context), so a context parked inside Wren leaves the view safe
//! and one resuming into Wren finds it running. `task_step` makes a
//! worker's view running for the step. Inside a fiber of the core's,
//! wren_lift's collector scans nothing of the thread; what only that
//! stack holds is the core's collection's to keep, as everything
//! wren_lift's marking did not reach is.

use std::cell::{Cell, UnsafeCell};
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use caribou::heap;
use caribou::sched::{self, HostState, ResumeCause, Suspension, Task, TaskId};
use wren_lift::runtime::rt::{NO_DEADLINE, wlift_rt_task_step, wlift_rt_task_suspend};
use wren_lift::runtime::stack_scan::{SPILL_WORDS, spill_callee_saved};
use wren_lift::runtime::vm::{Spill, VM};

// ── The view's safe state ────────────────────────────────────────────────

thread_local! {
    /// The callee-saved registers at the last `view_safe`, kept where the
    /// frame that spilled them is not: the view stays safe after it goes.
    static HELD_REGS: UnsafeCell<[usize; SPILL_WORDS]> = const { UnsafeCell::new([0; SPILL_WORDS]) };
    /// Whether the blocking hook made the view safe, to make it running
    /// again on leaving.
    static BLOCKED_SAFE: Cell<bool> = const { Cell::new(false) };
    /// Whether the main context of this thread's world has its state.
    static MAIN_READY: Cell<bool> = const { Cell::new(false) };
    /// Whether this thread asked wren_lift's world to stop: the safepoint
    /// hook answers no stop of the thread's own.
    static COLLECTING: Cell<bool> = const { Cell::new(false) };
}

/// The view this thread runs, if any: the entered one, else the one
/// wren_lift is dispatching on.
fn view() -> *mut VM {
    crate::proto::current_vm()
}

/// Make the view safe for wren_lift's collector from here, with the
/// registers of this moment published as a second range, since this
/// frame is gone while the view stays safe.
///
/// # Safety
/// `vm` is this thread's view, running.
pub(crate) unsafe fn view_safe(vm: *mut VM) {
    let regs = HELD_REGS.with(|c| c.get());
    unsafe { spill_callee_saved(&mut *regs) };
    let lo = regs as usize;
    let thread = unsafe { &(*vm).thread };
    thread.extra_lo.store(lo, Ordering::Relaxed);
    thread
        .extra_hi
        .store(lo + size_of::<[usize; SPILL_WORDS]>(), Ordering::Relaxed);
    let mut spill = Spill::new();
    unsafe { (*vm).enter_safe_here(&mut spill) };
    std::hint::black_box(&spill);
}

/// Back to running, once no collection of wren_lift's is under way.
///
/// # Safety
/// `vm` is this thread's view, safe.
pub(crate) unsafe fn view_running(vm: *mut VM) {
    unsafe { (*vm).leave_safe_here() };
}

/// The core's safepoint while wren_lift's world asks for a stop: the
/// thread's view goes safe and comes back running once the stop is over,
/// as at wren_lift's own safepoints.
pub(crate) fn safepoint_hook() {
    let vm = view();
    if vm.is_null() || COLLECTING.with(Cell::get) {
        return;
    }
    let vm_ref = unsafe { &*vm };
    if vm_ref.world.requested() && !vm_ref.thread.is_safe() {
        unsafe {
            view_safe(vm);
            view_running(vm);
        }
    }
}

/// The core's outermost blocking region on this thread: a thread in one
/// runs nothing, so its view is safe meanwhile.
pub(crate) fn blocking_hook(on: bool) {
    let vm = view();
    if vm.is_null() {
        return;
    }
    if on {
        if !unsafe { (*vm).thread.is_safe() } {
            unsafe { view_safe(vm) };
            BLOCKED_SAFE.with(|c| c.set(true));
        }
    } else if BLOCKED_SAFE.with(|c| c.replace(false)) {
        unsafe { view_running(vm) };
    }
}

/// The view's state as one context had it: running or safe. Swapped
/// around the core's switches, so a context parked inside Wren leaves
/// the view safe for the others, and finds it running again.
struct ViewState {
    running: bool,
}

impl HostState for ViewState {
    fn swap_out(&mut self) {
        let vm = view();
        self.running = !vm.is_null() && !unsafe { (*vm).thread.is_safe() };
        if self.running {
            unsafe { view_safe(vm) };
        }
    }

    fn swap_in(&mut self) {
        if !self.running {
            return;
        }
        let vm = view();
        if !vm.is_null() && unsafe { (*vm).thread.is_safe() } {
            unsafe { view_running(vm) };
        }
    }
}

/// The run of the view a context is in, set aside while another context
/// runs on the thread: the fiber, its error state and the JIT's
/// per-thread state are the view's one set, and each context has its
/// own. A context that never ran Wren sets aside a run in nothing, and
/// takes that up again.
struct WrenActivation {
    aside: Option<u64>,
}

impl HostState for WrenActivation {
    fn swap_out(&mut self) {
        let vm = view();
        if !vm.is_null() {
            self.aside = Some(unsafe { (*vm).set_aside() });
        }
    }

    fn swap_in(&mut self) {
        if let Some(id) = self.aside.take() {
            let vm = view();
            if !vm.is_null() {
                unsafe { (*vm).take_up(id) };
            }
        }
    }
}

/// Before a task's first turn, whichever language spawned it: the task
/// and, once per world, the main context carry the view's state and the
/// run they are in.
pub(crate) fn task_born(id: TaskId) {
    if !MAIN_READY.with(|c| c.replace(true)) {
        sched::attach_host_state(TaskId::NONE, Box::new(ViewState { running: false }));
        sched::attach_host_state(TaskId::NONE, Box::new(WrenActivation { aside: None }));
    }
    if sched::with_host_state::<ViewState, _>(id, |_| ()).is_none() {
        sched::attach_host_state(id, Box::new(ViewState { running: false }));
        sched::attach_host_state(id, Box::new(WrenActivation { aside: None }));
    }
}

/// wren_lift's world asked its threads to stop, or let them go: the
/// core's safepoints answer meanwhile, and its poll epoch moves so
/// compiled loops reach one.
pub unsafe extern "C" fn host_poll(on: bool) {
    COLLECTING.with(|c| c.set(on));
    heap::hosted_stop(on);
    if on {
        sched::request_poll();
    }
}

// ── The world ────────────────────────────────────────────────────────────

fn deadline(ns: u64) -> Option<Instant> {
    (ns != NO_DEADLINE).then(|| Instant::now() + Duration::from_nanos(ns))
}

/// A Wren task on the core's world: the context wren_lift steps.
struct WrenTask(*mut c_void);

// SAFETY: the context is stepped only on the world it is placed on, and
// wren_lift makes its fiber there.
unsafe impl Send for WrenTask {}

impl Task for WrenTask {
    fn step(&mut self) -> Suspension {
        // A park the step asked for is on the core's running task; the
        // world parks it when this returns.
        if unsafe { wlift_rt_task_step()(self.0) } {
            Suspension::Pending
        } else {
            Suspension::Completed
        }
    }
}

pub unsafe extern "C" fn waiter_new(_vm: *mut c_void) -> u64 {
    sched::new_waiter().token()
}

pub unsafe extern "C" fn waiter_discard(_vm: *mut c_void, token: u64) {
    sched::discard(token);
}

pub unsafe extern "C" fn wake(token: u64) -> bool {
    sched::wake_token(token)
}

pub unsafe extern "C" fn waiter_ready(_vm: *mut c_void, token: u64) -> i32 {
    if sched::adopt(token).is_none() {
        return -1;
    }
    match sched::notified_before_park(token) {
        Some(true) => 1,
        Some(false) => 0,
        None => -1,
    }
}

pub unsafe extern "C" fn park_request(_vm: *mut c_void, token: u64, deadline_ns: u64) {
    if let Some(waiter) = sched::adopt(token) {
        sched::request_park(waiter, deadline(deadline_ns));
    }
}

pub unsafe extern "C" fn park_pending(_vm: *mut c_void) -> bool {
    sched::park_pending()
}

pub unsafe extern "C" fn resume_woken(_vm: *mut c_void) -> bool {
    sched::resume_cause() == ResumeCause::Notified
}

pub unsafe extern "C" fn park_drive(_vm: *mut c_void, token: u64, deadline_ns: u64) -> bool {
    // The caller drives the world meanwhile, so the thread owns one from
    // here: a thread without one could only poll for the wake.
    if !sched::is_on_task() {
        sched::world_id();
    }
    let Some(waiter) = sched::adopt(token) else {
        return false;
    };
    sched::park(waiter, deadline(deadline_ns))
}

/// wren_lift's switch, for the core to suspend a Wren task from inside.
fn suspend_wren() -> bool {
    unsafe { wlift_rt_task_suspend()() }
}

pub unsafe extern "C" fn spawn(_vm: *mut c_void, task: *mut c_void, on_pool: bool) {
    let task = Box::new(WrenTask(task));
    if on_pool {
        sched::spawn_task_on_pool(task, Some(suspend_wren));
    } else {
        sched::spawn_task(task, Some(suspend_wren));
    }
}

pub unsafe extern "C" fn tick(_vm: *mut c_void, deadline_ns: u64) -> bool {
    sched::tick(deadline(deadline_ns))
}

/// On a task, a yield: the world's idle is the main context's.
pub unsafe extern "C" fn idle(_vm: *mut c_void, deadline_ns: u64) {
    if sched::is_on_task() {
        sched::yield_now();
    } else {
        sched::scheduler_idle(deadline(deadline_ns));
    }
}

pub unsafe extern "C" fn live(_vm: *mut c_void) -> usize {
    sched::live_tasks()
}

pub unsafe extern "C" fn workers(_vm: *mut c_void) -> usize {
    sched::worker_count()
}
