//! What a world schedules: a task body of either kind, plus the per-task
//! bookkeeping the scheduler keeps beside it.

use std::any::Any;

use krio_core::{Suspension, Task, TaskId};

#[cfg(not(target_family = "wasm"))]
use crate::heap;

/// Default stack for a stackful task. wren_lift-proven; 64 KB tripped on
/// real workloads there.
pub const DEFAULT_STACK_SIZE: usize = 256 * 1024;

/// Per-task state an adapter keeps in the thread's live cells while the
/// task runs: Ash's trap chain and pending exception, Zyntax's handler
/// stack. The scheduler calls `swap_in` before resuming the task and
/// `swap_out` after it yields; the object owns the contents, the scheduler
/// owns the ordering. Neither may call back into the scheduler.
///
/// `Any` so an adapter can downcast what it attached: cast the
/// `&mut dyn HostState` to `&mut dyn Any` first.
pub trait HostState: Any {
    fn swap_in(&mut self);
    fn swap_out(&mut self);
}

/// Observes every switch, after the suspended stack pointer has been
/// published to the heap. `TaskId::NONE` is the main context.
pub type SwitchHook = fn(from: TaskId, to: TaskId);

/// Why the current task was resumed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResumeCause {
    #[default]
    Scheduled,
    Notified,
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunState {
    Runnable,
    Running,
    /// Parked on the wait token.
    Waiting(u64),
}

/// A task body: a krio fiber with its own stack, or a state machine the
/// scheduler steps on its own stack. Where the host cannot switch stacks
/// there is only the second kind.
pub(super) enum Body {
    #[cfg(not(target_family = "wasm"))]
    Stackful(StackfulTask),
    Stackless(Box<dyn Task>),
}

impl Body {
    /// Build the body `spawn_fiber` asks for.
    pub(super) fn fiber(id: TaskId, stack_size: usize, body: Box<dyn FnOnce()>) -> Self {
        #[cfg(not(target_family = "wasm"))]
        {
            Body::Stackful(StackfulTask::new(id, stack_size, body))
        }
        #[cfg(target_family = "wasm")]
        {
            let _ = (id, stack_size);
            Body::Stackless(Box::new(RunThrough(Some(body))))
        }
    }

    /// Whether `park` and `yield_now` can suspend this body from inside.
    /// Natively only a fiber can; on wasm every body suspends through the
    /// host, or not at all, and either way the call returns.
    pub(super) fn can_suspend(&self) -> bool {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Body::Stackful(_) => true,
            Body::Stackless(_) => cfg!(target_family = "wasm"),
        }
    }

    pub(super) fn step(&mut self, id: TaskId) -> Suspension {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Body::Stackful(task) => task.step(),
            Body::Stackless(task) => {
                // A panic must not unwind through the scheduler's frames
                // with the active-task slot still set; a fiber's trampoline
                // already catches at the same boundary.
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task.step())) {
                    Ok(suspension) => suspension,
                    Err(_) => {
                        eprintln!("[caribou] task {} terminated with a panic", id.0);
                        Suspension::Completed
                    }
                }
            }
        }
    }

    /// Publish the suspended stack pointer to the heap. Runs before the
    /// switch hook and before the host state is swapped back, so nothing
    /// that may collect sees a fiber stack without its live window.
    pub(super) fn publish_sp(&mut self) {
        #[cfg(not(target_family = "wasm"))]
        if let Body::Stackful(task) = self {
            task.publish_sp();
        }
    }

    pub(super) fn suspended_sp(&self) -> Option<usize> {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Body::Stackful(task) => (task.published_sp != 0).then_some(task.published_sp),
            Body::Stackless(_) => None,
        }
    }
}

/// A body that runs its closure to the end in one step. What `spawn_fiber`
/// makes on wasm: the host's suspension, if any, happens inside the step.
#[cfg(target_family = "wasm")]
struct RunThrough(Option<Box<dyn FnOnce()>>);

#[cfg(target_family = "wasm")]
impl Task for RunThrough {
    fn step(&mut self) -> Suspension {
        if let Some(body) = self.0.take() {
            body();
        }
        Suspension::Completed
    }
}

/// A krio fiber whose stack the heap scans.
#[cfg(not(target_family = "wasm"))]
pub(super) struct StackfulTask {
    fiber: krio_fiber::Fiber,
    /// The low bits of the task id: the heap keys fiber stacks by a u32
    /// (git-bug 0c7bfb5c717452391ab18aec34727a546c94bd7e2e24659c0618cc731f024d00).
    gc_id: u32,
    /// The stack pointer last published to the heap; zero before the
    /// first suspension.
    published_sp: usize,
}

#[cfg(not(target_family = "wasm"))]
impl StackfulTask {
    fn new(id: TaskId, stack_size: usize, body: Box<dyn FnOnce()>) -> Self {
        let fiber = krio_fiber::Fiber::with_stack_size(stack_size, body);
        let (base, len) = fiber.stack_range();
        let gc_id = id.0 as u32;
        // Registering a stack needs the heap singleton; an adapter that has
        // not initialised it yet gets it here.
        heap::init();
        // SAFETY: the range is the fiber's own stack, address-stable until
        // the fiber drops, and `Drop` unregisters it first.
        unsafe { heap::gc_register_fiber_stack(gc_id, base as usize, len) };
        // The off-heap stack counts as allocation pressure, so dead tasks'
        // stacks still turn into collections.
        heap::track_external(len as u64);
        Self {
            fiber,
            gc_id,
            published_sp: 0,
        }
    }

    fn publish_sp(&mut self) {
        let sp = self.fiber.saved_sp() as usize;
        // SAFETY: `gc_id` was registered in `new` and is not yet unregistered.
        unsafe { heap::gc_update_fiber_sp(self.gc_id, sp) };
        self.published_sp = sp;
    }
}

#[cfg(not(target_family = "wasm"))]
impl Task for StackfulTask {
    fn step(&mut self) -> Suspension {
        // Where the main stack is suspended while this one runs; the heap
        // scans upward from here whenever a fiber is the running stack.
        let probe = 0usize;
        // SAFETY: id 0 is this thread's main-stack descriptor, registered
        // alongside the first fiber stack.
        unsafe { heap::gc_update_fiber_sp(0, &probe as *const usize as usize) };
        match self.fiber.resume() {
            krio_fiber::FiberStep::Yielded => Suspension::Yielded,
            krio_fiber::FiberStep::Done => Suspension::Completed,
            krio_fiber::FiberStep::Errored => {
                drop(self.fiber.take_error());
                eprintln!("[caribou] task {} terminated with a panic", self.gc_id);
                Suspension::Completed
            }
        }
    }
}

#[cfg(not(target_family = "wasm"))]
impl Drop for StackfulTask {
    fn drop(&mut self) {
        // Before the fiber frees its stack.
        // SAFETY: registered in `new`; nothing publishes it after this.
        unsafe { heap::gc_unregister_fiber_stack(self.gc_id) };
    }
}

pub(super) struct TaskRecord {
    /// Taken out for the duration of a step, so the world can be borrowed
    /// from the running task.
    pub(super) body: Option<Body>,
    pub(super) run_state: RunState,
    pub(super) resume_cause: ResumeCause,
    pub(super) gc_blocking_depth: u32,
    pub(super) host: Option<Box<dyn HostState>>,
}

impl TaskRecord {
    pub(super) fn new(body: Body) -> Self {
        Self {
            body: Some(body),
            run_state: RunState::Runnable,
            resume_cause: ResumeCause::Scheduled,
            gc_blocking_depth: 0,
            host: None,
        }
    }
}
