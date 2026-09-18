//! A native plugin on the shared ABI: loaded by the driver's crate,
//! registered as a language of the world under its own name, its table
//! published as classes, and reached from Wren by the ordinary import.
//! Scalars cross by kind, a value as itself, and a wrong argument or
//! another ABI version is an error, not a call.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::registry;
use caribou::world::{Config, World};
use caribou_abi::{ABI_VERSION, TypeTag};
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

/// Where the build script put the test plugins.
fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("OUT_DIR")).join("plugins/debug")
}

const USE: &str = r#"
import "math:Math" for Math
import "math:Vec" for Vec
System.print(Math.hypot(3, 4))
System.print(Math.twice(21))
System.print(Math.isEven(4294967298))
System.print(Math.isEven(3))
System.print(Math.bump() + Math.bump())
System.print(Math.same("as it is"))
System.print(Math.same([1, 2]).count)
System.print(Vec.len3(1, 2, 2))
System.print(Fiber.new { Math.twice("no") }.try())
"#;

#[test]
fn a_plugin_is_a_language_wren_imports() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");

    let plugins = caribou_plugin::load_dir(&plugin_dir()).expect("the plugins load");
    assert_eq!(plugins.len(), 1, "{:?}", plugin_dir());
    let math = &plugins[0];
    assert_eq!(math.name(), "math");
    assert_eq!(math.symbols().len(), 6);
    let hypot = math
        .symbols()
        .iter()
        .find(|s| unsafe { s.method.as_str() } == "hypot")
        .unwrap();
    assert_eq!(hypot.param_count, 2);
    assert_eq!(hypot.params[0], TypeTag::F64);
    assert_eq!(hypot.ret, TypeTag::F64);
    assert_eq!(ABI_VERSION, 1);

    let world = World::new(Config::default());
    world
        .register(Box::new(caribou_ash::Runtime::new()))
        .expect("haxe registers");
    world
        .register(Box::new(caribou_wren::Runtime::new()))
        .expect("wren registers");
    let langs = world
        .register(Box::new(caribou_plugin::Runtime::new(plugins)))
        .expect("the plugin registers");
    assert_eq!(langs.len(), 1);
    assert_eq!(caribou::world::language_name(langs[0]), "math");
    // Its classes are published under its own namespace.
    assert!(registry::lookup_class("math", "Math", "Math").is_some());
    assert!(registry::lookup_class("math", "Vec", "Vec").is_some());

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
    assert_eq!(result, InterpretResult::Success, "{:?}", errors.borrow());
    assert_eq!(
        output,
        "5\n42\ntrue\nfalse\n3\nas it is\n2\n3\nargument 1 of the plugin function cannot be a caribou.Str\n"
    );
    drop(vm);
}
