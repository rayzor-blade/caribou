//! Python's objects, closures and errors reached from the core: an object
//! a constructor makes is a proxy whose fields read and write the object
//! and whose methods run on it, the same object crosses as the same
//! proxy, a closure is callable, and an exception a call or a module body
//! raises is an error of the core naming it.

use std::path::PathBuf;

use caribou::bridge;
use caribou::error::Str;
use caribou::protocol::Callable;
use caribou::registry::{self, Namespace};
use caribou::symbol::intern;
use caribou::world::{Config, LANG_CORE, World};
use caribou_abi::Value;
use caribou_zyntax::Frontend;

fn message(e: Value) -> String {
    match unsafe { caribou::error::Error::from_value(e) } {
        Some(e) => unsafe { caribou::error::Error::message_str(e) }.to_owned(),
        None => bridge::describe(e),
    }
}

#[test]
fn python_objects_closures_and_errors_cross_to_the_core() {
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

    let ledger = registry::lookup_or_load("shop", "ledger")
        .expect("loads")
        .expect("published");
    let account = &ledger.classes[0];
    assert_eq!(account.name, "Account");
    let call = |target: Callable, args: &[Value]| bridge::call(target, args, LANG_CORE);
    let method = |name: &str| {
        account
            .methods
            .iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("Account publishes {name}"))
            .target
    };

    // A constructor's object, its fields read and written in place.
    let ctor = account.ctor.as_ref().expect("Account constructs").target;
    let owner = Str::value(Str::new("ada"));
    let a = call(ctor, &[owner, Value::number(10.0)]).expect("constructed");
    let get = |name: &str| bridge::get(a, intern(name), LANG_CORE).expect("a field");
    assert_eq!(get("balance").as_number(), Some(10.0));
    assert_eq!(unsafe { Str::text(get("owner")) }, Some("ada"));
    bridge::set(a, intern("balance"), Value::number(12.5), LANG_CORE).expect("set");
    assert_eq!(get("balance").as_number(), Some(12.5));

    // Its methods, through the published target and as the object's own.
    let deposited = call(method("deposit"), &[a, Value::number(2.5)]).expect("deposited");
    assert_eq!(deposited.as_number(), Some(15.0));
    let sent = bridge::invoke(a, intern("deposit"), &[Value::number(5.0)], LANG_CORE)
        .expect("deposited");
    assert_eq!(sent.as_number(), Some(20.0));

    // The same object crosses back as the same proxy.
    let same = call(method("same"), &[a]).expect("itself");
    assert_eq!(same.to_bits(), a.to_bits());

    // A module function returning an object of the class.
    let open = ledger
        .functions
        .iter()
        .find(|f| f.name == "open_account")
        .expect("open_account")
        .target;
    let b = call(open, &[Str::value(Str::new("bo"))]).expect("opened");
    assert_eq!(
        bridge::get(b, intern("balance"), LANG_CORE)
            .expect("a field")
            .as_number(),
        Some(0.0)
    );

    // A closure, called with what it captured.
    let scaler = account
        .methods
        .iter()
        .find(|m| m.name == "scaler")
        .expect("scaler")
        .target;
    let triple = call(scaler, &[Value::number(3.0)]).expect("a closure");
    let tripled = call(Callable::Dynamic(triple), &[Value::number(7.0)]).expect("called");
    assert_eq!(tripled.as_number(), Some(21.0));

    // An exception a call raises is an error of the core naming it, and
    // the object is as it was.
    let refused = call(method("deposit"), &[a, Value::number(-1.0)]).expect_err("refused");
    assert_eq!(message(refused), "ValueError: a deposit cannot be negative");
    assert_eq!(get("balance").as_number(), Some(20.0));

    // An object of another class is refused where an Account goes.
    let wrong = call(method("deposit"), &[triple, Value::number(1.0)]).expect_err("refused");
    assert!(message(wrong).contains("argument 1"), "{}", message(wrong));

    // A module whose body raises does not load, and says why.
    let broken = registry::lookup_or_load("shop", "broken").expect_err("its body raised");
    assert!(
        broken.ends_with("RuntimeError: the ledger is closed"),
        "{broken}"
    );
}
