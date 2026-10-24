//! An async ZynML call started in a scope stops when the scope is
//! cancelled: its future is rejected, long before its sleep would end.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use caribou::bridge;
use caribou::registry::{self, Namespace};
use caribou::sched;
use caribou::world::{Config, LANG_CORE, World};
use caribou_abi::Value;
use caribou_zyntax::Frontend;

#[test]
fn a_cancelled_scope_rejects_its_zynml_tasks() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/src");
    let frontend = Frontend::snapshot(zynml::snapshot_bytes()).expect("the snapshot loads");
    let world = World::new(Config {
        namespaces: vec![Namespace {
            name: "game".to_owned(),
            langs: vec!["zynml".to_owned()],
            modules: None,
        }],
        roots: vec![root],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![frontend])))
        .expect("zyntax registers");
    let later = registry::lookup_or_load("game", "later")
        .unwrap()
        .expect("game:later");
    let doubled = later
        .functions
        .iter()
        .find(|f| f.name == "doubled")
        .expect("doubled");

    let scope = sched::new_scope();
    let outer = sched::enter_scope(scope);
    let future = bridge::call(doubled.target, &[Value::int(21)], LANG_CORE).expect("a future");
    sched::enter_scope(outer);
    sched::tick(None);
    assert_eq!(sched::cancel_scope(scope), 1);

    let start = Instant::now();
    let awaited = bridge::invoke(future, caribou::symbol::intern("await"), &[], LANG_CORE);
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "{:?}",
        start.elapsed()
    );
    let error = awaited.expect_err("the future is rejected");
    let error = unsafe { caribou::error::Error::from_value(error) }.expect("an Error");
    assert_eq!(
        unsafe { caribou::error::Error::message_str(error) },
        "the task was cancelled"
    );
}
