//! Zyntax's handler stack is the thread's, and several of the world's
//! stacks run Zyntax code on one thread: a `with` scope one stack leaves
//! open across a park would be in scope for the next stack's code. So each
//! stack runs in a segment of its own, entered when the scheduler switches
//! to the stack and left when it switches away (`sched::switch_stack`). An
//! async call's task needs no segment of its own: Zyntax's `HostTask`
//! brackets each step.
//!
//! A stack the thread ran before its first Zyntax runtime came up has no
//! segment, and shares the frames of whatever runs beside it.

use std::cell::RefCell;
use std::sync::Once;

use caribou::sched::{self, HostState};
use zyntax_embed::{HandlerSegmentScope, TieredRuntime};

thread_local! {
    /// The thread's live runtimes. The segment calls act on the thread's
    /// handler stack, whichever runtime makes them, so any one serves.
    static RUNTIMES: RefCell<Vec<*const TieredRuntime>> = const { RefCell::new(Vec::new()) };
}

/// One stack's segment: its id, and the scope open while the stack runs.
struct Segment {
    id: i64,
    scope: Option<HandlerSegmentScope>,
}

impl HostState for Segment {
    fn swap_in(&mut self) {
        if let Some(runtime) = runtime() {
            self.scope = Some(runtime.enter_handler_segment(self.id));
        }
    }

    fn swap_out(&mut self) {
        if let (Some(runtime), Some(scope)) = (runtime(), self.scope.take()) {
            runtime.leave_handler_segment(self.id, scope);
        }
    }
}

fn runtime() -> Option<&'static TieredRuntime> {
    let runtime = RUNTIMES.with(|runtimes| runtimes.borrow().last().copied())?;
    // SAFETY: added by `install` and removed by `uninstall` before the
    // runtime is dropped, on this thread.
    unsafe { runtime.as_ref() }
}

/// A stack's first turn on a thread with a Zyntax runtime: its segment,
/// entered by the swap that follows.
fn attach(stack: u64) {
    if let Some(runtime) = runtime() {
        let id = runtime.new_handler_segment();
        sched::attach_stack_host_state(stack, Box::new(Segment { id, scope: None }));
    }
}

/// Give the stacks of this thread their segments, through `runtime` while
/// it lives. With the thread's first runtime, the running stack's segment
/// opens now, since it is already running.
pub fn install(runtime: &TieredRuntime) {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| sched::add_stack_hook(attach));
    let first = RUNTIMES.with(|runtimes| {
        let mut runtimes = runtimes.borrow_mut();
        runtimes.push(runtime);
        runtimes.len() == 1
    });
    if !first {
        return;
    }
    let id = runtime.new_handler_segment();
    let scope = Some(runtime.enter_handler_segment(id));
    sched::attach_stack_host_state(sched::current_stack(), Box::new(Segment { id, scope }));
}

/// `runtime` is going: the stacks stop using it.
pub fn uninstall(runtime: &TieredRuntime) {
    RUNTIMES.with(|runtimes| {
        runtimes
            .borrow_mut()
            .retain(|&live| !std::ptr::eq(live, runtime))
    });
}
