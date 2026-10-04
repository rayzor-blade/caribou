//! A ZynML module using a plugin in place: the plugin's functions and
//! methods are the program's direct calls, its declared fields loads and
//! stores in the object, and an error the plugin raises reaches the
//! caller.

use std::path::PathBuf;

use caribou::bridge;
use caribou::registry::{self, Namespace};
use caribou::world::{Config, LANG_CORE, World};
use caribou_abi::Value;
use caribou_zyntax::Frontend;

#[test]
fn a_zynml_module_calls_a_plugin_in_place() {
    let library = format!(
        "{}caribou_plugin_math.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    );
    let plugin = caribou_plugin::load(
        &PathBuf::from(env!("CARIBOU_TEST_PLUGINS"))
            .join("plugins/debug")
            .join(library),
    )
    .unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/native");
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
        .register(Box::new(caribou_plugin::Runtime::new(vec![plugin])))
        .unwrap();
    let frontend = Frontend::snapshot(zynml::snapshot_bytes()).expect("the snapshot loads");
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![frontend])))
        .unwrap();

    // What the program is given: Vec2's scalar members and fields bound
    // to the plugin's code and storage.
    let vec2 = caribou_zyntax::host::module("math.Vec2").expect("math.Vec2 is a host module");
    let class = &vec2.classes[0];
    assert!(
        class.word,
        "a program holds a plugin object as its core word"
    );
    for method in ["len", "scale", "dot"] {
        let m = class.methods.iter().find(|m| m.name == method).unwrap();
        assert!(
            m.native.as_ref().is_some_and(|n| n.address != 0),
            "{method} is bound"
        );
    }
    assert!(
        class
            .fields
            .iter()
            .all(|f| f.native.is_some() && f.writable)
    );

    let geometry = registry::lookup_or_load("game", "geometry")
        .unwrap()
        .unwrap();
    let call = |name: &str, args: &[Value]| {
        let f = geometry
            .functions
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("geometry publishes {name}"));
        bridge::call(f.target, args, LANG_CORE)
    };
    let number = |name: &str| {
        call(name, &[])
            .unwrap_or_else(|e| panic!("{name}: {}", bridge::describe(e)))
            .as_number()
    };
    assert_eq!(number("length"), Some(5.0));
    assert_eq!(number("fields"), Some(32.0));
    assert_eq!(number("dots"), Some(11.0));
    let counted = call("counted", &[]).unwrap();
    assert_eq!(caribou::error::Int64::of(counted), Some(55));
    let starred = call("starred", &[]).unwrap();
    assert_eq!(unsafe { caribou::error::Str::text(starred) }, Some("***"));
    assert_eq!(
        call("divided", &[Value::number(9.0), Value::number(2.0)])
            .unwrap()
            .as_number(),
        Some(4.5)
    );
    let refused = call("divided", &[Value::number(1.0), Value::number(0.0)])
        .expect_err("a quotient by zero raises");
    let error = unsafe { caribou::error::Error::from_value(refused) }.expect("an Error");
    assert_eq!(
        unsafe { caribou::error::Error::message_str(error) },
        "quotient by zero"
    );
}
