//! Scheduler behaviour through the public API. Each test runs on its own
//! thread and so on its own world.

#![cfg(not(target_family = "wasm"))]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use caribou::sched::{
    DEFAULT_STACK_SIZE, HostState, ResumeCause, Suspension, Task, TaskId, Waiter,
    attach_host_state, current_task, has_worker_pool, is_pool_worker, live_tasks, new_waiter, park,
    request_park, resume_cause, scheduler_idle, set_switch_hook, sleep_until, spawn, spawn_fiber,
    spawn_fiber_on_pool, suspended_sp, tick, wake, world_id, yield_now,
};

type Log<T> = Rc<RefCell<Vec<T>>>;

fn log<T>() -> Log<T> {
    Rc::new(RefCell::new(Vec::new()))
}

/// The driver shape: turns while anything is ready, idle until the next
/// command or timer otherwise.
fn run_to_completion() {
    while tick(None) {
        scheduler_idle(None);
    }
}

#[test]
fn stackful_tasks_round_robin() {
    let order: Log<(u8, u8)> = log();
    for task in 0..3u8 {
        let order = Rc::clone(&order);
        spawn_fiber(DEFAULT_STACK_SIZE, move || {
            order.borrow_mut().push((task, 0));
            yield_now();
            order.borrow_mut().push((task, 1));
            yield_now();
            order.borrow_mut().push((task, 2));
        });
    }
    assert_eq!(live_tasks(), 3);
    run_to_completion();
    assert_eq!(live_tasks(), 0);
    assert_eq!(
        *order.borrow(),
        [
            (0, 0),
            (1, 0),
            (2, 0),
            (0, 1),
            (1, 1),
            (2, 1),
            (0, 2),
            (1, 2),
            (2, 2)
        ]
    );
}

struct Counter {
    steps: u32,
    order: Log<&'static str>,
}

impl Task for Counter {
    fn step(&mut self) -> Suspension {
        self.steps += 1;
        self.order
            .borrow_mut()
            .push(["s0", "s1", "s2"][self.steps as usize - 1]);
        if self.steps < 3 {
            Suspension::Yielded
        } else {
            Suspension::Completed
        }
    }
}

#[test]
fn stackless_task_interleaves_with_stackful() {
    let order: Log<&'static str> = log();
    spawn(Box::new(Counter {
        steps: 0,
        order: Rc::clone(&order),
    }));
    let fiber_order = Rc::clone(&order);
    spawn_fiber(DEFAULT_STACK_SIZE, move || {
        fiber_order.borrow_mut().push("f0");
        yield_now();
        fiber_order.borrow_mut().push("f1");
        yield_now();
        fiber_order.borrow_mut().push("f2");
    });
    run_to_completion();
    assert_eq!(*order.borrow(), ["s0", "f0", "s1", "f1", "s2", "f2"]);
    assert_eq!(live_tasks(), 0);
}

#[test]
fn park_then_wake_from_another_task() {
    let token = Rc::new(Cell::new(None));
    let notified = Rc::new(Cell::new(None));
    let order: Log<&'static str> = log();

    let (token_a, notified_a, order_a) =
        (Rc::clone(&token), Rc::clone(&notified), Rc::clone(&order));
    spawn_fiber(DEFAULT_STACK_SIZE, move || {
        let waiter = new_waiter();
        token_a.set(Some(waiter));
        order_a.borrow_mut().push("a-park");
        notified_a.set(Some(park(waiter, None)));
        order_a.borrow_mut().push("a-resumed");
    });
    let (token_b, order_b) = (Rc::clone(&token), Rc::clone(&order));
    spawn_fiber(DEFAULT_STACK_SIZE, move || {
        order_b.borrow_mut().push("b-yield");
        yield_now();
        order_b.borrow_mut().push("b-wake");
        assert!(wake(token_b.get().expect("A registered its waiter first")));
        order_b.borrow_mut().push("b-done");
    });

    run_to_completion();
    assert_eq!(notified.get(), Some(true));
    assert_eq!(
        *order.borrow(),
        ["a-park", "b-yield", "b-wake", "b-done", "a-resumed"]
    );
}

#[test]
fn park_with_deadline_and_no_wake_times_out() {
    let outcome = Rc::new(Cell::new(None));
    let started = Instant::now();
    let deadline = started + Duration::from_millis(20);
    let outcome_task = Rc::clone(&outcome);
    spawn_fiber(DEFAULT_STACK_SIZE, move || {
        let waiter = new_waiter();
        let notified = park(waiter, Some(deadline));
        outcome_task.set(Some((notified, Instant::now())));
    });
    run_to_completion();
    let (notified, resumed_at) = outcome.get().expect("task completed");
    assert!(!notified);
    assert!(resumed_at >= deadline);
}

