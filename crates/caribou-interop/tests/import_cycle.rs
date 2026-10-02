//! An import cycle through two languages, Python's module importing a
//! Wren module that imports it back: the import inside the cycle fails
//! with an error naming it, where the Python module used to load a second
//! time inside its first load and leave the process hung.
//!
//! An uncaught error in a Python module's body still ends the process
//! (git-bug cb8d9f10dc2c1d940a61ed6e46978c0f72adcfd02e19e7631716deebb74bb39a),
//! so the cycle runs in a child process of this test binary.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use caribou::registry::{self, Namespace};
use caribou::world::{Config, World};
use caribou_zyntax::Frontend;
use wren_lift::runtime::engine::ExecutionMode;
use wren_lift::runtime::vm::{VM, VMConfig};

#[test]
fn a_cycle_through_python_and_wren_names_itself() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "load_the_cycle", "--nocapture"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(120) {
            child.kill().unwrap();
            panic!("the cyclic import did not return");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(
        stderr.contains("ImportError: import cycle: cyc:py_side -> cyc:wren_side -> cyc:py_side;"),
        "{stderr}"
    );
}

#[test]
#[ignore = "run by a_cycle_through_python_and_wren_names_itself in a child process"]
fn load_the_cycle() {
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
    eprintln!("{error}");
}
