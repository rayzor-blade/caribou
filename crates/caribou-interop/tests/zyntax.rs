//! A Zyntax module is a module of the world: the frontend (ZynML's
//! snapshot) is a language, the module's functions and structs are
//! published from its declarations and called as machine code by
//! signature, and Wren reaches them by the ordinary import through the
//! namespace both languages share: the functions as module variables,
//! the struct as a class.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::registry::{self, Namespace, TypeRef};
use caribou::world::{Config, World};
use caribou_zyntax::Frontend;
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::vm::{VM, VMConfig};

const USE: &str = r#"
import "game:scorer" for score, weight, perfect, echo
System.print(score.call(7, 2))
System.print(weight.call(3, 1.5))
System.print(perfect.call(5, 5))
System.print(perfect.call(4, 5))
System.print(Fiber.new { score.call("seven", 2) }.try())
System.print(echo.call("goal"))
"#;

/// A struct the module declares is a class with its fields and methods;
/// its statics over scalars are called, and what needs an object to
/// cross says so.
const POINT: &str = r#"
import "game:scorer" for Point
System.print(Point.area(3, 4))
System.print(Fiber.new { Point.origin() }.try())
"#;

#[test]
fn a_zynml_module_is_imported_from_wren() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/src");
    let frontend = Frontend::snapshot(zynml::snapshot_bytes()).expect("the snapshot loads");
    assert_eq!(frontend.name(), "zynml");

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
    let langs = world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![frontend])))
        .expect("zyntax registers");
    assert_eq!(caribou::world::language_name(langs[0]), "zynml");

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
    assert_eq!(
        output,
        "64\n5\ntrue\nfalse\nargument 1 of the function cannot be a caribou.Str\ngoal\n"
    );
    vm.output_buffer = Some(String::new());
    let result = caribou_wren::with_vm(&mut vm, |vm| vm.interpret("point", POINT));
    let output = vm.take_output();
    assert_eq!(
        result,
        InterpretResult::Success,
        "{:?} {output:?}",
        errors.borrow()
    );
    assert_eq!(
        output,
        "12\nthe function returns a Zyntax type the core does not pass yet\n"
    );

    // Published from the declarations: the module's functions typed by
    // their signatures, as the module's own.
    let iface = registry::lookup("game", "scorer").expect("published");
    let weight = iface
        .functions
        .iter()
        .find(|m| m.name == "weight")
        .expect("weight");
    assert_eq!(weight.params, vec![TypeRef::Float, TypeRef::Float]);
    assert_eq!(weight.ret, TypeRef::Float);
    let echo = iface
        .functions
        .iter()
        .find(|m| m.name == "echo")
        .expect("echo");
    assert_eq!(
        (echo.params.clone(), echo.ret.clone()),
        (vec![TypeRef::Str], TypeRef::Str)
    );
    // The struct, from the typed declarations: fields, a constructor, a
    // method on the instance and statics, each with its Zyntax types.
    let (iface, index) = registry::lookup_class("game", "scorer", "Point").expect("published");
    let point = &iface.classes[index];
    assert_eq!(point.type_name, "zynml.Point");
    let fields: Vec<(&str, &TypeRef)> = point
        .fields
        .iter()
        .map(|f| (f.name.as_str(), &f.ty))
        .collect();
    assert_eq!(fields, vec![("x", &TypeRef::Float), ("y", &TypeRef::Float)]);
    let ctor = point.ctor.as_ref().unwrap_or_else(|| {
        panic!(
            "{:?}",
            point
                .methods
                .iter()
                .map(|m| (m.name.clone(), m.is_static, m.ret.clone(), m.params.clone()))
                .collect::<Vec<_>>()
        )
    });
    assert_eq!(ctor.ret, TypeRef::Object("zynml.Point".to_owned()));
    let len = point.methods.iter().find(|m| m.name == "len").expect("len");
    assert!(!len.is_static);
    assert_eq!(
        (len.params.clone(), len.ret.clone()),
        (vec![], TypeRef::Float)
    );
    let origin = point
        .methods
        .iter()
        .find(|m| m.name == "origin")
        .expect("origin");
    assert!(origin.is_static);
    assert_eq!(origin.ret, TypeRef::Object("zynml.Point".to_owned()));
    drop(vm);
}
