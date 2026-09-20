//! A quiescent point across the worker worlds: every worker parked between
//! turns while the calling world does something no task may run through,
//! a reload. The calling world's own tasks are between turns already,
//! since it runs this from its main context.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::heap;

/// Set while a quiescent point is wanted; workers park on seeing it.
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Workers parked so far, and the condition both sides wait on.
static PARKED: Mutex<usize> = Mutex::new(0);
static CHANGED: Condvar = Condvar::new();
/// One quiescent point at a time.
static ONE: Mutex<()> = Mutex::new(());

/// How long to wait for the workers before going ahead without the ones
/// still out: a worker inside a native call that blocks for longer runs
/// no task code meanwhile, so it holds the world's tables no more than a
/// parked one does.
const PATIENCE: Duration = Duration::from_secs(1);

/// Run `f` with every worker world parked between turns.
pub fn quiesce<R>(f: impl FnOnce() -> R) -> R {
    let workers = super::pool::started_workers();
    if workers.is_empty() {
        return f();
    }
    let _one = ONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    REQUESTED.store(true, Ordering::Release);
    // Wakes a worker idle in its wait; the command itself does nothing.
    for worker in &workers {
        worker.push(super::world::WorldCommand::Park);
    }
    // A worker that parks may first have to finish a collection that needs
    // this thread at a safepoint, so the wait is announced as blocking.
    heap::gc_set_blocking(true);
    {
        let deadline = Instant::now() + PATIENCE;
        let mut parked = PARKED.lock().unwrap();
        while *parked < workers.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            parked = CHANGED.wait_timeout(parked, left).unwrap().0;
        }
    }
    heap::gc_set_blocking(false);
    let result = f();
    REQUESTED.store(false, Ordering::Release);
    CHANGED.notify_all();
    // Released one by one before the next quiescent point may count them;
    // a worker leaving may again have a collection to finish first.
    heap::gc_set_blocking(true);
    {
        let mut parked = PARKED.lock().unwrap();
        while *parked > 0 {
            parked = CHANGED.wait(parked).unwrap();
        }
    }
    heap::gc_set_blocking(false);
    result
}

/// Whether a worker should park now.
pub(super) fn requested() -> bool {
    REQUESTED.load(Ordering::Acquire)
}

/// Park this worker until the quiescent point ends. Called between turns.
pub(super) fn park() {
    heap::gc_set_blocking(true);
    {
        let mut parked = PARKED.lock().unwrap();
        *parked += 1;
        CHANGED.notify_all();
        while REQUESTED.load(Ordering::Acquire) {
            parked = CHANGED.wait(parked).unwrap();
        }
        *parked -= 1;
        CHANGED.notify_all();
    }
    heap::gc_set_blocking(false);
}
