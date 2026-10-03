//! Each stack runs Zyntax code in its own segment of the thread's handler
//! stack: a frame one fiber leaves open across a yield is not in scope
//! for another fiber on the same thread, and is there again when the
//! first resumes.

use std::cell::RefCell;
use std::rc::Rc;

use caribou::sched;
use caribou::world::{Config, World};
use caribou_zyntax::Frontend;
use caribou_zyntax::zyntax_embed::{
    __zyntax_effect_pop_handler, __zyntax_effect_push_handler, handler_stack_depth,
};

#[test]
fn a_frame_open_across_a_yield_stays_with_its_stack() {
    let frontend = Frontend::snapshot(zynml::snapshot_bytes()).expect("the snapshot loads");
    let world = World::new(Config::default());
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![frontend])))
        .expect("zyntax registers");

    let base = handler_stack_depth();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&seen);
    sched::spawn_fiber(sched::DEFAULT_STACK_SIZE, move || {
        let frame = __zyntax_effect_push_handler(1, std::ptr::null_mut(), std::ptr::null_mut(), 0);
        log.borrow_mut().push(("a open", handler_stack_depth()));
        sched::yield_now();
        log.borrow_mut().push(("a resumed", handler_stack_depth()));
        __zyntax_effect_pop_handler(frame);
    });
    let log = Rc::clone(&seen);
    sched::spawn_fiber(sched::DEFAULT_STACK_SIZE, move || {
        log.borrow_mut().push(("b", handler_stack_depth()));
    });
    while sched::live_tasks() > 0 {
        sched::schedule_step();
    }

    assert_eq!(
        *seen.borrow(),
        [("a open", base + 1), ("b", base), ("a resumed", base + 1)]
    );
    assert_eq!(
        handler_stack_depth(),
        base,
        "the main stack sees none of them"
    );
}
