//! A compiled Wren loop that never allocates still comes to the core's
//! stop: the collector's stop hook asks wren_lift for a safepoint on
//! every thread running Wren, its compiled loops poll wren_lift's page
//! at their headers, and the collection runs while the loop is still
//! going, in far less than the rendezvous deadline after which a stop is
//! abandoned.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use caribou::heap;
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

/// A thread spinning on numbers alone for longer than the test needs,
/// in a method the tier compiles while it runs.
const SPIN: &str = "\
import \"thread\" for Thread
class Spin {
  static go(n) {
    var i = 0
    while (i < n) i = i + 1
    return i
  }
}
var t = Thread.create { Spin.go(800000000) }
";

#[test]
fn a_compiled_loop_that_never_allocates_comes_to_a_stop() {
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");
    let errors = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&errors);
    let mut config = VMConfig {
        execution_mode: ExecutionMode::Tiered,
        gc_strategy: GcStrategy::Immix,
        error_fn: Some(Box::new(move |_kind, _module, _line, message: &str| {
            sink.borrow_mut().push(message.to_owned());
        })),
        ..VMConfig::default()
    };
    caribou_wren::import::configure(&mut config);
    let mut vm = VM::new(config);
    vm.krio_fiber_active = true;

    let started = Instant::now();
    assert_eq!(
        caribou_wren::with_vm(&mut vm, |vm| vm.interpret("spin", SPIN)),
        InterpretResult::Success,
        "{:?}",
        errors.borrow()
    );
    // The loop is under way on its thread; three collections from here.
    std::thread::sleep(Duration::from_millis(50));
    for _ in 0..3 {
        let began = Instant::now();
        heap::major();
        let took = began.elapsed();
        assert!(
            took < Duration::from_millis(500),
            "a collection took {took:?} with the loop running"
        );
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the loop ended before the collections ran"
    );
    assert_eq!(
        caribou_wren::with_vm(&mut vm, |vm| vm
            .interpret("join", "import \"spin\" for t\nt.join()\n")),
        InterpretResult::Success,
        "{:?}",
        errors.borrow()
    );
    drop(vm);
    assert!(errors.take().is_empty());
}
