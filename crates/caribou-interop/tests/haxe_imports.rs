//! A Haxe program imports a Wren class with an ordinary `import`: the build
//! macro emitted `game.hud.Hud` for the Wren module, every member of it is
//! a native the runtime binds by name when the program loads, and each
//! call reaches the Wren method through the bridge. Objects keep their
//! identity across the crossing, strings cross by value, and a Wren abort
//! is what Haxe catches.
//!
//! One test, because the seams are process-global: both are installed
//! before either runtime allocates, the world registers both adapters and
//! the shared namespace, the Wren module loads and publishes, and only
//! then does the Haxe program run.

use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::bridge;
use caribou::heap;
use caribou::registry::Namespace;
use caribou::world::{Config, World};
use caribou_ash::{Mode, Options};
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

const HUD: &str = include_str!("../fixtures/src/game/hud.wren");

const EXPECTED: &str = "7\n10\nhp: 10\ntrue\n10\ncaught boom\n";

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/hud.hl")
}

/// What `f` writes to the process's stdout: the Haxe program prints
/// through the runtime's own `Sys.println`.
fn captured<F: FnOnce()>(f: F) -> String {
    let path = std::env::temp_dir().join(format!("caribou-haxe-imports-{}", std::process::id()));
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("a capture file");
    std::io::stdout().flush().unwrap();
    let saved = unsafe { libc::dup(1) };
    assert!(unsafe { libc::dup2(file.as_raw_fd(), 1) } >= 0);
    f();
    std::io::stdout().flush().unwrap();
    unsafe { libc::fflush(std::ptr::null_mut()) };
    assert!(unsafe { libc::dup2(saved, 1) } >= 0);
    unsafe { libc::close(saved) };
    let mut text = String::new();
    file.rewind().unwrap();
    file.read_to_string(&mut text).unwrap();
    drop(file);
    let _ = std::fs::remove_file(path);
    text
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

    let errors = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&errors);
    let mut vm = VM::new(VMConfig {
        execution_mode: ExecutionMode::Interpreter,
        gc_strategy: GcStrategy::Immix,
        error_fn: Some(Box::new(move |_kind, _module, _line, message: &str| {
            sink.borrow_mut().push(message.to_owned());
        })),
        ..VMConfig::default()
    });
    assert_eq!(
        caribou_wren::with_vm(&mut vm, |vm| vm.interpret("hud", HUD)),
        InterpretResult::Success
    );
    let published = caribou_wren::publish_module(&vm, "hud").expect("the module publishes");
    assert_eq!(published.classes[0].name, "Hud");

    let mut program = caribou_ash::load(
        &fixture(),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the fixture loads: its caribou natives bind by name");
    let output = captured(|| {
        caribou_wren::with_vm(&mut vm, |_| program.start().expect("main runs"));
    });
    assert_eq!(output, EXPECTED);

    // Both collectors run while a Haxe static holds the face: the Wren
    // object stays, and the face is still the same object.
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
    assert_eq!(output, "after: 10\n");
    assert!(errors.take().is_empty());
    program.finish();
}
