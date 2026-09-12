//! Wait tokens: the one blocking primitive, and the registry that lets a
//! wake from any thread find the world and task that are parked on it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use krio_core::TaskId;

use super::task::ResumeCause;
use super::{trace, world};
use crate::heap;

/// One wait operation by one task (or the main context) of one world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Waiter {
    /// Zero for a thread that owns no world.
    world: u64,
    task: TaskId,
    token: u64,
}

impl Waiter {
    pub fn world(&self) -> u64 {
        self.world
    }

    pub fn task(&self) -> TaskId {
        self.task
    }

    pub fn token(&self) -> u64 {
        self.token
    }

    /// Rebuild a waiter an adapter carried across an ABI in its own layout;
    /// the parts must be ones this world handed out.
    pub fn from_parts(world: u64, task: TaskId, token: u64) -> Self {
        Self { world, task, token }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitStatus {
    Waiting,
    Notified,
    TimedOut,
}

#[derive(Clone, Copy)]
struct WaitRegistration {
    world: u64,
    task: TaskId,
    status: WaitStatus,
}

static WAIT_REGISTRY: LazyLock<Mutex<HashMap<u64, WaitRegistration>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_WAIT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// A token for the calling context. Registered before it is handed out,
/// so a wake that races the park is not lost.
pub fn new_waiter() -> Waiter {
    let token = NEXT_WAIT_TOKEN.fetch_add(1, Ordering::Relaxed).max(1);
    let waiter = Waiter {
        world: world::try_world_id().unwrap_or(0),
        task: world::current_task(),
        token,
    };
    WAIT_REGISTRY.lock().unwrap().insert(
        token,
        WaitRegistration {
            world: waiter.world,
            task: waiter.task,
            status: WaitStatus::Waiting,
        },
    );
    waiter
}

/// Notify a waiter. `false` if it was already notified, timed out or
/// finished; the token check makes stale entries harmless.
pub fn wake(waiter: Waiter) -> bool {
    if !claim_notification(waiter) {
        return false;
    }
    if waiter.world == 0 {
        // A foreign thread polls the registry; there is no world to tell.
        return true;
    }
    match world::endpoint_of(waiter.world) {
        Some(endpoint) => {
            trace("wake", waiter.task.0, waiter.token);
            endpoint.push(world::WorldCommand::Wake(waiter));
            true
        }
        None => {
            WAIT_REGISTRY.lock().unwrap().remove(&waiter.token);
            false
        }
    }
}

fn claim_notification(waiter: Waiter) -> bool {
    let mut waiters = WAIT_REGISTRY.lock().unwrap();
    let Some(registration) = waiters.get_mut(&waiter.token) else {
        return false;
    };
    if registration.world != waiter.world
        || registration.task != waiter.task
        || registration.status != WaitStatus::Waiting
    {
        return false;
    }
    registration.status = WaitStatus::Notified;
    true
}

pub(super) fn claim_timeout(token: u64) -> bool {
    let mut waiters = WAIT_REGISTRY.lock().unwrap();
    let Some(registration) = waiters.get_mut(&token) else {
        return false;
    };
    if registration.status != WaitStatus::Waiting {
        return false;
    }
    registration.status = WaitStatus::TimedOut;
    true
}

fn wait_status(token: u64) -> Option<WaitStatus> {
    WAIT_REGISTRY
        .lock()
        .unwrap()
        .get(&token)
        .map(|registration| registration.status)
}

fn finish_wait(token: u64) -> Option<WaitStatus> {
    WAIT_REGISTRY
        .lock()
        .unwrap()
        .remove(&token)
        .map(|registration| registration.status)
}

/// Block until the waiter is notified or the deadline passes. On a task:
/// record the request and yield. On the main context: drive turns and idle.
/// On a thread the runtime did not create: poll, never drive, because it
/// has no fiber to yield and may not run tasks. Returns whether it was
/// notified.
///
/// A native stackless task cannot block here; it records the wait with
/// [`request_park`], returns `Pending`, and reads [`world::resume_cause`]
/// on its next step.
pub fn park(waiter: Waiter, deadline: Option<Instant>) -> bool {
    debug_assert_eq!(waiter.task, world::current_task());
    if world::is_on_task() {
        park_task(waiter, deadline)
    } else if world::has_world() {
        park_main(waiter, deadline)
    } else {
        park_foreign(waiter, deadline)
    }
}

fn park_task(waiter: Waiter, deadline: Option<Instant>) -> bool {
    if wait_status(waiter.token) == Some(WaitStatus::Notified) {
        finish_wait(waiter.token);
        return true;
    }
    let can_suspend = request_park(waiter, deadline);
    assert!(
        can_suspend,
        "park on a stackless task: record the wait with request_park and return Suspension::Pending"
    );
    world::suspend_current();
    let notified = world::resume_cause() == ResumeCause::Notified
        || wait_status(waiter.token) == Some(WaitStatus::Notified);
    finish_wait(waiter.token);
    notified
}

/// Leave a park request on the running task without yielding; the task is
/// parked when its step returns. Returns whether the task can suspend from
/// inside, i.e. whether a blocking [`park`] would have worked. `false` when
/// called off a task.
pub fn request_park(waiter: Waiter, deadline: Option<Instant>) -> bool {
    world::set_pending_park(waiter, deadline)
}

fn park_main(waiter: Waiter, deadline: Option<Instant>) -> bool {
    trace("park-main", waiter.token, deadline.is_some() as u64);
    loop {
        // A registered mutator that never reaches a safepoint holds up
        // every world stop for as long as it waits.
        heap::gc_safepoint();
        match wait_status(waiter.token) {
            Some(WaitStatus::Notified) => {
                finish_wait(waiter.token);
                return true;
            }
            Some(WaitStatus::TimedOut) | None => {
                finish_wait(waiter.token);
                return false;
            }
            Some(WaitStatus::Waiting) => {}
        }
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            let _ = claim_timeout(waiter.token);
            return finish_wait(waiter.token) == Some(WaitStatus::Notified);
        }
        if !world::schedule_step() {
            world::scheduler_idle(deadline);
        }
    }
}

fn park_foreign(waiter: Waiter, deadline: Option<Instant>) -> bool {
    trace("park-foreign", waiter.token, deadline.is_some() as u64);
    loop {
        heap::gc_safepoint();
        match wait_status(waiter.token) {
            Some(WaitStatus::Notified) => {
                finish_wait(waiter.token);
                return true;
            }
            Some(WaitStatus::TimedOut) | None => {
                finish_wait(waiter.token);
                return false;
            }
            Some(WaitStatus::Waiting) => {}
        }
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            let _ = claim_timeout(waiter.token);
            return finish_wait(waiter.token) == Some(WaitStatus::Notified);
        }
        std::thread::sleep(Duration::from_micros(50));
    }
}

/// Sleep without staying runnable. The main context keeps driving ready
/// tasks while it waits for its own deadline.
pub fn sleep_until(deadline: Instant) {
    let waiter = new_waiter();
    let _ = park(waiter, Some(deadline));
}
