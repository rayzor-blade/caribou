//! The bridge's pending slot across scheduler tasks. Each test runs on its
//! own thread and so on its own world.

#![cfg(not(target_family = "wasm"))]

use std::cell::RefCell;
use std::rc::Rc;

use caribou::abi::{ErrorKind, Value};
use caribou::bridge::{has_pending, set_pending, take_pending};
use caribou::error::{Error, Str};
use caribou::sched::{DEFAULT_STACK_SIZE, scheduler_idle, spawn_fiber, tick, yield_now};

fn run_to_completion() {
    while tick(None) {
        scheduler_idle(None);
    }
}

#[test]
fn each_task_has_its_own_pending_slot() {
    let seen: Rc<RefCell<Vec<(u32, &'static str)>>> = Rc::new(RefCell::new(Vec::new()));
    for task in 0..2u32 {
        let seen = Rc::clone(&seen);
        spawn_fiber(DEFAULT_STACK_SIZE, move || {
            let message = if task == 0 { "first" } else { "second" };
            let e = Error::new(ErrorKind::User, message, 1);
            set_pending(Error::value(e));
            assert!(has_pending());
            // The other task sets its own slot before this one resumes.
            yield_now();
            let taken = take_pending().expect("this task's error is still pending");
            let text = unsafe { Error::message_str(Error::from_value(taken).unwrap()) };
            seen.borrow_mut().push((task, text));
            assert!(!has_pending());
            yield_now();
            assert!(
                !has_pending(),
                "the other task's take did not touch this slot"
            );
        });
    }
    assert!(!has_pending(), "the main context has no pending error");
    run_to_completion();
    assert_eq!(*seen.borrow(), [(0, "first"), (1, "second")]);
    assert!(!has_pending());
    assert_eq!(take_pending(), None);
}

#[test]
fn a_pending_value_survives_a_collection_between_set_and_take() {
    let s = Str::new("kept by the slot");
    set_pending(Str::value(s));
    caribou::heap::major();
    let taken = take_pending().unwrap();
    assert_eq!(unsafe { Str::text(taken) }, Some("kept by the slot"));
    assert_eq!(take_pending(), None);
    // A non-object value pends and comes back unchanged.
    set_pending(Value::int(42));
    assert_eq!(take_pending(), Some(Value::int(42)));
}