#[test]
fn sleep_until_does_not_spin() {
    let deadline = Instant::now() + Duration::from_millis(20);
    let sleeper = Rc::new(Cell::new(None));
    let yields_done_at = Rc::new(Cell::new(None));

    let sleeper_task = Rc::clone(&sleeper);
    spawn_fiber(DEFAULT_STACK_SIZE, move || {
        sleep_until(deadline);
        sleeper_task.set(Some(Instant::now()));
    });
    let done_at = Rc::clone(&yields_done_at);
    spawn_fiber(DEFAULT_STACK_SIZE, move || {
        for _ in 0..1000 {
            yield_now();
        }
        done_at.set(Some(Instant::now()));
    });

    run_to_completion();
    let resumed_at = sleeper.get().expect("sleeper completed");
    let yields_done_at = yields_done_at.get().expect("yielder completed");
    assert!(resumed_at >= deadline);
    // A parked task consumes no switch, so the yielder never waited on it.
    assert!(yields_done_at < resumed_at);
}

thread_local! {
    static EVENTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn event(text: String) {
    EVENTS.with(|events| events.borrow_mut().push(text));
}

struct Recorder(&'static str);

impl HostState for Recorder {
    fn swap_in(&mut self) {
        event(format!("{}:in", self.0));
    }

    fn swap_out(&mut self) {
        event(format!("{}:out", self.0));
    }
}

fn recording_hook(from: TaskId, to: TaskId) {
    // Published before the hook: a task that just yielded has its stack
    // pointer visible here.
    let published = if from.is_task() {
        suspended_sp(from).is_some()
    } else {
        false
    };
    event(format!(
        "hook:{}->{}{}",
        from.0,
        to.0,
        if published { "(sp)" } else { "" }
    ));
}

#[test]
fn host_state_and_hook_order_per_resume() {
    set_switch_hook(recording_hook);
    attach_host_state(TaskId::NONE, Box::new(Recorder("main")));
    let id = spawn_fiber(DEFAULT_STACK_SIZE, || {
        event("run:1".into());
        yield_now();
        event("run:2".into());
    });
    assert!(attach_host_state(id, Box::new(Recorder("task"))).is_none());
    assert!(suspended_sp(id).is_none());

    run_to_completion();

    let t = id.0;
    let expected = [
        "main:out",
        "task:in",
        &format!("hook:0->{t}"),
        "run:1",
        &format!("hook:{t}->0(sp)"),
        "task:out",
        "main:in",
        "main:out",
        "task:in",
        &format!("hook:0->{t}"),
        "run:2",
        &format!("hook:{t}->0(sp)"),
        "task:out",
        "main:in",
    ];
    let events = EVENTS.with(|events| events.borrow().clone());
    assert_eq!(events, expected);
}

#[test]
fn current_task_is_none_on_main_and_set_on_task() {
    assert_eq!(current_task(), TaskId::NONE);
    let seen = Rc::new(Cell::new(TaskId::NONE));
    let seen_task = Rc::clone(&seen);
    let id = spawn_fiber(DEFAULT_STACK_SIZE, move || seen_task.set(current_task()));
    run_to_completion();
    assert_eq!(seen.get(), id);
    assert_eq!(current_task(), TaskId::NONE);
}

#[test]
#[cfg_attr(all(target_family = "wasm", not(target_feature = "atomics")), ignore = "needs threads")]
fn wake_from_another_thread_reaches_an_idle_world() {
    let waiter_slot = Arc::new(Mutex::new(None));
    let notified = Rc::new(Cell::new(None));
    let slot = Arc::clone(&waiter_slot);
    let notified_task = Rc::clone(&notified);
    spawn_fiber(DEFAULT_STACK_SIZE, move || {
        let waiter = new_waiter();
        *slot.lock().unwrap() = Some(waiter);
        notified_task.set(Some(park(
            waiter,
            Some(Instant::now() + Duration::from_secs(5)),
        )));
    });
    // Park the task first, so the wake arrives while the world is idle.
    tick(None);
    let waker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        let waiter = waiter_slot.lock().unwrap().expect("task parked");
        wake(waiter)
    });
    run_to_completion();
    assert!(waker.join().unwrap());
    assert_eq!(notified.get(), Some(true));
}

#[test]
#[cfg_attr(all(target_family = "wasm", not(target_feature = "atomics")), ignore = "needs threads")]
fn main_context_parks_and_is_woken_by_a_thread() {
    // The main context of a world drives turns while it waits; without a
    // world it would poll like a foreign thread.
    world_id();
    let waiter = new_waiter();
    let waker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        wake(waiter)
    });
    assert!(park(waiter, Some(Instant::now() + Duration::from_secs(5))));
    assert!(waker.join().unwrap());
}

