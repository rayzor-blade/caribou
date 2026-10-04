//! Python's allocations come from the core heap, and what a program drops
//! is reclaimed: many rounds of objects nothing keeps leave the heap no
//! larger than a few rounds would.

use std::path::PathBuf;

use caribou::bridge;
use caribou::heap;
use caribou::registry::{self, Namespace};
use caribou::world::{Config, LANG_CORE, World};
use caribou_abi::Value;
use caribou_zyntax::Frontend;

fn heap_stats() -> (f64, f64) {
    let (mut allocated, mut current) = (0.0, 0.0);
    unsafe { heap::stats(&mut allocated, std::ptr::null_mut(), &mut current) };
    (allocated, current)
}

#[test]
fn python_garbage_is_reclaimed_by_the_core_heap() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/pyobj/src");
    let world = World::new(Config {
        namespaces: vec![Namespace {
            name: "shop".to_owned(),
            langs: vec!["python".to_owned()],
            modules: None,
        }],
        roots: vec![root],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![Frontend::new(
            Box::new(caribou_python::Python::new()),
        )])))
        .expect("python registers");
    let module = registry::lookup_or_load("shop", "churn")
        .expect("loads")
        .expect("published");
    let churn = module
        .functions
        .iter()
        .find(|f| f.name == "churn")
        .expect("churn")
        .target;
    let rounds = 200_000;
    let run = || {
        let total = bridge::call(churn, &[Value::int(rounds)], LANG_CORE).expect("churned");
        let rounds = i64::from(rounds);
        let total = total
            .as_int()
            .map(i64::from)
            .or_else(|| caribou::error::Int64::of(total));
        assert_eq!(total, Some(rounds * (rounds + 1) / 2));
    };

    run();
    heap::major();
    let (allocated_before, settled) = heap_stats();
    for _ in 0..4 {
        run();
    }
    heap::major();
    let (allocated_after, current) = heap_stats();

    let churned = allocated_after - allocated_before;
    assert!(
        churned > 4.0 * f64::from(rounds) * 48.0,
        "the rounds allocate from the core heap: {churned} bytes"
    );
    assert!(
        current <= settled + 8.0 * 1024.0 * 1024.0,
        "what they drop is reclaimed: {settled} bytes settled, {current} after"
    );
}
