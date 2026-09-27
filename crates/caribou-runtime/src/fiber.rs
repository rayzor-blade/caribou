//! Ash's fibers for caribou's scheduler in a wasm program
//! (`caribou::sched::host_fiber`). Each is Ash's `guest::Fiber`: a side
//! stack that the link-time transform saves a suspended fiber's frames
//! into, and a shadow stack of its own, in one range the collector scans.

use ash_wasm_runtime::guest::{Fiber, FiberState, yield_now};
use caribou::sched::host_fiber::{self, HostFiber, HostFibers, HostStep};

struct AshFiber(Fiber);

impl HostFiber for AshFiber {
    fn resume(&mut self) -> HostStep {
        self.0.resume();
        match self.0.state() {
            FiberState::Suspended => HostStep::Yielded,
            FiberState::Errored => HostStep::Errored,
            _ => HostStep::Done,
        }
    }

    fn stack_range(&self) -> (usize, usize) {
        let (base, len) = self.0.stack_range();
        (base as usize, len)
    }

    fn saved_sp(&self) -> usize {
        self.0.saved_sp() as usize
    }
}

/// Lend the scheduler Ash's fibers, with Ash's default side stack.
pub(crate) fn install() {
    host_fiber::install(HostFibers {
        make: |body| Box::new(AshFiber(Fiber::with_stack_size(0, body))),
        suspend: yield_now,
    });
}
