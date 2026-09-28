//! A Lua module used by another language: the table its chunk returns
//! publishes its functions, called with values of the core; a Lua table
//! or function that comes back is a core object that answers as Lua
//! would, and a plugin object that goes in and back is itself.

use std::path::PathBuf;

use caribou::bridge;
use caribou::protocol::Callable;
use caribou::registry::{self, Namespace, TypeRef};
use caribou::symbol::intern;
use caribou::world::{Config, LANG_CORE, World};
use caribou_abi::Value;
use caribou_zyntax::Frontend;

const CALC: &str = r#"
local Vec2 = require("math.Vec2").Vec2
local M = {}
function M.add(a, b) return a + b end
function M.length(v) return v:len() end
function M.make(x, y) return Vec2(x, y) end
function M.point(x, y)
  return { x = x, y = y, sum = function(self) return self.x + self.y end }
end
function M.adder(n) return function(m) return n + m end end
function M.fail() error("no luck") end
function M.packed() return string.pack("<f", 1.5) end
function M.unpacked(bytes) return string.unpack("<f", bytes) end
function M.same(x) return x end
function M.fill_string()
  local Data = require("math.Data").Data
  return pcall(Data.fill, string.pack("<f", 1.5), 7)
end
-- A table of functions is a class: `new` constructs, the rest are statics.
M.Counter = {
  LIMIT = 10,
  new = function(start) return { n = start } end,
  bump = function(c) c.n = c.n + 1 return c.n end,
}
return M
"#;

const BROKEN: &str = r#"
local Math = require("math.Math").Math
assert(Math.hypot(3, 4) == 6, "hypot is not 6")
"#;

#[test]
fn another_language_calls_a_lua_module() {
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
    let root = std::env::temp_dir().join(format!("caribou-lua-module-{}", std::process::id()));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::write(root.join("game/calc.lua"), CALC).unwrap();
    std::fs::write(root.join("game/broken.lua"), BROKEN).unwrap();
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
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![Frontend::new(
            Box::new(caribou_lua::Lua::new()),
        )])))
        .unwrap();

    let calc = registry::lookup_or_load("game", "calc")
        .unwrap_or_else(|e| panic!("calc: {e}"))
        .expect("calc loads");
    // In the order the chunk declares them.
    let names: Vec<&str> = calc.functions.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "add",
            "length",
            "make",
            "point",
            "adder",
            "fail",
            "packed",
            "unpacked",
            "same",
            "fill_string"
        ]
    );
    let function = |name: &str| calc.functions.iter().find(|f| f.name == name).unwrap();
    assert_eq!(function("add").params, [TypeRef::Dyn, TypeRef::Dyn]);
    let call = |name: &str, args: &[Value]| bridge::call(function(name).target, args, LANG_CORE);

    assert_eq!(
        call("add", &[Value::int(2), Value::int(3)])
            .unwrap()
            .as_int(),
        Some(5)
    );

    // A plugin object goes in as a foreign object, and comes back as itself.
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
    assert_eq!(call("length", &[v]).unwrap().as_number(), Some(10.0));
    let made = call("make", &[Value::int(3), Value::int(4)]).unwrap();
    assert_eq!(bridge::type_name(made).as_deref(), Some("math.Vec2"));

    // A Lua table answers as Lua would: a field, and a method with itself first.
    let point = call("point", &[Value::int(1), Value::int(2)]).unwrap();
    assert_eq!(bridge::type_name(point).as_deref(), Some("lua.table"));
    assert_eq!(
        bridge::get(point, intern("y"), LANG_CORE).unwrap().as_int(),
        Some(2)
    );
    assert_eq!(
        bridge::invoke(point, intern("sum"), &[], LANG_CORE)
            .unwrap()
            .as_int(),
        Some(3)
    );

    // A Lua closure is a callable value.
    let adder = call("adder", &[Value::int(10)]).unwrap();
    assert_eq!(bridge::arity(adder), Some(1));
    assert_eq!(
        bridge::call(Callable::Dynamic(adder), &[Value::int(5)], LANG_CORE)
            .unwrap()
            .as_int(),
        Some(15)
    );

    let error = call("fail", &[]).unwrap_err();
    let message = unsafe {
        caribou::error::Error::from_value(error)
            .map(|e| caribou::error::Error::message_str(e).to_owned())
    }
    .unwrap_or_else(|| bridge::describe(error));
    assert!(message.contains("no luck"), "{message}");

    // A Lua string that is not text leaves as a read-only buffer over its
    // own bytes; a buffer enters Lua as itself, read there in place, and
    // comes back as the same object.
    let packed = call("packed", &[]).unwrap();
    assert!(caribou::data::is_read_only(packed));
    assert_eq!(call("unpacked", &[packed]).unwrap().as_number(), Some(1.5));
    let buffer = Value::object(caribou::data::buffer_new(&1.5f32.to_le_bytes()).cast());
    assert_eq!(call("unpacked", &[buffer]).unwrap().as_number(), Some(1.5));
    assert_eq!(call("same", &[buffer]).unwrap(), buffer);
    // A plugin that writes its buffer refuses a Lua string's.
    let refused = call("fill_string", &[]).unwrap();
    assert_eq!(
        refused.as_bool(),
        Some(false),
        "{}",
        bridge::describe(refused)
    );

    // A table of functions is a class.
    let counter = &calc.classes[0];
    assert_eq!(counter.name, "Counter");
    assert_eq!(counter.statics[0].name, "LIMIT");
    let bump = &counter.methods[0];
    assert_eq!((bump.name.as_str(), bump.is_static), ("bump", true));
    let made = bridge::call(
        counter.ctor.as_ref().unwrap().target,
        &[Value::int(4)],
        LANG_CORE,
    )
    .unwrap();
    assert_eq!(
        bridge::call(bump.target, &[made], LANG_CORE)
            .unwrap()
            .as_int(),
        Some(5)
    );
    assert_eq!(
        bridge::get(counter.class_object, intern("LIMIT"), LANG_CORE)
            .unwrap()
            .as_int(),
        Some(10)
    );

    // An error in a module's chunk is the load's.
    let error = registry::lookup_or_load("game", "broken").unwrap_err();
    assert!(error.contains("hypot is not 6"), "{error}");
    let _ = std::fs::remove_dir_all(&root);
}
