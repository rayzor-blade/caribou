//! One world: a Haxe thread, a Wren fiber and a Wren thread are tasks of
//! the same scheduler. A wait on either side, a Haxe `Lock` or a Wren
//! one, lets every other task run; a Haxe call from a Wren task that
//! parks inside a `try` still catches what Haxe throws after the park;
//! and a Wren call from a Haxe task that parks keeps what its frame holds
//! through a Wren cycle, though no Wren stack the cycle can place holds it.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::registry::Namespace;
use caribou::world::{Config, World};
use caribou_ash::{Mode, Options};
use caribou_interop::captured;
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

const RELAY: &str = include_str!("../fixtures/src/game/relay.wren");

const EXPECTED: &str = "haxe-thread\nwren-fiber,wren-thread\ncaught late2 caught late4\n2005890\n";

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/relay.hl")
}

#[test]
fn haxe_and_wren_tasks_share_the_world() {
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
    .expect("the fixture loads");
    program.publish().expect("the program publishes");

    let errors = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&errors);
    let mut config = VMConfig {
        execution_mode: ExecutionMode::Jit,
        gc_strategy: GcStrategy::Immix,
        error_fn: Some(Box::new(move |_kind, _module, _line, message: &str| {
            sink.borrow_mut().push(message.to_owned());
        })),
        ..VMConfig::default()
    };
    caribou_wren::import::configure(&mut config);
    let mut vm = VM::new(config);
    vm.krio_fiber_active = true;
    assert_eq!(
        caribou_wren::with_vm(&mut vm, |vm| vm.interpret("relay", RELAY)),
        InterpretResult::Success,
        "{:?}",
        errors.borrow()
    );
    caribou_wren::publish_module(&vm, "relay").expect("the module publishes");

    let output = captured(|| {
        caribou_wren::with_vm(&mut vm, |_| program.start().expect("main runs"));
    });
    assert_eq!(output, EXPECTED);
    assert!(errors.take().is_empty());
    program.finish();
}
