//! wren_lift's threads and isolates on the core heap: tasks of a worker
//! pool allocating on one heap record from several OS threads, with the
//! core's collections and wren_lift's cycles stopping each other's
//! threads through the seam, and isolates each minting a heap of their
//! own on their thread. One test, since the seam installs once per
//! process.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

/// Eight tasks, each allocating enough to run cycles while the others
/// do, summing through a `Deque`.
const THREADS: &str = r#"
import "thread" for Thread, Lock, Deque

class Fib {
  static of(n) {
    if (n < 2) return n
    return of(n - 1) + of(n - 2)
  }
}

var results = Deque.new()
var done = Lock.new()
for (i in 0...8) {
  Thread.create {
    var xs = []
    for (k in 0...20000) xs.add([k, "%(k)"])
    results.add(Fib.of(22) + xs.count)
    done.release()
  }
}
for (i in 0...8) done.wait()
var total = 0
while (results.count > 0) total = total + results.pop(false)
System.print(total)
"#;

const WORKER: &str = r#"
import "isolate" for Isolate
var arg = Isolate.arg
var sum = 0
var xs = []
for (i in 0...arg["n"]) {
  sum = sum + i
  xs.add("%(i)")
}
arg["reply"].send({"who": arg["who"], "sum": sum, "n": xs.count})
"#;

/// Four isolates, the main thread parked on the channel meanwhile.
const ISOLATES: &str = r#"
import "isolate" for Isolate, Channel
var reply = Channel.new()
var workers = []
for (k in 0...4) {
  workers.add(Isolate.spawn("worker", {"who": k, "n": 200000, "reply": reply}))
}
var total = 0
for (k in 0...4) {
  var r = reply.receive()
  total = total + r["sum"]
}
for (w in workers) w.join()
System.print(total)
"#;

fn config(errors: &Rc<RefCell<Vec<String>>>, mode: ExecutionMode) -> VMConfig {
    let sink = Rc::clone(errors);
    let mut config = VMConfig {
        execution_mode: mode,
        gc_strategy: GcStrategy::Immix,
        error_fn: Some(Box::new(move |_kind, _module, _line, message: &str| {
            sink.borrow_mut().push(message.to_owned());
        })),
        load_module_fn: Some(Box::new(|name: &str, _from: &str| {
            (name == "worker").then(|| WORKER.to_owned())
        })),
        ..VMConfig::default()
    };
    caribou_wren::import::configure(&mut config);
    config
}

fn run(mode: ExecutionMode, source: &str) -> String {
    let errors = Rc::new(RefCell::new(Vec::new()));
    let mut vm = VM::new(config(&errors, mode));
    vm.krio_fiber_active = true;
    vm.output_buffer = Some(String::new());
    // An isolate's VM is made on the isolate's thread, from the factory.
    vm.isolate_factory = Some(Arc::new(move || {
        let errors = Rc::new(RefCell::new(Vec::new()));
        let mut vm = VM::new(config(&errors, mode));
        vm.krio_fiber_active = true;
        vm
    }));
    let result = caribou_wren::with_vm(&mut vm, |vm| vm.interpret("main", source));
    let output = vm.take_output();
    assert_eq!(result, InterpretResult::Success, "{:?}", errors.borrow());
    output.trim().to_owned()
}

#[test]
fn threads_and_isolates_run_on_the_core_heap() {
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");
    for mode in [ExecutionMode::Interpreter, ExecutionMode::Tiered] {
        for _ in 0..3 {
            assert_eq!(run(mode, THREADS), "301688");
        }
        assert_eq!(run(mode, ISOLATES), "79999600000");
    }
}