#[test]
#[cfg_attr(all(target_family = "wasm", not(target_feature = "atomics")), ignore = "needs threads")]
fn foreign_thread_polls_its_token() {
    let woken = Arc::new(AtomicBool::new(false));
    let timed_out = std::thread::spawn(|| {
        let waiter = new_waiter();
        park(waiter, Some(Instant::now() + Duration::from_millis(10)))
    });
    assert!(!timed_out.join().unwrap());

    let waiter_slot = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&waiter_slot);
    let woken_flag = Arc::clone(&woken);
    let foreign = std::thread::spawn(move || {
        let waiter = new_waiter();
        *slot.lock().unwrap() = Some(waiter);
        let notified = park(waiter, Some(Instant::now() + Duration::from_secs(5)));
        woken_flag.store(notified, Ordering::Release);
    });
    let waiter = loop {
        if let Some(waiter) = *waiter_slot.lock().unwrap() {
            break waiter;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(wake(waiter));
    foreign.join().unwrap();
    assert!(woken.load(Ordering::Acquire));
}

#[test]
#[cfg_attr(all(target_family = "wasm", not(target_feature = "atomics")), ignore = "needs threads")]
fn pooled_task_runs_and_wakes_the_spawner() {
    world_id();
    let waiter = new_waiter();
    let ran_on = Arc::new(Mutex::new(None));
    let ran_on_task = Arc::clone(&ran_on);
    spawn_fiber_on_pool(DEFAULT_STACK_SIZE, move || {
        *ran_on_task.lock().unwrap() = Some(std::thread::current().id());
        assert!(wake(waiter));
    });
    // Notified whether the pool took the task or this world ran it.
    assert!(park(waiter, Some(Instant::now() + Duration::from_secs(5))));
    assert!(ran_on.lock().unwrap().is_some());
}

#[test]
#[cfg_attr(all(target_family = "wasm", not(target_feature = "atomics")), ignore = "needs threads")]
fn pool_workers_know_themselves() {
    assert!(!is_pool_worker());
    world_id();
    let waiter = new_waiter();
    let seen = Arc::new(Mutex::new(None));
    let seen_task = Arc::clone(&seen);
    spawn_fiber_on_pool(DEFAULT_STACK_SIZE, move || {
        *seen_task.lock().unwrap() = Some((std::thread::current().id(), is_pool_worker()));
        assert!(wake(waiter));
    });
    assert!(park(waiter, Some(Instant::now() + Duration::from_secs(5))));
    let (ran_on, on_worker) = seen.lock().unwrap().expect("task ran");
    // A pooled task is on a worker exactly when it left this thread.
    assert_eq!(on_worker, ran_on != std::thread::current().id());
    if !has_worker_pool() {
        assert!(!on_worker);
    }
    assert!(!is_pool_worker());
}

#[test]
fn a_task_can_spawn_and_the_child_runs_next_turn() {
    let order: Log<&'static str> = log();
    let parent_order = Rc::clone(&order);
    spawn_fiber(DEFAULT_STACK_SIZE, move || {
        parent_order.borrow_mut().push("parent:spawn");
        let child_order = Rc::clone(&parent_order);
        spawn_fiber(DEFAULT_STACK_SIZE, move || {
            child_order.borrow_mut().push("child");
        });
        parent_order.borrow_mut().push("parent:yield");
        yield_now();
        parent_order.borrow_mut().push("parent:done");
    });
    run_to_completion();
    assert_eq!(
        *order.borrow(),
        ["parent:spawn", "parent:yield", "child", "parent:done"]
    );
}

/// A stackless wait: record the park, return `Pending`, read the cause on
/// the next step.
struct StacklessSleeper {
    deadline: Instant,
    waiter: Option<Waiter>,
    outcome: Rc<Cell<Option<(ResumeCause, Instant)>>>,
}

impl Task for StacklessSleeper {
    fn step(&mut self) -> Suspension {
        match self.waiter {
            None => {
                let waiter = new_waiter();
                self.waiter = Some(waiter);
                assert!(!request_park(waiter, Some(self.deadline)));
                Suspension::Pending
            }
            Some(_) => {
                self.outcome.set(Some((resume_cause(), Instant::now())));
                Suspension::Completed
            }
        }
    }
}

#[test]
fn stackless_task_parks_through_request_park() {
    let deadline = Instant::now() + Duration::from_millis(20);
    let outcome = Rc::new(Cell::new(None));
    spawn(Box::new(StacklessSleeper {
        deadline,
        waiter: None,
        outcome: Rc::clone(&outcome),
    }));
    let steps = Rc::new(Cell::new(0u32));
    let steps_task = Rc::clone(&steps);
    spawn_fiber(DEFAULT_STACK_SIZE, move || {
        for _ in 0..100 {
            steps_task.set(steps_task.get() + 1);
            yield_now();
        }
    });
    run_to_completion();
    let (cause, resumed_at) = outcome.get().expect("sleeper completed");
    assert_eq!(cause, ResumeCause::TimedOut);
    assert!(resumed_at >= deadline);
    assert_eq!(steps.get(), 100);
}
