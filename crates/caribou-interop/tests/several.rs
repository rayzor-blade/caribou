//! Several values at once between Zyntax languages and the others: a Lua
//! function's two declared results are a tuple Python unpacks, and a
//! Python tuple is a list to Wren.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::registry::Namespace;
use caribou::world::{Config, World};
use caribou_zyntax::Frontend;
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::vm::{VM, VMConfig};

const USE: &str = r#"
import "game:split" for split, counted
System.print(split.call(17, 5))
System.print(counted.call())
"#;

#[test]
fn several_values_cross_between_lua_python_and_wren() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/several/src");
    let world = World::new(Config {
        namespaces: vec![Namespace {
            name: "game".to_owned(),
            langs: vec!["wren".to_owned(), "lua".to_owned(), "python".to_owned()],
            modules: None,
        }],
        roots: vec![root],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_wren::Runtime::new()))
        .expect("wren registers");
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![
            Frontend::new(Box::new(caribou_lua::Lua::new())),
            Frontend::new(Box::new(caribou_python::Python::new())),
        ])))
        .expect("lua and python register");

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
    assert_eq!(output, "302\n3\n");
}
