//! A Python module is a module of the world through Zyntax's Python
//! frontend: no grammar file, the frontend parses on its own. Its
//! functions are the module's own, as in Python, so Wren imports them
//! as module variables and calls each as it calls a `Fn`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::registry::{self, Namespace, TypeRef};
use caribou::world::{Config, World};
use caribou_zyntax::Frontend;
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

const USE: &str = r#"
import "game:tally" for score, weight, perfect, echo
System.print(score.call(7, 2))
System.print(weight.call(3, 1.5))
System.print(perfect.call(5, 5))
System.print(perfect.call(4, 5))
System.print(echo.call("goal"))
System.print(score.arity)
System.print(Fiber.new { score.call("seven", 2) }.try())
"#;

#[test]
fn a_python_module_is_imported_from_wren() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/src");
    assert!(caribou_python::Python::present_in(&[&root]));

    let world = World::new(Config {
        namespaces: vec![Namespace {
            name: "game".to_owned(),
            langs: vec!["wren".to_owned(), "python".to_owned()],
            modules: None,
        }],
        roots: vec![root],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_wren::Runtime::new()))
        .expect("wren registers");
    let frontend = Frontend::new(Box::new(caribou_python::Python::new()));
    let langs = world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![frontend])))
        .expect("python registers");
    assert_eq!(caribou::world::language_name(langs[0]), "python");

    let errors = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&errors);
    let mut config = VMConfig {
        execution_mode: ExecutionMode::Interpreter,
        gc_strategy: GcStrategy::Immix,
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
    assert_eq!(result, InterpretResult::Success, "{:?} {output:?}", errors.borrow());
    assert_eq!(
        output,
        "64\n5\ntrue\nfalse\ngoal\n2\nargument 1 of the function cannot be a caribou.Str\n"
    );

    // Published as the module's own functions, typed from the
    // annotations, and its one class with the methods Python exports;
    // the frontend's prelude declares nothing of the module's.
    let iface = registry::lookup("game", "tally").expect("published");
    let names: Vec<&str> = iface.functions.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["score", "weight", "perfect", "echo"]);
    let echo = iface.functions.iter().find(|m| m.name == "echo").expect("echo");
    assert_eq!((echo.params.clone(), echo.ret.clone()), (vec![TypeRef::Str], TypeRef::Str));
    let names: Vec<&str> = iface.classes.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Tally"]);
    let tally = &iface.classes[0];
    assert_eq!(tally.type_name, "python.Tally");
    assert!(tally.ctor.is_some(), "new is the constructor");
    let methods: Vec<(&str, bool)> = tally.methods.iter().map(|m| (m.name.as_str(), m.is_static)).collect();
    assert_eq!(methods, [("total", false)]);
    drop(vm);
}
