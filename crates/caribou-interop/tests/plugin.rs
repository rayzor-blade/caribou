//! A native plugin on the shared ABI: loaded by the driver's crate,
//! registered as a language of the world under its own name, its table
//! published as classes, and reached from Wren by the ordinary import.
//! Scalars cross by kind, a value as itself, an instance of a plugin
//! class as the object the core holds its payload by, and a wrong
//! argument is an error, not a call. An object nothing holds is released
//! through the plugin's finalizer.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::registry;
use caribou::world::{Config, World};
use caribou_abi::{ABI_VERSION, TypeTag};
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
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
System.print(Math.is_even(4294967298))
System.print(Math.is_even(3))
System.print(Math.bump() + Math.bump())
System.print(Math.same("as it is"))
System.print(Math.same([1, 2]).count)
System.print(Vec.len3(1, 2, 2))
System.print(Fiber.new { Math.twice("no") }.try())
System.print(Math.shout("héllo"))
System.print(Math.width("héllo"))
System.print(Fiber.new { Math.width(5) }.try())
System.print(Math.quotient(1, 4))
System.print(Fiber.new { Math.quotient(1, 0) }.try())
"#;

/// A class that keeps a function across calls and calls it, and hands
/// on what the function raises.
const TALLY: &str = r#"
import "math:Tally" for Tally
var t = Tally.new()
System.print(t.add(2))
var seen = []
t.watch(Fn.new {|total|
  seen.add(total)
  total * 10
})
System.print(t.add(3))
System.print(t.add(1))
System.print(seen)
System.print(t.label("sum"))
t.watch(Fn.new {|total| Fiber.abort("too much: %(total)") })
System.print(Fiber.new { t.add(1) }.try())
"#;

/// A class with instances: constructed, sent to, passed to another of
/// its own, compared, and refused where another class is expected.
const OBJECTS: &str = r#"
import "math:Vec2" for Vec2
var v = Vec2.new(3, 4)
System.print(v.len())
v.scale(2)
System.print(v.len())
System.print(v.dot(Vec2.new(1, 0)))
System.print(v.unit().len())
System.print(v is Vec2)
System.print(v == v)
System.print(Fiber.new { v.dot(5) }.try())
for (i in 0...100) Vec2.new(i, i)
System.print(Vec2.live() >= 2)
"#;

const LIVE: &str = r#"
import "math:Vec2" for Vec2
System.print(Vec2.live() < 10)
"#;

const DATA: &str = r#"
import "math:Data" for Data
import "math:Event" for Event
import "core:Future" for Future
var b = Data.bytes()
System.print(b.count)
System.print(b[0])
System.print(b[2])
b[0] = 23
var alias = Data.echo(b)
System.print(Data.same_storage(b, alias))
System.print(Data.sum(alias))
var sum = 0
for (byte in b) sum = sum + byte
System.print(sum)
Data.save(b)
System.print(Data.saved()[0])
var e = Data.event(1)
System.print(e is Event)
System.print(e.tag)
System.print(e.constructor)
System.print(e.width)
System.print(Data.area(e))
System.print(Data.echo_event(e) == e)
System.print(Fiber.new { Data.sum("text") }.try())
System.print(Fiber.new { Data.area(b) }.try())
var pair = Data.pair()
var rebuilt = Data.rebuild_nested(pair)
System.print(rebuilt.first.label)
System.print(rebuilt.first.bytes[0])
System.print(Data.same_storage(pair.first.bytes, rebuilt.first.bytes))
var future = Data.later(73)
System.print(future.ready() || !future.ready())
System.print(future.await())
var manual = Future.new()
System.print(manual.resolve(91))
System.print(manual.resolve(92))
System.print(manual.await())
var rejected = Future.new()
System.print(rejected.reject("future failed"))
System.print(Fiber.new { rejected.await() }.try())
"#;

#[test]
fn a_plugin_is_a_language_wren_imports() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");

    let plugins = caribou_plugin::load_dir(&plugin_dir()).expect("the plugins load");
    assert_eq!(plugins.len(), 1, "{:?}", plugin_dir());
    let math = &plugins[0];
    assert_eq!(math.name(), "math");
    assert_eq!(math.symbols().len(), 35);
    let hypot = math
        .symbols()
        .iter()
        .find(|s| unsafe { s.method.as_str() } == "hypot")
        .unwrap();
    assert_eq!(hypot.param_count, 2);
    assert_eq!(hypot.params[0], TypeTag::F64);
    assert_eq!(hypot.ret, TypeTag::F64);
    assert_eq!(ABI_VERSION, 3);
    assert_eq!(math.classes().len(), 4);

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
    let (iface, index) = registry::lookup_class("math", "Vec2", "Vec2").expect("published");
    assert!(
        iface.classes[index].ctor.is_some(),
        "new is the constructor"
    );
    assert_eq!(iface.classes[index].type_name, "math.Vec2");

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
    assert_eq!(result, InterpretResult::Success, "{:?}", errors.borrow());
    assert_eq!(
        output,
        "5\n42\ntrue\nfalse\n3\nas it is\n2\n3\nargument 1 of the plugin function cannot be a caribou.Str\n\
         HÉLLO!\n5\nargument 1 of the plugin function cannot be a number\n0.25\nquotient by zero\n"
    );

    vm.output_buffer = Some(String::new());
    let result = caribou_wren::with_vm(&mut vm, |vm| vm.interpret("tally", TALLY));
    let output = vm.take_output();
    assert_eq!(
        result,
        InterpretResult::Success,
        "{:?} {output:?}",
        errors.borrow()
    );
    assert_eq!(output, "2\n50\n510\n[5, 51]\nsum: 510\ntoo much: 511\n");

    vm.output_buffer = Some(String::new());
    let result = caribou_wren::with_vm(&mut vm, |vm| vm.interpret("objects", OBJECTS));
    let output = vm.take_output();
    assert_eq!(
        result,
        InterpretResult::Success,
        "{:?} {output:?}",
        errors.borrow()
    );
    assert_eq!(
        output,
        "5\n10\n6\n1\ntrue\ntrue\nargument 2 of the plugin function must be a math.Vec2, not a number\ntrue\n"
    );
    vm.output_buffer = Some(String::new());
    let result = caribou_wren::with_vm(&mut vm, |vm| vm.interpret("data", DATA));
    let output = vm.take_output();
    assert_eq!(
        result,
        InterpretResult::Success,
        "{:?} {output:?}",
        errors.borrow()
    );
    assert_eq!(
        output,
        "4\n0\n255\ntrue\n471\n471\n23\ntrue\n1\nResized\n800\n480000\ntrue\nargument 1 of the plugin function must be a caribou.Buffer, not a caribou.Str\nargument 1 of the plugin function must be a math.Event, not a caribou.Buffer\nfirst\n42\ntrue\ntrue\n73\ntrue\nfalse\n91\ntrue\nfuture failed\n"
    );

    // The temporaries die with Wren's cycle and the core's collection
    // that ends it: their instances go, then the cells they held the
    // objects by, then the objects, through the plugin's finalizer.
    caribou_wren::with_vm(&mut vm, |vm| vm.collect_garbage());
    caribou::heap::major();
    vm.output_buffer = Some(String::new());
    let result = caribou_wren::with_vm(&mut vm, |vm| vm.interpret("live", LIVE));
    let output = vm.take_output();
    assert_eq!(result, InterpretResult::Success, "{:?}", errors.borrow());
    assert_eq!(output, "true\n");
    drop(vm);
}
