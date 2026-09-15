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
//! The view is safe for wren_lift's collector while the core runs other
//! tasks on its thread, and the seam hears nothing of it, since the
//! thread is in no wait: `task_step` makes the view running for the
//! step, and the driver's park, tick and idle here make it safe around
//! the core's turns, on the thread's own stack. Inside a fiber of the
//! core's, which wren_lift's collector cannot place, the view stays
//! running.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use caribou::sched::{self, ResumeCause, Suspension, Task};
use wren_lift::runtime::rt::{NO_DEADLINE, wlift_rt_task_step, wlift_rt_task_suspend};
use wren_lift::runtime::vm::{Spill, VM};

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

/// Whether the calling stack is the thread's own: where wren_lift's
/// collector can place a safe view.
fn on_thread_stack() -> bool {
    sched::current_stack() == 0
}

pub unsafe extern "C" fn park_drive(vm: *mut c_void, token: u64, deadline_ns: u64) -> bool {
    // The caller drives the world meanwhile, so the thread owns one from
    // here: a thread without one could only poll for the wake.
    if !sched::is_on_task() {
        sched::world_id();
    }
    let Some(waiter) = sched::adopt(token) else {
        return false;
    };
    let vm = vm as *mut VM;
    let safe = on_thread_stack() && !unsafe { (*vm).thread.is_safe() };
    let mut spill = Spill::new();
    if safe {
        unsafe { (*vm).enter_safe_here(&mut spill) };
    }
    let woken = sched::park(waiter, deadline(deadline_ns));
    if safe {
        unsafe { (*vm).leave_safe_here() };
    }
    std::hint::black_box(&spill);
    woken
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

pub unsafe extern "C" fn tick(vm: *mut c_void, deadline_ns: u64) -> bool {
    let vm = vm as *mut VM;
    let safe = !sched::is_on_task() && on_thread_stack() && !unsafe { (*vm).thread.is_safe() };
    let mut spill = Spill::new();
    if safe {
        unsafe { (*vm).enter_safe_here(&mut spill) };
    }
    let live = sched::tick(deadline(deadline_ns));
    if safe {
        unsafe { (*vm).leave_safe_here() };
    }
    std::hint::black_box(&spill);
    live
}

pub unsafe extern "C" fn idle(vm: *mut c_void, deadline_ns: u64) {
    if sched::is_on_task() {
        sched::yield_now();
        return;
    }
    let vm = vm as *mut VM;
    let safe = on_thread_stack() && !unsafe { (*vm).thread.is_safe() };
    let mut spill = Spill::new();
    if safe {
        unsafe { (*vm).enter_safe_here(&mut spill) };
    }
    sched::scheduler_idle(deadline(deadline_ns));
    if safe {
        unsafe { (*vm).leave_safe_here() };
    }
    std::hint::black_box(&spill);
}

pub unsafe extern "C" fn live(_vm: *mut c_void) -> usize {
    sched::live_tasks()
}

pub unsafe extern "C" fn workers(_vm: *mut c_void) -> usize {
    sched::worker_count()
}
