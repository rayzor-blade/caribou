//! A program loaded, started and published, and its classes driven through
//! the bridge from Rust. Needs the `runner` feature. One test: the seam and
//! ash's standard library are process-global.

#![cfg(feature = "runner")]

use std::path::PathBuf;

use caribou::bridge;
use caribou::error::{Error, Str};
use caribou::heap;
use caribou::registry::{self, TypeRef};
use caribou::symbol::intern;
use caribou::world::{Config, World};
use caribou_abi::hl;
use caribou_abi::{ErrorKind, Value};
use caribou_ash::{Mode, Options};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../caribou-interop/fixtures/game.hl")
}

#[test]
fn a_started_program_publishes_its_classes_and_the_bridge_drives_them() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    let mut world = World::new(Config::default());
    let haxe = world
        .register(Box::new(caribou_ash::Runtime::new()))
        .expect("haxe registers")[0];

    let mut program = caribou_ash::load(
        &fixture(),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the fixture loads");

    // The decoded shape: the instance type holds the fields and the
    // instance methods; its companion, an `hl.Class`, holds the statics as
    // function-typed fields bound to their functions, and binds the class's
    // inherited `__constructor__` field to the constructor.
    let types = &program.bytecode().types;
    let player = types
        .iter()
        .find(|t| t.obj.as_ref().is_some_and(|o| o.name == "game.Player"))
        .and_then(|t| t.obj.as_ref())
        .expect("game.Player is decoded");
    assert_eq!(
        player
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["hp", "name"]
    );
    assert_eq!(
        player
            .proto
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["hit", "explode"]
    );
    assert!(player.global_value != 0, "the class object has a global");
    let companion = types
        .iter()
        .find(|t| t.obj.as_ref().is_some_and(|o| o.name == "game.$Player"))
        .and_then(|t| t.obj.as_ref())
        .expect("the companion is decoded");
    assert_eq!(
        companion
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["spawnAt"]
    );
    assert!(companion.proto.is_empty());
    assert_eq!(
        companion.bindings.len(),
        4,
        "the constructor and one static: {:?}",
        companion.bindings
    );
    // Publishing before the start is allowed: the interpreter registers
    // its closure runner as the program starts, before main, so a caller
    // that waits for main reaches every published class.
    let early = program.publish().expect("publishes before the start");
    assert!(early.iter().any(|i| i.module == "game.Player"));

    program.start().expect("main runs");
    let published = program.publish().expect("the program publishes");
    assert!(published.iter().any(|i| i.module == "game.Player"));
    assert!(published.iter().any(|i| i.module == "Std"));
    assert!(
        !published
            .iter()
            .any(|i| i.module.starts_with("hl.") || i.module.contains('$'))
    );

    let (iface, index) =
        registry::lookup_class("haxe", "game.Player", "Player").expect("published");
    assert_eq!(iface.lang, haxe);
    let class = &iface.classes[index];
    assert_eq!(class.type_name, "game.Player");
    assert_eq!(class.superclass, None);
    let fields: Vec<(&str, &TypeRef)> = class
        .fields
        .iter()
        .map(|f| (f.name.as_str(), &f.ty))
        .collect();
    assert_eq!(fields, [("hp", &TypeRef::Int), ("name", &TypeRef::Str)]);
    let hit = class.methods.iter().find(|m| m.name == "hit").expect("hit");
    assert!(!hit.is_static);
    assert_eq!(hit.params, [TypeRef::Int]);
    assert_eq!(hit.ret, TypeRef::Bool);
    let spawn = class
        .methods
        .iter()
        .find(|m| m.name == "spawnAt")
        .expect("spawnAt");
    assert!(spawn.is_static);
    assert_eq!(spawn.params, [TypeRef::Float, TypeRef::Float]);
    assert_eq!(spawn.ret, TypeRef::Object("game.Player".into()));
    let ctor = class.ctor.as_ref().expect("a constructor");
    assert_eq!(ctor.params, [TypeRef::Str]);
    assert!(caribou_ash::is_constructor(ctor.target));

    // Construct with a core string, read a field, call a method, set a
    // field, call a static.
    let name = Str::new("ada");
    let _root = heap::handle_new(name as *mut u8);
    let p = caribou_ash::construct(class, &[Str::value(name)]).expect("constructs");
    let _p_root = heap::handle_new(p.as_object().unwrap() as *mut u8);
    assert_eq!(bridge::type_name(p).as_deref(), Some("game.Player"));
    let got = bridge::get(p, intern("name"), haxe).unwrap();
    assert_eq!(unsafe { Str::text(got) }, Some("ada"));
    assert_eq!(bridge::get(p, intern("hp"), haxe), Ok(Value::int(100)));
    assert_eq!(
        bridge::call(hit.target, &[p, Value::int(30)], haxe),
        Ok(Value::bool(false))
    );
    assert_eq!(bridge::get(p, intern("hp"), haxe), Ok(Value::int(70)));
    bridge::set(p, intern("hp"), Value::int(5), haxe).unwrap();
    assert_eq!(
        bridge::call(hit.target, &[p, Value::number(10.0)], haxe),
        Ok(Value::bool(true))
    );
    let q = bridge::call(
        spawn.target,
        &[Value::number(3.0), Value::number(4.0)],
        haxe,
    )
    .unwrap();
    let _q_root = heap::handle_new(q.as_object().unwrap() as *mut u8);
    assert_eq!(bridge::get(q, intern("hp"), haxe), Ok(Value::int(7)));
    let spawned = bridge::get(q, intern("name"), haxe).unwrap();
    assert_eq!(unsafe { Str::text(spawned) }, Some("spawned"));
    let thrown: *mut hl::vdynamic = caribou_ash::unwrap(q).unwrap();
    assert_eq!(unsafe { (*(*thrown).t).kind }, hl::HOBJ);

    // A wrong argument is an error with the callee's frame, not a crash.
    let err = bridge::call_named(hit.target, &[p], haxe, "hit").unwrap_err();
    let e = unsafe { Error::from_value(err) }.expect("an Error value");
    assert_eq!(unsafe { Error::kind(e) }, ErrorKind::Type);
    // A throw inside lands on the trap; back to a Haxe caller it is the
    // thrown String itself, and to anyone else an Error with its text.
    let explode = class
        .methods
        .iter()
        .find(|m| m.name == "explode")
        .expect("explode");
    let thrown = bridge::call(explode.target, &[p], haxe).unwrap_err();
    let thrown: *mut hl::vdynamic = caribou_ash::unwrap(thrown).expect("the String thrown");
    assert_eq!(unsafe { (*(*thrown).t).kind }, hl::HOBJ);
    let err = bridge::call_named(explode.target, &[p], 0, "explode").unwrap_err();
    let e = unsafe { Error::from_value(err) }.expect("an Error value");
    assert_eq!(unsafe { Error::message_str(e) }, "kaboom");
    assert_eq!(unsafe { Error::kind(e) }, ErrorKind::User);
    program.finish();
}
