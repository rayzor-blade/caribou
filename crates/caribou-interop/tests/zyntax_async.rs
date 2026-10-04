//! An async Zyntax function, called from Wren, returns a `caribou.Future`
//! at once and runs on a task of the world: calls made together wait
//! together, and awaiting the future gives the function's result.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::registry::{self, Namespace, TypeRef};
use caribou::world::{Config, World};
use caribou_zyntax::Frontend;
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::vm::{VM, VMConfig};

const USE: &str = r#"
import "game:later" for doubled, half, outer
var start = System.clock
var a = doubled.call(21)
var b = doubled.call(4)
System.print(a.ready())
System.print(a.await())
System.print(b.await())
// Two 250 ms waits, overlapped: well under the 500 ms of one after the other.
System.print(System.clock - start < 0.45)
System.print(half.call().await())
System.print(outer.call().await())
"#;

#[test]
fn an_async_zynml_function_returns_a_future_from_wren() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/src");
    let frontend = Frontend::snapshot(zynml::snapshot_bytes()).expect("the snapshot loads");
    let world = World::new(Config {
        namespaces: vec![Namespace {
            name: "game".to_owned(),
            langs: vec!["wren".to_owned(), "zynml".to_owned()],
            modules: None,
        }],
        roots: vec![root],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_wren::Runtime::new()))
        .expect("wren registers");
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![frontend])))
        .expect("zyntax registers");

    let errors = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&errors);
    let mut config = VMConfig {
        execution_mode: ExecutionMode::Interpreter,
        error_fn: Some(Box::new(move |_kind, _module, _line, message: &str| {
            sink.borrow_mut().push(message.to_owned());
        })),
        ..VMConfig::default()
    };
    caribou_wren::import::configure(&mut config);
    let mut vm = VM::new(config);
    vm.krio_fiber_active = true;
    vm.output_buffer = Some(String::new());
    let result = caribou_wren::with_vm(&mut vm, |vm| vm.interpret("use", USE));
    let output = vm.take_output();
    assert_eq!(
        result,
        InterpretResult::Success,
        "{:?} {output:?}",
        errors.borrow()
    );
    assert_eq!(output, "false\n42\n8\ntrue\n2.5\n21\n");

    // The interface types the result as the future it is.
    let iface = registry::lookup("game", "later").expect("the module is published");
    let doubled = iface
        .functions
        .iter()
        .find(|f| f.name == "doubled")
        .expect("doubled is published");
    assert_eq!(doubled.ret, TypeRef::Future(Box::new(TypeRef::Int)));
}
