//! Worker worlds: OS threads that each run a world, for task bodies that
//! may run off the spawning thread. A task goes to the least-loaded worker
//! before its stack exists and never moves afterwards.

use std::cell::Cell;

use krio_core::TaskId;

pub(super) use super::task::Placed;

thread_local! {
    static POOL_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread is one the pool started. An adapter that keeps
/// per-thread machinery on its main world uses it to keep pooled tasks off
/// that machinery.
pub fn is_pool_worker() -> bool {
    POOL_WORKER.with(Cell::get)
}

#[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
mod threaded {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    use krio_core::TaskId;

    use super::Placed;
    use crate::heap;
    use crate::sched::trace;
    use crate::sched::world::{self, WorldCommand, WorldEndpoint};

    pub(super) struct WorkerPool {
        /// Behind a lock because on wasm the list grows: see `dispatch`.
        /// Elsewhere it is written once.
        workers: Mutex<Vec<Arc<WorldEndpoint>>>,
        next: AtomicUsize,
    }

    static WORKER_POOL: OnceLock<Option<WorkerPool>> = OnceLock::new();

    /// `CARIBOU_WORKERS` (or `ASH_WORKERS`): worker worlds to start, 0 for
    /// none; default is the core count less one. A value that does not
    /// parse falls back to the default and says so, since a typo read as
    /// zero would look like a speed-up rather than a mistake.
    pub(super) fn configured_worker_count() -> usize {
        static COUNT: OnceLock<usize> = OnceLock::new();
        *COUNT.get_or_init(|| {
            let machine_default = || {
                std::thread::available_parallelism()
                    .map(|count| count.get().saturating_sub(1))
                    .unwrap_or(0)
            };
            let (name, value) = match std::env::var("CARIBOU_WORKERS") {
                Ok(value) => ("CARIBOU_WORKERS", value),
                Err(_) => match std::env::var("ASH_WORKERS") {
                    Ok(value) => ("ASH_WORKERS", value),
                    Err(_) => return machine_default(),
                },
            };
            match value.trim().parse::<usize>() {
                Ok(n) => n,
                Err(_) => {
                    let n = machine_default();
                    eprintln!(
                        "[caribou] {name}={value:?} is not a worker count; using {n}. \
                         Set {name}=0 to run every task on the spawning world."
                    );
                    n
                }
            }
        })
    }

    fn worker_pool() -> Option<&'static WorkerPool> {
        WORKER_POOL.get_or_init(spawn_worker_pool).as_ref()
    }

    fn spawn_worker_pool() -> Option<WorkerPool> {
        // Empty on wasm: the pool there grows as tasks are created, so
        // there is nothing to size up front and nothing to size it from.
        if cfg!(target_family = "wasm") {
            return Some(WorkerPool {
                workers: Mutex::new(Vec::new()),
                next: AtomicUsize::new(0),
            });
        }
        let count = configured_worker_count();
        if count == 0 {
            return None;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut started = 0usize;
        for index in 0..count {
            let sender = sender.clone();
            let spawn = std::thread::Builder::new()
                .name(format!("caribou-world-{index}"))
                .spawn(move || worker_main(Some(sender), None));
            if spawn.is_ok() {
                started += 1;
            }
        }
        drop(sender);
        // A worker that dies before publishing its endpoint must not hang
        // startup; dispatch is correct over any non-empty subset.
        let mut workers = Vec::with_capacity(started);
        for _ in 0..started {
            match receiver.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(endpoint) => workers.push(endpoint),
                Err(_) => break,
            }
        }
        (!workers.is_empty()).then(|| WorkerPool {
            workers: Mutex::new(workers),
            next: AtomicUsize::new(0),
        })
    }

    /// One worker: its own world, and every task it is given.
    ///
    /// A pool sized up front hands it `sender` and waits to hear back; a
    /// pool that grows hands it `first` and waits for nothing, and the
    /// worker lists itself only after taking that job, so it is not picked
    /// as idle and given a second one.
    fn worker_main(
        sender: Option<std::sync::mpsc::Sender<Arc<WorldEndpoint>>>,
        first: Option<WorldCommand>,
    ) {
        super::POOL_WORKER.with(|worker| worker.set(true));
        heap::gc_register_current_os_thread();
        let endpoint = world::endpoint();
        trace("worker-ready", world::world_id(), 0);
        if let Some(command) = first {
            endpoint.assigned.fetch_add(1, Ordering::AcqRel);
            endpoint.push(command);
        }
        match sender {
            Some(sender) => {
                if sender.send(Arc::clone(&endpoint)).is_err() {
                    heap::gc_unregister_current_os_thread();
                    return;
                }
            }
            None => {
                if let Some(pool) = worker_pool() {
                    pool.workers
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(Arc::clone(&endpoint));
                }
            }
        }
        loop {
            // Every time round: a world whose turns keep reporting progress
            // never reaches the idle wait, and nothing on this path takes
            // the heap lock, which is where the rest reaches a safepoint.
            heap::gc_safepoint();
            if world::schedule_step() {
                continue;
            }
            world::scheduler_idle(None);
        }
    }

    /// Hand one task to one worker, and count it against that worker.
    fn assign(worker: &WorldEndpoint, id: TaskId, index: usize, placed: Placed) {
        worker.assigned.fetch_add(1, Ordering::AcqRel);
        trace("dispatch", id.0, index as u64);
        worker.push(WorldCommand::Spawn { id, placed });
    }

    /// Rotate the starting point so equal loads do not favour lane zero,
    /// then take the least-loaded worker.
    #[cfg(not(target_family = "wasm"))]
    pub(super) fn dispatch(id: TaskId, placed: Placed) -> Result<(), Placed> {
        let Some(pool) = worker_pool() else {
            return Err(placed);
        };
        let workers = pool
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if workers.is_empty() {
            return Err(placed);
        }
        let start = pool.next.fetch_add(1, Ordering::Relaxed) % workers.len();
        let index = (0..workers.len())
            .min_by_key(|offset| {
                let index = (start + offset) % workers.len();
                workers[index].assigned.load(Ordering::Acquire)
            })
            .map(|offset| (start + offset) % workers.len())
            .unwrap_or(start);
        assign(&workers[index], id, index, placed);
        Ok(())
    }

    /// A wasm worker runs a body straight through and cannot hold a second
    /// task while the first waits, so a fixed pool of N would make the
    /// N+1th thread wait for one to finish. The pool grows instead: one
    /// agent per live task, as many as the host will give. The task goes
    /// with the agent as it starts rather than after it reports ready, so
    /// N thread creations overlap instead of queueing.
    #[cfg(target_family = "wasm")]
    pub(super) fn dispatch(id: TaskId, placed: Placed) -> Result<(), Placed> {
        let Some(pool) = worker_pool() else {
            return Err(placed);
        };
        let workers = pool
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = workers
            .iter()
            .position(|worker| worker.assigned.load(Ordering::Acquire) == 0)
        {
            assign(&workers[index], id, index, placed);
            return Ok(());
        }
        let at = workers.len();
        // Dropped first: two threads racing to grow costs a spare agent,
        // blocking one behind the other costs the parallelism this is for.
        drop(workers);
        trace("dispatch", id.0, at as u64);
        // The body stays reachable from here until the agent takes it, so a
        // host that gives no more threads hands it back to run locally.
        let slot: Arc<Mutex<Option<Placed>>> = Arc::new(Mutex::new(Some(placed)));
        let agent_slot = Arc::clone(&slot);
        let spawned = std::thread::Builder::new()
            .name(format!("caribou-world-{at}"))
            .spawn(move || {
                let first = agent_slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                    .map(|placed| WorldCommand::Spawn { id, placed });
                worker_main(None, first)
            });
        match spawned {
            Ok(_) => Ok(()),
            Err(_) => Err(slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .expect("an unstarted agent leaves the body in its slot")),
        }
    }
}

#[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
pub(super) fn dispatch(id: TaskId, placed: Placed) -> Result<(), Placed> {
    threaded::dispatch(id, placed)
}

/// Whether `spawn_fiber_on_pool` may place a task off the calling world.
/// Reads the configuration only; the pool starts on the first dispatch. On
/// wasm the host answers by granting or refusing the first thread.
#[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
pub fn has_worker_pool() -> bool {
    cfg!(target_family = "wasm") || threaded::configured_worker_count() != 0
}

/// Worker worlds a task may be placed on. Reads the configuration only,
/// as [`has_worker_pool`] does.
#[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
pub fn worker_count() -> usize {
    threaded::configured_worker_count()
}

#[cfg(all(target_family = "wasm", not(target_feature = "atomics")))]
pub fn worker_count() -> usize {
    0
}

#[cfg(all(target_family = "wasm", not(target_feature = "atomics")))]
pub fn has_worker_pool() -> bool {
    false
}

/// No threads to make a pool from; the spawning world runs the task.
#[cfg(all(target_family = "wasm", not(target_feature = "atomics")))]
pub(super) fn dispatch(id: TaskId, placed: Placed) -> Result<(), Placed> {
    let _ = id;
    Err(placed)
}
