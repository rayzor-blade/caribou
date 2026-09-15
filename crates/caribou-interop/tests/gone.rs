//! A Wren VM dropped while Haxe keeps one of its objects: the cell Haxe
//! holds stands for nothing after, a send through it raises instead of
//! reaching freed memory, the VM's published classes leave the registry,
//! and a collection after runs clean. A new VM then serves as before.

use std::cell::RefCell;
use std::rc::Rc;

use caribou::error::{Error, Str};
use caribou::registry::{self, Namespace};
use caribou::symbol::intern;
use caribou::world::{Config, LANG_CORE, World};
use caribou::{bridge, heap};
use caribou_abi::Value;
use caribou_ash::{Mode, Options};
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

const KEEPER: &str = "\
import \"game:Player\" for Player
class Thing {
  construct new() {}
  hello { \"hi\" }
}
Player.onHit = Fn.new {|d| Thing.new().hello }
";

fn vm(errors: &Rc<RefCell<Vec<String>>>) -> VM {
    let sink = Rc::clone(errors);
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
    vm
}

#[test]
fn a_cell_outlives_its_vm_as_a_gone_object() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");
    let world = World::new(Config {
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
    let wren = world
        .register(Box::new(caribou_wren::Runtime::new()))
        .expect("wren registers")[0];
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/game.hl");
    let mut program = caribou_ash::load(
        &fixture,
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the fixture loads");
    program.publish().expect("the program publishes");
    // Ash registers what a call into Haxe needs as the program starts.
    program.start().expect("main runs");

    let errors = Rc::new(RefCell::new(Vec::new()));
    let mut first = vm(&errors);
    caribou_wren::with_vm(&mut first, |vm| {
        assert_eq!(vm.interpret("keeper", KEEPER), InterpretResult::Success);
        caribou_wren::publish_module(vm, "keeper").expect("Thing publishes");
    });
    assert!(registry::lookup_class("wren", "keeper", "Thing").is_some());

    // Haxe keeps the Wren function in its static and calls it from
    // `hit`; once while its VM lives.
    let (player, index) = registry::lookup_class("game", "Player", "Player").expect("published");
    let name = Str::value(Str::new("ada"));
    let p = caribou_ash::construct(&player.classes[index], &[name]).expect("a Player");
    let root = heap::handle_new(p.as_object().unwrap() as *mut u8);
    let hit = |p| bridge::invoke(p, intern("hit"), &[Value::int(3)], LANG_CORE);
    caribou_wren::with_vm(&mut first, |_| {
        assert_eq!(
            hit(p).expect("the callback runs on its VM"),
            Value::bool(false)
        );
    });

    drop(first);

    // The VM's classes are gone from the registry.
    assert!(registry::lookup_class("wren", "keeper", "Thing").is_none());
    assert!(registry::interface(wren, "keeper").is_none());

    // The function Haxe kept stands for nothing now: the call raises,
    // and Haxe's throw reaches the caller as the error.
    let error = hit(p).expect_err("the function is gone");
    let message = unsafe { Error::message_str(error.as_object().unwrap() as *const Error) };
    assert!(message.contains("gone"), "{message}");

    // The severed cell marks nothing; the collection runs clean.
    heap::major();

    // A new VM serves the same program: its function replaces the gone
    // one in the static.
    let mut second = vm(&errors);
    caribou_wren::with_vm(&mut second, |vm| {
        assert_eq!(vm.interpret("keeper", KEEPER), InterpretResult::Success);
        caribou_wren::publish_module(vm, "keeper").expect("Thing publishes again");
    });
    assert!(registry::lookup_class("wren", "keeper", "Thing").is_some());
    caribou_wren::with_vm(&mut second, |_| {
        assert_eq!(
            hit(p).expect("the new VM's callback runs"),
            Value::bool(false)
        );
    });
    heap::handle_release(root);
    drop(second);
    assert!(errors.take().is_empty());
}
