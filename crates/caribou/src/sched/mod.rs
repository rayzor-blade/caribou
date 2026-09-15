//! The scheduler: one world per OS thread, every language's concurrency
//! on it.
//!
//! A world holds its tasks, a ready queue and a timer heap in a
//! thread-local. The unit of scheduling is krio-core's [`Task`]: a
//! stackful task owns a krio fiber and suspends by switching stacks; a
//! stackless one is a state machine whose `step` returns its suspension.
//! A turn ([`schedule_step`]) resumes every task that was ready when it
//! began; parked tasks consume no switch. The main context, the thread's
//! original stack, drives turns; a task never does, it yields.
//!
//! Around each turn the scheduler publishes the suspended stack pointers
//! to the heap and swaps the task's [`HostState`] into the thread's live
//! cells; the [`SwitchHook`] runs after the publish. [`park`] and [`wake`]
//! are the one blocking primitive; locks, conditions and sleeps are built
//! on them. Compiled loops poll [`POLL_EPOCH`], which a timer bumps while
//! tasks exist and the collector bumps when it wants the world stopped.
//!
//! Other threads reach a world only through its endpoint: a wake, or a
//! task to spawn. Worker worlds for pooled bodies are OS threads running
//! the same loop; a task is placed once and never migrates.

mod pool;
mod preempt;
mod task;
mod wait;
mod world;

pub use krio_core::{Suspension, Task, TaskId};

pub use pool::{has_worker_pool, is_pool_worker, worker_count};
pub use preempt::{POLL_EPOCH, any_live_tasks, poll_epoch_address, request_poll};
pub use task::{DEFAULT_STACK_SIZE, HostState, ResumeCause, Suspend, SwitchHook};
pub use wait::{
    Waiter, adopt, discard, new_waiter, notified_before_park, park, request_park, sleep_until,
    waiter_for, wake, wake_token,
};
pub use world::{
    add_task_hook, attach_host_state, block_yield, current_stack, current_task, enter_blocking,
    has_world, is_blocking, is_on_task, leave_blocking, live_tasks, park_pending, poll,
    resume_cause, schedule_step, scheduler_idle, set_switch_hook, spawn, spawn_fiber,
    spawn_fiber_on_pool, spawn_task, spawn_task_on_pool, suspended_sp, task_exists, tick,
    with_host_state, world_id, yield_now,
};

/// `CARIBOU_SCHED_TRACE`: one line per scheduler event on stderr. Safe to
/// run with.
pub(crate) fn trace(label: &str, id: u64, detail: u64) {
    use std::sync::OnceLock;
    use std::time::Instant;

    static START: OnceLock<Instant> = OnceLock::new();
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("CARIBOU_SCHED_TRACE").is_some()) {
        return;
    }
    let elapsed = START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1e3;
    eprintln!("[sched] {elapsed:8.2}ms {label} id={id} detail={detail}");
}
