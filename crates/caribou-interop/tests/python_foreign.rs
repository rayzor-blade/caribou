//! A Python module using another language's modules: the math plugin's
//! classes, imported as Python imports a module, constructed, and their
//! methods and statics called, with a plugin object passed in.

use std::path::PathBuf;

use caribou::bridge;
use caribou::registry::{self, Namespace};
use caribou::world::{Config, LANG_CORE, World};
use caribou_abi::Value;
use caribou_zyntax::Frontend;

#[test]
fn a_python_module_uses_a_plugins_classes() {
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
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/src");
    let world = World::new(Config {
        namespaces: vec![Namespace {
            name: "game".to_owned(),
            langs: vec!["python".to_owned()],
            modules: None,
        }],
        roots: vec![root],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_plugin::Runtime::new(vec![plugin])))
        .unwrap();
    let frontend = Frontend::new(Box::new(caribou_python::Python::new()));
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![frontend])))
        .unwrap();

    let reach = registry::lookup_or_load("game", "reach").unwrap().unwrap();
    let call = |name: &str, args: &[Value]| {
        let f = reach
            .functions
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("reach publishes {name}"));
        bridge::call(f.target, args, LANG_CORE)
            .unwrap_or_else(|e| panic!("{name}: {}", bridge::describe(e)))
    };
    let text = |v: Value| unsafe { caribou::error::Str::text(v) }.unwrap().to_owned();

    assert_eq!(
        reach
            .functions
            .iter()
            .find(|function| function.name == "inferred_length")
            .map(|function| &function.ret),
        Some(&caribou::registry::TypeRef::Float)
    );
    assert_eq!(call("length", &[]).as_number(), Some(5.0));
    assert_eq!(call("inferred_length", &[]).as_number(), Some(5.0));
    assert_eq!(
        call("scaled", &[Value::number(2.0)]).as_number(),
        Some(10.0)
    );
    assert_eq!(
        text(call(
            "loud",
            &[caribou::error::Str::value(caribou::error::Str::new("hi"))]
        )),
        "HI!"
    );
    assert_eq!(call("hypot", &[]).as_number(), Some(13.0));

    // A plugin object passed in crosses as a foreign object.
    let (math, vec2) = registry::lookup_class_or_load("math", "Vec2", "Vec2")
        .unwrap()
        .unwrap();
    let ctor = math.classes[vec2].ctor.as_ref().unwrap();
    let v = bridge::call(
        ctor.target,
        &[Value::number(6.0), Value::number(8.0)],
        LANG_CORE,
    )
    .unwrap();
    assert_eq!(call("measure", &[v]).as_number(), Some(10.0));

    let message = text(call("missing", &[]));
    assert!(message.contains("nothing"), "{message}");
}
