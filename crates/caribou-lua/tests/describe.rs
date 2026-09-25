//! Lua modules described for a build step, from their chunks' types and
//! their LuaLS annotations: nothing runs, so a module whose top level
//! requires what is not there yet, or raises, is described all the same.

use caribou::describe::MemberKind;
use caribou::registry::TypeRef;
use caribou_zyntax::Frontend;

const CALC: &str = r##"
local Missing = require("nowhere.Missing")
error("describing runs nothing")

local Counter = {}
Counter.__index = Counter
Counter.LIMIT = 10
function Counter.new(start) return setmetatable({ n = start }, Counter) end
function Counter.bump(c) c.n = c.n + 1 return c.n end
function Counter:reset() self.n = 0 end

local M = { Counter = Counter }
---@param a integer
---@param b number
---@return number
function M.add(a, b) return a + b end
function M.sum(...) return select("#", ...) end
M.version = "1.0"
M.missing = Missing
return M
"##;

const SCALE: &str = r#"
local Scale = {}
function Scale.run(device, queue) return device end
return { Scale = Scale }
"#;

#[test]
fn describes_lua_modules_without_running_them() {
    let root = std::env::temp_dir().join(format!("caribou-lua-describe-{}", std::process::id()));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::write(root.join("game/calc.lua"), CALC).unwrap();
    std::fs::write(root.join("scale.lua"), SCALE).unwrap();
    let modules = caribou_zyntax::describe(
        &root,
        vec![Frontend::new(Box::new(caribou_lua::Lua::new()))],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    std::fs::remove_dir_all(&root).ok();

    let calc = modules
        .iter()
        .find(|m| m.module == "game/calc")
        .unwrap_or_else(|| panic!("{modules:?}"));
    assert_eq!(calc.lang, "lua");
    // The module's functions, by their parameters; `...` takes any.
    let functions: Vec<(&str, usize)> = calc
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f.params.len()))
        .collect();
    assert_eq!(functions, [("add", 2), ("sum", 0)]);
    // Typed by the annotations, `Dyn` where there are none.
    let add = &calc.functions[0];
    let types: Vec<&TypeRef> = add.params.iter().map(|p| &p.ty).collect();
    assert_eq!(types, [&TypeRef::Int, &TypeRef::Float]);
    assert_eq!(add.ret, TypeRef::Float);
    assert_eq!(calc.functions[1].ret, TypeRef::Dyn);
    // A table of functions is a class: `new` constructs, a `:` function
    // is a method, any other a static, and the fields of what `new`
    // makes are its instances'; its other fields are statics, and
    // `__index` is the metatable's own.
    let names: Vec<&str> = calc.classes.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Counter"]);
    let counter = &calc.classes[0];
    let members: Vec<(&str, MemberKind, usize)> = counter
        .members
        .iter()
        .map(|m| (m.name.as_str(), m.kind, m.params.len()))
        .collect();
    assert_eq!(
        members,
        [
            ("new", MemberKind::Constructor, 1),
            ("bump", MemberKind::Static, 1),
            ("reset", MemberKind::Method, 0)
        ]
    );
    let fields: Vec<&str> = counter.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(fields, ["n"]);
    let statics: Vec<(&str, &TypeRef)> = counter
        .statics
        .iter()
        .map(|s| (s.name.as_str(), &s.ty))
        .collect();
    assert_eq!(statics, [("LIMIT", &TypeRef::Dyn)]);

    // A file at the root is a module of Lua's own namespace.
    let scale = modules
        .iter()
        .find(|m| m.module == "scale")
        .unwrap_or_else(|| panic!("{modules:?}"));
    assert_eq!(scale.classes.len(), 1);
    assert_eq!(scale.classes[0].name, "Scale");
    assert_eq!(scale.classes[0].members[0].name, "run");
    assert_eq!(scale.classes[0].members[0].params.len(), 2);
}
