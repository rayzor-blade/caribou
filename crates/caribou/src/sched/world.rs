//! The world: one OS thread's scheduler. Its tasks, ready queue and timer
//! heap live in a thread-local; other threads reach it only through its
//! endpoint.

use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, Weak};
use std::time::Instant;

use krio_core::{Task, TaskId};

use super::task::{Body, HostState, ResumeCause, RunState, SwitchHook, TaskRecord};
use super::wait::{Waiter, claim_timeout};
use super::{pool, preempt, trace};
use crate::heap;

/// What another thread may ask of a world.
pub(super) enum WorldCommand {
    Wake(Waiter),
    /// Only a pool sends one, and a target without threads has no pool.
    #[cfg_attr(
        all(target_family = "wasm", not(target_feature = "atomics")),
        allow(dead_code)
    )]
    Spawn {
        id: TaskId,
        stack_size: usize,
        body: Box<dyn FnOnce() + Send + 'static>,
    },
}

/// A world's mailbox. The only part of a world that is `Sync`.
pub(super) struct WorldEndpoint {
    pub(super) commands: Mutex<VecDeque<WorldCommand>>,
    pub(super) changed: Condvar,
    /// Tasks assigned to this world, parked ones included. A krio stack is
    /// `!Send`, so this is read before a task is placed and never after.
    pub(super) assigned: AtomicUsize,
}

impl WorldEndpoint {
    fn new() -> Self {
        Self {
            commands: Mutex::new(VecDeque::new()),
            changed: Condvar::new(),
            assigned: AtomicUsize::new(0),
        }
    }

    pub(super) fn push(&self, command: WorldCommand) {
        self.commands.lock().unwrap().push_back(command);
        preempt::request_poll();
        self.changed.notify_one();
    }
}

static NEXT_WORLD_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);
static WORLD_ENDPOINTS: LazyLock<Mutex<HashMap<u64, Weak<WorldEndpoint>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn endpoint_of(world: u64) -> Option<Arc<WorldEndpoint>> {
    let mut endpoints = WORLD_ENDPOINTS.lock().unwrap();
    let endpoint = endpoints.get(&world).and_then(Weak::upgrade);
    if endpoint.is_none() {
        endpoints.remove(&world);
    }
    endpoint
}

pub(super) struct World {
    id: u64,
    endpoint: Arc<WorldEndpoint>,
    tasks: HashMap<TaskId, TaskRecord>,
    ready: VecDeque<TaskId>,
    /// Earliest deadline first: `(deadline, wait token, task)`.
    timers: BinaryHeap<Reverse<(Instant, u64, TaskId)>>,
    /// Swapped out while a task runs and back in when it yields.
    main_host: Option<Box<dyn HostState>>,
    switch_hook: Option<SwitchHook>,
}

impl World {
    fn new() -> Self {
        static HOOK: std::sync::Once = std::sync::Once::new();
        HOOK.call_once(|| heap::set_poll_request_hook(preempt::request_poll));
        let id = NEXT_WORLD_ID.fetch_add(1, Ordering::Relaxed);
        let endpoint = Arc::new(WorldEndpoint::new());
        WORLD_ENDPOINTS
            .lock()
            .unwrap()
            .insert(id, Arc::downgrade(&endpoint));
        Self {
            id,
            endpoint,
            tasks: HashMap::new(),
            ready: VecDeque::new(),
            timers: BinaryHeap::new(),
            main_host: None,
            switch_hook: None,
        }
    }

    fn enqueue_ready(&mut self, id: TaskId) {
        if !self.ready.contains(&id) {
            self.ready.push_back(id);
        }
    }

    fn next_timer(&self) -> Option<Instant> {
        self.timers
            .peek()
            .map(|Reverse((deadline, _, _))| *deadline)
    }

    fn wake_claimed(&mut self, waiter: Waiter) -> bool {
        if !waiter.task().is_task() {
            // The main context polls its own registration.
            return true;
        }
        let Some(record) = self.tasks.get_mut(&waiter.task()) else {
            return false;
        };
        if record.run_state != RunState::Waiting(waiter.token()) {
            return false;
        }
        record.run_state = RunState::Runnable;
        record.resume_cause = ResumeCause::Notified;
        self.enqueue_ready(waiter.task());
        true
    }

