//! A Haxe program imports a Wren class with an ordinary `import`, and the
//! Wren module imports a class of the Haxe program: the build macro emitted
//! `game.hud.Hud` for the Wren module, every member of it is a native the
//! runtime binds by name when the program loads, and each call reaches the
//! Wren method through the bridge. Objects keep their identity across the
//! crossing, strings cross by value, and a Wren abort is what Haxe catches.
//!
//! Wired by hand, unlike `project.rs`: both seams are installed before
//! either runtime allocates, the world registers both adapters and the
//! shared namespace, the Haxe program loads and publishes, the Wren module
//! loads and publishes, and only then does the Haxe program run.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::bridge;
use caribou::heap;
use caribou::registry::Namespace;
use caribou::world::{Config, World};
use caribou_ash::{Mode, Options};
use caribou_interop::captured;
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

const HUD: &str = include_str!("../fixtures/src/game/hud.wren");

const EXPECTED: &str = "7\n10\nhp: 10\ntrue\n10\ncaught boom\nada\n5\n42\n3\n6\n";

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/hud.hl")
}

#[test]
fn a_haxe_program_imports_a_wren_class() {
    // Both seams before either runtime allocates.
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");

    let mut world = World::new(Config {
        namespaces: vec![Namespace {
            name: "game".to_owned(),
            langs: vec!["haxe".to_owned(), "wren".to_owned()],
            modules: None,
        }],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_ash::Runtime::new()))
        .expect("haxe registers");
    world
        .register(Box::new(caribou_wren::Runtime::new()))
        .expect("wren registers");

    let mut program = caribou_ash::load(
        &fixture(),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the fixture loads: its caribou natives bind by name");
    // Its classes, for the Wren module's own import, before anything runs.
    program.publish().expect("the program publishes");

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
    assert_eq!(
        caribou_wren::with_vm(&mut vm, |vm| vm.interpret("hud", HUD)),
        InterpretResult::Success,
        "{:?}",
        errors.borrow()
    );
    let published = caribou_wren::publish_module(&vm, "hud").expect("the module publishes");
    assert_eq!(published.classes[0].name, "Hud");

    let output = captured(|| {
        caribou_wren::with_vm(&mut vm, |_| program.start().expect("main runs"));
    });
    assert_eq!(output, EXPECTED);

    // Both collectors run while Haxe statics hold the face and a typed
    // closure: the Wren object and the function stay, and the face is
    // still the same object.
    let after = program
        .publish()
        .expect("the program publishes")
        .iter()
        .flat_map(|iface| iface.classes.iter())
        .find(|c| c.name == "UseHud")
        .and_then(|c| c.methods.iter().find(|m| m.name == "after" && m.is_static))
        .map(|m| m.target)
        .expect("UseHud.after is published");
    let output = captured(|| {
        caribou_wren::with_vm(&mut vm, |vm| {
            heap::major();
            vm.collect_garbage();
            bridge::call(after, &[], caribou_wren::lang()).expect("after runs");
        });
    });
    assert_eq!(output, "after: 10\n12\n");
    assert!(errors.take().is_empty());
    program.finish();
}
