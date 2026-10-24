//! An async ZynML function's `sleep` follows the world's clock: under a
//! stepped clock it ends when the host moves the clock on, not as wall
//! time passes.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use caribou::bridge;
use caribou::registry::{self, Namespace};
use caribou::sched;
use caribou::world::{Config, LANG_CORE, World};
use caribou_abi::Value;
use caribou_zyntax::Frontend;

#[test]
fn a_zynml_sleep_ends_when_the_clock_moves_on() {
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

    sched::step_clock();
    let future = bridge::call(doubled.target, &[Value::int(21)], LANG_CORE).expect("a future");
    let pending = caribou::future::of(future).expect("a caribou.Future");
    let ready = || unsafe { caribou::future::ready(pending) };
    // Well past its 250 ms of wall time, it still waits on the clock.
    let wall = Instant::now();
    while wall.elapsed() < Duration::from_millis(400) {
        sched::tick(None);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !ready(),
        "wall time does not end a sleep on a stepped clock"
    );

    sched::advance_clock(Duration::from_millis(250));
    let moved = Instant::now();
    while !ready() && moved.elapsed() < Duration::from_secs(5) {
        sched::tick(None);
    }
    assert!(ready(), "the advance ends it");
    sched::wall_clock();
}
