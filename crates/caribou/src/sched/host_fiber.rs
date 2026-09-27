//! Fibers a host lends on wasm, where krio cannot switch stacks.
//!
//! The runtime linked into a program installs its fibers here
//! ([`install`]); a task body that may suspend then runs on one, and a
//! suspension inside it goes through the host. With none installed a body
//! runs to the end in one step.
//!
//! The installed yield is krio's suspender, so library code far from the
//! scheduler suspends the same way. It suspends only on a host fiber:
//! anywhere else it returns at once, since a host's suspension unwinds to
//! the fiber's entry and there is none to stop at outside one.

use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// How a host fiber stopped.
pub enum HostStep {
    /// It suspended; the next resume continues it.
    Yielded,
    /// Its body returned.
    Done,
    /// Its body panicked.
    Errored,
}

/// One fiber of the host's.
pub trait HostFiber {
    /// Run the body, or continue it from where it suspended, until it
    /// suspends or ends.
    fn resume(&mut self) -> HostStep;
    /// The memory the collector scans for a suspended fiber's values:
    /// start and length, address-stable for the fiber's life.
    fn stack_range(&self) -> (usize, usize);
    /// Where the live part of the range starts while suspended; zero
    /// otherwise.
    fn saved_sp(&self) -> usize;
}

/// What a host fiber runs. It may be entered more than once to continue
/// it, so it is `FnMut`.
pub type FiberBody = Box<dyn FnMut()>;

/// The host's fibers.
pub struct HostFibers {
    /// A fiber over `body`.
    pub make: fn(body: FiberBody) -> Box<dyn HostFiber>,
    /// Suspend the running fiber.
    pub suspend: fn(),
}

static HOST: OnceLock<HostFibers> = OnceLock::new();

thread_local! {
    /// The id of the host fiber running on this thread; 0 for none.
    static RUNNING: Cell<u64> = const { Cell::new(0) };
}

/// Lend the scheduler the host's fibers. The first install wins.
pub fn install(fibers: HostFibers) {
    if HOST.set(fibers).is_ok() {
        krio_fiber::set_suspender(suspend);
    }
}

pub(super) fn installed() -> bool {
    HOST.get().is_some()
}

pub(super) fn make(body: FiberBody) -> Option<Box<dyn HostFiber>> {
    HOST.get().map(|host| (host.make)(body))
}

/// Run `f` as the host fiber `id`.
pub(super) fn running<R>(id: u64, f: impl FnOnce() -> R) -> R {
    let outer = RUNNING.with(|running| running.replace(id));
    let result = f();
    RUNNING.with(|running| running.set(outer));
    result
}

/// The id of the host fiber running on this thread; 0 for none.
pub(super) fn current() -> u64 {
    RUNNING.with(Cell::get)
}

/// krio's suspender: the host's suspension on a host fiber, nothing
/// anywhere else.
fn suspend() {
    if current() != 0
        && let Some(host) = HOST.get()
    {
        (host.suspend)();
    }
}

/// An id for a host fiber's stack, in the heap's registry of fiber stacks.
/// krio makes no fibers where the host lends them, so its ids are free.
pub(super) fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
