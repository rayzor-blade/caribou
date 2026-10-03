//! An import cycle through two languages, Python's module importing a
//! Wren module that imports it back: the load fails with an error naming
//! the cycle, where it used to load the Python module a second time
//! inside its first load and leave the process hung.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use caribou::registry::{self, Namespace};
use caribou::world::{Config, World};
use caribou_zyntax::Frontend;
use wren_lift::runtime::engine::ExecutionMode;
use wren_lift::runtime::vm::{VM, VMConfig};

#[test]
fn a_cycle_through_python_and_wren_names_itself() {
    // A hang is the failure this guards against; end the process instead.
    let (done, finished) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(120)).is_err() {
            eprintln!("the cyclic import did not return");
            std::process::abort();
        }
    });

    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/cycle/src");
    let world = World::new(Config {
        namespaces: vec![Namespace {
            name: "cyc".to_owned(),
            langs: vec!["wren".to_owned(), "python".to_owned()],
            modules: None,
        }],
        roots: vec![root],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_wren::Runtime::new()))
        .expect("wren registers");
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![Frontend::new(
            Box::new(caribou_python::Python::new()),
        )])))
        .expect("python registers");

    let mut config = VMConfig {
        execution_mode: ExecutionMode::Interpreter,
        error_fn: Some(Box::new(|_, _, _, _| {})),
        ..VMConfig::default()
    };
    caribou_wren::import::configure(&mut config);
    let mut vm = VM::new(config);
    vm.krio_fiber_active = true;
    let loaded = caribou_wren::with_vm(&mut vm, |_| registry::lookup_or_load("cyc", "py_side"));
    let error = loaded.expect_err("a module that imports itself back does not load");
    assert!(
        error.starts_with("import cycle: cyc:py_side -> cyc:wren_side -> cyc:py_side;"),
        "{error}"
    );
    let _ = done.send(());
}