    fn wake_due_timers(&mut self) {
        let now = Instant::now();
        loop {
            let Some(Reverse((deadline, token, id))) = self.timers.peek().copied() else {
                return;
            };
            if deadline > now {
                return;
            }
            self.timers.pop();
            // A waiter notified first leaves a stale entry; the claim fails.
            if !claim_timeout(token) {
                continue;
            }
            let Some(record) = self.tasks.get_mut(&id) else {
                continue;
            };
            if record.run_state != RunState::Waiting(token) {
                continue;
            }
            record.run_state = RunState::Runnable;
            record.resume_cause = ResumeCause::TimedOut;
            self.enqueue_ready(id);
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        WORLD_ENDPOINTS.lock().unwrap().remove(&self.id);
        let queued = self
            .endpoint
            .commands
            .lock()
            .map(|queue| {
                queue
                    .iter()
                    .filter(|command| matches!(command, WorldCommand::Spawn { .. }))
                    .count()
            })
            .unwrap_or(0);
        preempt::tasks_removed(self.tasks.len() + queued);
    }
}

#[derive(Clone, Copy)]
struct ParkRequest {
    waiter: Waiter,
    deadline: Option<Instant>,
}

/// The running task, as seen from its own stack. Kept beside the record
/// rather than in it so the task can borrow the world.
#[derive(Clone, Copy)]
struct ActiveTask {
    id: TaskId,
    resume_cause: ResumeCause,
    pending_park: Option<ParkRequest>,
    gc_blocking_depth: u32,
    can_suspend: bool,
}

thread_local! {
    static WORLD: RefCell<Option<World>> = const { RefCell::new(None) };
    static ACTIVE: Cell<Option<ActiveTask>> = const { Cell::new(None) };
    /// The main context's blocking depth; a task's travels with the task.
    static MAIN_BLOCKING_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// This thread's world, created on first use. Never held across a call
/// into adapter code or a task.
fn with_world<R>(f: impl FnOnce(&mut World) -> R) -> R {
    WORLD.with(|slot| f(slot.borrow_mut().get_or_insert_with(World::new)))
}

fn try_with_world<R>(f: impl FnOnce(&mut World) -> R) -> Option<R> {
    WORLD.with(|slot| slot.borrow_mut().as_mut().map(f))
}

/// Whether this thread owns a world. A thread that does not is foreign: it
/// may wait on a token but never drives tasks.
pub fn has_world() -> bool {
    WORLD.with(|slot| slot.borrow().is_some())
}

/// This thread's world id, creating the world if it has none.
pub fn world_id() -> u64 {
    with_world(|world| world.id)
}

pub(super) fn try_world_id() -> Option<u64> {
    try_with_world(|world| world.id)
}

pub(super) fn endpoint() -> Arc<WorldEndpoint> {
    with_world(|world| Arc::clone(&world.endpoint))
}

/// The running task, or `TaskId::NONE` on the main context.
pub fn current_task() -> TaskId {
    ACTIVE.with(|active| active.get().map_or(TaskId::NONE, |task| task.id))
}

pub fn is_on_task() -> bool {
    ACTIVE.with(|active| active.get().is_some())
}

/// Why the running task was resumed; `Scheduled` off a task.
pub fn resume_cause() -> ResumeCause {
    ACTIVE.with(|active| {
        active
            .get()
            .map_or(ResumeCause::Scheduled, |task| task.resume_cause)
    })
}

/// Tasks alive on this world.
pub fn live_tasks() -> usize {
    with_world(|world| world.tasks.len())
}

fn next_task_id() -> TaskId {
    TaskId(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed))
}

fn install(id: TaskId, body: Body) {
    trace("install", id.0, try_world_id().unwrap_or(0));
    with_world(|world| {
        world.tasks.insert(id, TaskRecord::new(body));
        world.enqueue_ready(id);
    });
}

fn spawn_local(id: TaskId, body: Body) -> TaskId {
    with_world(|world| world.endpoint.assigned.fetch_add(1, Ordering::AcqRel));
    install(id, body);
    id
}

/// A stackful task on this world: a krio fiber with its own stack, which
/// the heap scans while it is suspended. On wasm the body runs straight
/// through in one step and suspends only through the host.
pub fn spawn_fiber(stack_size: usize, body: impl FnOnce() + 'static) -> TaskId {
    let id = next_task_id();
    preempt::task_created();
    trace("create", id.0, 0);
    spawn_local(id, Body::fiber(id, stack_size, Box::new(body)))
}

/// A stackless task on this world: the scheduler calls `step` on its own
/// stack and reads the suspension.
pub fn spawn(task: Box<dyn Task>) -> TaskId {
    let id = next_task_id();
    preempt::task_created();
    trace("create", id.0, 1);
    spawn_local(id, Body::Stackless(task))
}

/// A stackful task on whichever world is least loaded, chosen now and
/// never changed: this world when there is no pool.
pub fn spawn_fiber_on_pool(stack_size: usize, body: impl FnOnce() + Send + 'static) -> TaskId {
    let id = next_task_id();
    preempt::task_created();
    trace("create", id.0, 2);
    let body: Box<dyn FnOnce() + Send + 'static> = Box::new(body);
    match pool::dispatch(id, stack_size, body) {
        Ok(()) => id,
        Err(body) => spawn_local(id, Body::fiber(id, stack_size, body)),
    }
}

/// Attach the state the scheduler swaps around `id`'s turns; `NONE` is the
/// main context. Returns what was attached before.
pub fn attach_host_state(id: TaskId, state: Box<dyn HostState>) -> Option<Box<dyn HostState>> {
    with_world(|world| {
        if id.is_task() {
            world.tasks.get_mut(&id)?.host.replace(state)
        } else {
            world.main_host.replace(state)
        }
    })
}

/// Borrow the host state attached to `id` (`NONE` for the main context).
/// `None` when nothing is attached or the task is gone. Not available from
/// inside a swap.
pub fn with_host_state<R>(id: TaskId, f: impl FnOnce(&mut dyn HostState) -> R) -> Option<R> {
    with_world(|world| {
        let slot = if id.is_task() {
            &mut world.tasks.get_mut(&id)?.host
        } else {
            &mut world.main_host
        };
        slot.as_deref_mut().map(f)
    })
}

/// The hook that observes this world's switches. Set once, by the adapter
/// that owns the world.
pub fn set_switch_hook(hook: SwitchHook) {
    with_world(|world| world.switch_hook = Some(hook));
}

/// The stack pointer last published to the heap for `id`. `None` for a
/// stackless task, an unknown task, or one that has not suspended yet.
pub fn suspended_sp(id: TaskId) -> Option<usize> {
    with_world(|world| world.tasks.get(&id)?.body.as_ref()?.suspended_sp())
}

pub(super) fn set_pending_park(waiter: Waiter, deadline: Option<Instant>) -> bool {
    ACTIVE.with(|active| {
        let Some(mut task) = active.get() else {
            return false;
        };
        debug_assert!(task.pending_park.is_none());
        task.pending_park = Some(ParkRequest { waiter, deadline });
        let can_suspend = task.can_suspend;
        active.set(Some(task));
        can_suspend
    })
}

/// Suspend the running task back to its world's turn.
#[cfg(not(target_family = "wasm"))]
pub(super) fn suspend_current() {
    krio_fiber::yield_now();
}

/// On wasm the suspension is the host's, reached through krio's suspender
/// so library code far from the scheduler suspends the same way. Without
/// one installed a yield returns at once: run-to-completion, not deadlock.
#[cfg(target_family = "wasm")]
pub(super) fn suspend_current() {
    if krio_fiber::has_suspender() {
        krio_fiber::yield_now();
    }
}

fn drain_commands() {
    let endpoint = endpoint();
    let commands: Vec<WorldCommand> = endpoint.commands.lock().unwrap().drain(..).collect();
    for command in commands {
        match command {
            WorldCommand::Wake(waiter) => {
                with_world(|world| world.wake_claimed(waiter));
            }
            WorldCommand::Spawn {
                id,
                stack_size,
                body,
            } => install(id, Body::fiber(id, stack_size, body)),
        }
    }
}

/// Swap a task's host state (`NONE`: the main context's). The object is
/// out of its slot for the call, so the world stays borrowable.
fn swap_host(id: TaskId, swap_in: bool) {
    let taken = with_world(|world| {
        if id.is_task() {
            world.tasks.get_mut(&id)?.host.take()
        } else {
            world.main_host.take()
        }
    });
    let Some(mut host) = taken else {
        return;
    };
    if swap_in {
        host.swap_in();
    } else {
        host.swap_out();
    }
    with_world(|world| {
        let slot = if id.is_task() {
            match world.tasks.get_mut(&id) {
                Some(record) => &mut record.host,
                None => return,
            }
        } else {
            &mut world.main_host
        };
        if slot.is_none() {
            *slot = Some(host);
        }
    });
}

/// One task's turn. `false` if it was not runnable after all.
fn resume_task(id: TaskId) -> bool {
    let Some((mut body, cause, depth, hook)) = with_world(|world| {
        let record = world.tasks.get_mut(&id)?;
        if record.run_state != RunState::Runnable {
            return None;
        }
        let body = record.body.take()?;
        record.run_state = RunState::Running;
        let cause = std::mem::replace(&mut record.resume_cause, ResumeCause::Scheduled);
        Some((body, cause, record.gc_blocking_depth, world.switch_hook))
    }) else {
        return false;
    };
    trace("resume", id.0, cause as u64);

    swap_host(TaskId::NONE, false);
    swap_host(id, true);
    ACTIVE.with(|active| {
        debug_assert!(active.get().is_none());
        active.set(Some(ActiveTask {
            id,
            resume_cause: cause,
            pending_park: None,
            gc_blocking_depth: depth,
            can_suspend: body.can_suspend(),
        }));
    });
    if let Some(hook) = hook {
        hook(TaskId::NONE, id);
    }

    let suspension = body.step(id);

    // Invariant: the suspended stack is published before the hook, which
    // may publish interpreter roots and honour a pending collection.
    body.publish_sp();
    with_world(|world| {
        if let Some(record) = world.tasks.get_mut(&id) {
            record.body = Some(body);
        }
    });
    if let Some(hook) = hook {
        hook(id, TaskId::NONE);
    }
    let active = ACTIVE
        .with(|active| active.replace(None))
        .expect("resumed task lost its active state");
    swap_host(id, false);
    swap_host(TaskId::NONE, true);

    let removed =
        with_world(|world| {
            let record = world.tasks.get_mut(&id)?;
            record.gc_blocking_depth = active.gc_blocking_depth;
            if suspension.is_done() {
                let _ = world.endpoint.assigned.fetch_update(
                    Ordering::AcqRel,
                    Ordering::Acquire,
                    |n| Some(n.saturating_sub(1)),
                );
                return world.tasks.remove(&id);
            }
            // A recorded wait parks the task whatever it returned; without
            // one, `Pending` is a yield, since nothing could wake it.
            match active.pending_park {
                Some(request) => {
                    debug_assert_eq!(request.waiter.task(), id);
                    record.run_state = RunState::Waiting(request.waiter.token());
                    if let Some(deadline) = request.deadline {
                        world
                            .timers
                            .push(Reverse((deadline, request.waiter.token(), id)));
                    }
                }
                None => {
                    record.run_state = RunState::Runnable;
                    world.enqueue_ready(id);
                }
            }
            None
        });
    if let Some(record) = removed {
        trace("remove", id.0, 0);
        preempt::task_removed();
        // Dropped with no borrow held: a fiber unregisters its stack here.
        drop(record);
    }
    true
}

/// One turn: resume each task that was ready when the turn began. Tasks
/// parked on a token or a timer consume no switch. Returns whether any
/// task was resumed. From a task this does nothing: a task yields.
pub fn schedule_step() -> bool {
    if is_on_task() {
        return false;
    }
    drain_commands();
    with_world(|world| world.wake_due_timers());
    let mut resumed = false;
    let turns = with_world(|world| world.ready.len());
    for _ in 0..turns {
        let Some(id) = with_world(|world| world.ready.pop_front()) else {
            break;
        };
        if resume_task(id) {
            resumed = true;
        }
        with_world(|world| world.wake_due_timers());
    }
    resumed
}

/// Run turns until no task is ready or the deadline passes; what a driver
/// calls per frame. Returns whether this world still holds live tasks. From
/// a task it yields once instead.
pub fn tick(deadline: Option<Instant>) -> bool {
    if is_on_task() {
        yield_now();
    } else {
        loop {
            heap::gc_safepoint();
            if !schedule_step() {
                break;
            }
            if deadline.is_some_and(|limit| Instant::now() >= limit) {
                break;
            }
        }
    }
    live_tasks() != 0
}

/// Block until a command arrives, the next timer is due or `deadline`
/// passes. The main context's wait when nothing is ready; the reactor's
/// seam.
#[cfg(any(not(target_family = "wasm"), target_feature = "atomics"))]
pub fn scheduler_idle(deadline: Option<Instant>) {
    // Still a registered mutator: rendezvous with a collection another
    // world asked for before sleeping.
    heap::gc_safepoint();
    let (endpoint, next_timer) =
        with_world(|world| (Arc::clone(&world.endpoint), world.next_timer()));
    let wake_at = match (deadline, next_timer) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    // Peeked before announcing a blocking section, which costs a world-lock
    // round trip each way; and never announced while holding the queue,
    // since the announcement can park this thread until a collection ends
    // and a waker would then block on the lock without reaching a safepoint.
    if !endpoint.commands.lock().unwrap().is_empty() {
        return;
    }
    // TODO(reactor): sockets, file watches and cross-world channels register
    // here and wake `changed`; until then only commands and timers do.
    heap::mark_site(heap::SITE_SCHEDULER_IDLE);
    heap::gc_set_blocking(true);
    {
        // Re-checked under the lock: the peek raced with anyone pushing.
        let queue = endpoint.commands.lock().unwrap();
        if queue.is_empty() {
            match wake_at {
                Some(at) => {
                    let wait = at.saturating_duration_since(Instant::now());
                    let _ = endpoint.changed.wait_timeout(queue, wait).unwrap();
                }
                None => drop(endpoint.changed.wait(queue).unwrap()),
            }
        }
    }
    heap::gc_set_blocking(false);
    heap::mark_site(heap::SITE_RUNNING);
}

/// Without threads nothing can push a command while this thread sleeps, so
/// the wait is a short nap bounded by the next deadline.
#[cfg(all(target_family = "wasm", not(target_feature = "atomics")))]
pub fn scheduler_idle(deadline: Option<Instant>) {
    heap::gc_safepoint();
    let next_timer = with_world(|world| world.next_timer());
    let wake_at = match (deadline, next_timer) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    // TODO(reactor): the host's event source replaces this nap.
    let nap = std::time::Duration::from_millis(1);
    let wait = wake_at.map_or(nap, |at| {
        at.saturating_duration_since(Instant::now()).min(nap)
    });
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
}

/// Give up the rest of this turn. On a task that can suspend, back to the
/// world; on the main context, one turn for the others; on a native
/// stackless task, nothing: it suspends by returning from its step.
pub fn yield_now() {
    let can_suspend = ACTIVE.with(|active| active.get().map(|task| task.can_suspend));
    match can_suspend {
        Some(true) => suspend_current(),
        Some(false) => {}
        None => {
            if has_world() {
                schedule_step();
            }
        }
    }
}

/// The scheduler's safepoint: rendezvous with the collector, then let the
/// others run. A safepoint on a task yields it; one on the main context
/// advances each ready task once.
pub fn poll() {
    heap::gc_safepoint();
    if !preempt::any_live_tasks() {
        return;
    }
    if is_on_task() {
        yield_now();
    } else if has_world() {
        schedule_step();
    }
}

/// "I am blocked": on a task, yield; on the main context, run the others
/// and pace. Paced after every pass because a resumed-but-still-blocked
/// task yields instantly and counts as progress.
pub fn block_yield() {
    if is_on_task() {
        yield_now();
    } else {
        heap::gc_safepoint();
        if has_world() {
            schedule_step();
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn update_blocking_depth(depth: &mut u32, blocking: bool) -> bool {
    if blocking {
        *depth = depth.saturating_add(1);
        true
    } else if *depth == 0 {
        false
    } else {
        *depth -= 1;
        true
    }
}

fn set_blocking(blocking: bool) -> bool {
    let changed = ACTIVE.with(|active| match active.get() {
        Some(mut task) => {
            let changed = update_blocking_depth(&mut task.gc_blocking_depth, blocking);
            active.set(Some(task));
            changed
        }
        None => MAIN_BLOCKING_DEPTH.with(|depth| {
            let mut value = depth.get();
            let changed = update_blocking_depth(&mut value, blocking);
            depth.set(value);
            changed
        }),
    });
    if changed {
        heap::gc_set_blocking(blocking);
    }
    changed
}

/// Enter a native section that may block without polling. Counted per
/// task, and per thread for the main context, so nested sections balance.
pub fn enter_blocking() -> bool {
    set_blocking(true)
}

/// Leave the innermost blocking section. `false` if none was open.
pub fn leave_blocking() -> bool {
    set_blocking(false)
}

pub fn is_blocking() -> bool {
    ACTIVE.with(|active| match active.get() {
        Some(task) => task.gc_blocking_depth != 0,
        None => MAIN_BLOCKING_DEPTH.with(|depth| depth.get() != 0),
    })
}
