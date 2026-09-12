//! The poll epoch, in its own binary: the assertion that it stops moving
//! needs no other test's tasks alive in the process.

#![cfg(not(target_family = "wasm"))]

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use caribou::sched::{
    DEFAULT_STACK_SIZE, POLL_EPOCH, any_live_tasks, live_tasks, spawn_fiber, tick, yield_now,
};

#[test]
fn poll_epoch_moves_only_while_tasks_exist() {
    let stop = Rc::new(Cell::new(false));
    for _ in 0..2 {
        let stop = Rc::clone(&stop);
        spawn_fiber(DEFAULT_STACK_SIZE, move || {
            while !stop.get() {
                yield_now();
            }
        });
    }
    assert!(any_live_tasks());
    let before = POLL_EPOCH.load(Ordering::Acquire);
    tick(Some(Instant::now() + Duration::from_millis(30)));
    let during = POLL_EPOCH.load(Ordering::Acquire);
    assert!(
        during > before,
        "the timer bumps the epoch while tasks exist"
    );

    stop.set(true);
    while tick(None) {}
    assert_eq!(live_tasks(), 0);
    assert!(!any_live_tasks());
    // The timer may complete one quantum after the last task goes.
    std::thread::sleep(Duration::from_millis(10));
    let settled = POLL_EPOCH.load(Ordering::Acquire);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(POLL_EPOCH.load(Ordering::Acquire), settled);
}
