//! A Lua module using another language's modules: the math plugin's
//! classes, required as Lua requires a module, constructed, and their
//! methods and statics called. The chunk runs as the module loads, and
//! an error in it is the load's.

use std::path::PathBuf;

use caribou::bridge;
use caribou::registry::{self, Namespace};
use caribou::world::{Config, LANG_CORE, World};
use caribou_zyntax::Frontend;

const PROBE: &str = r#"
local Vec2 = require("math.Vec2").Vec2
local Math = require("math.Math").Math
local v = Vec2(3, 4)
assert(v:len() == 5, "len")
v:scale(2)
assert(v:len() == 10, "scale")
assert(Math.shout("hi") == "HI!", "shout")
assert(Math.hypot(5, 12) == 13, "hypot")
assert(type(v) == "userdata", type(v))
local ok, err = pcall(function() return Math.nothing() end)
assert(not ok and tostring(err):find("nothing"), tostring(err))
Math.bump()
"#;

const SECOND: &str = r#"
local Math = require("math.Math").Math
local function twice(n) return n * 2 end
assert(twice(Math.hypot(3, 4)) == 10, "second")
Math.bump()
"#;

#[test]
fn a_lua_module_uses_a_plugins_classes() {
    let library = format!(
        "{}caribou_plugin_math.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    );
    let plugin = caribou_plugin::load(
        &PathBuf::from(env!("OUT_DIR"))
            .join("plugins/debug")
            .join(library),
    )
    .unwrap();
    let root = std::env::temp_dir().join(format!("caribou-lua-{}", std::process::id()));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::write(root.join("game/probe.lua"), PROBE).unwrap();
    std::fs::write(root.join("game/second.lua"), SECOND).unwrap();
    assert!(caribou_lua::Lua::present_in(&[&root]));

    let world = World::new(Config {
        namespaces: vec![Namespace {
            name: "game".to_owned(),
            langs: vec!["lua".to_owned()],
            modules: None,
        }],
        roots: vec![root.clone()],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_plugin::Runtime::new(vec![plugin])))
        .unwrap();
    let frontend = Frontend::new(Box::new(caribou_lua::Lua::new()));
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![frontend])))
        .unwrap();

    registry::lookup_or_load("game", "probe")
        .unwrap_or_else(|e| panic!("probe: {e}"))
        .expect("probe loads");
    // The chunk ran once, and bumped the plugin's counter.
    let (math, class) = registry::lookup_class_or_load("math", "Math", "Math")
        .unwrap()
        .unwrap();
    let bump = math.classes[class]
        .methods
        .iter()
        .find(|m| m.name == "bump")
        .unwrap();
    assert_eq!(
        bridge::call(bump.target, &[], LANG_CORE).unwrap().as_int(),
        Some(2)
    );

    registry::lookup_or_load("game", "second")
        .unwrap_or_else(|e| panic!("second: {e}"))
        .expect("second loads");
    assert_eq!(
        bridge::call(bump.target, &[], LANG_CORE).unwrap().as_int(),
        Some(4)
    );
    let _ = std::fs::remove_dir_all(&root);
}
