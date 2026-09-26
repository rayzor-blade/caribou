//! Lua embedded as a C host embeds it. The runtime's state opens once
//! (`zyntax_lua::open_host`); a module is loaded with Lua's own `load`
//! and run with a protected call through the C API's cores, which return
//! a status rather than jump; and a Lua table or function that crosses to
//! another language is a core object ([`Proxy`]) answering the protocol
//! through protected calls of the helpers in [`HELPERS`].
//!
//! Every value that crosses, and each helper, is kept in the registry
//! under its own address while anything outside holds it: a proxy's
//! death releases its value, counted, at the next crossing into Lua.
//!
//! A Buffer enters Lua as itself, which Lua's string library reads in
//! place; a Lua string that is not text leaves as a read-only Buffer over
//! its own bytes.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, c_int, c_void};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use caribou::bridge;
use caribou::data;
use caribou::error::Error;
use caribou::heap::{self, TypeDesc};
use caribou::protocol::{self, Protocol, REPLY_OK, REPLY_UNSUPPORTED};
use caribou::registry::{TupleField, TypeRef};
use caribou::symbol::{self, Symbol};
use caribou_abi::hl::{self, hl_type, hl_type_detail};
use caribou_abi::mem::{KIND_DYNAMIC, TRACED};
use caribou_abi::{ErrorKind, LangId, Value};
use caribou_zyntax::foreign::{self, Crossing};
use caribou_zyntax::publish::run_type_name;
use caribou_zyntax::zyntax_embed::TieredRuntime;
use caribou_zyntax::zyntax_embed::foreign::{self as zforeign, Any};
use caribou_zyntax::{RunClass, RunFunction, RunModule};
use zyntax_lua::{
    Exported, ExportedFunction, ExportedTable, Exports, LuaType, Returned, is_metafield,
};
use zyntax_lua_capi::api::{
    zlc_getglobal, zlc_gettop, zlc_pushlstring, zlc_pushnil, zlc_rawgeti, zlc_rawsetp, zlc_settop,
    zlc_tolstring, zlc_type,
};
use zyntax_lua_capi::calls::{OK, zlc_pcall};
use zyntax_lua_capi::state::{self, L, REGISTRYINDEX};
use zyntax_lua_capi::values::{self as lua, LUA_TFUNCTION, LUA_TNIL, LUA_TSTRING, LUA_TTABLE};

/// The helpers every crossing calls, protected: index, assign, a method
/// call with the receiver first, the names of a table's functions, of
/// its tables that hold functions (its classes), and of its other
/// fields, a table's function bound to the table as its receiver, and
/// the type names of published classes: one recorded for a class table,
/// and the one a table's metatable has.
const HELPERS: &str = r#"
local classes = setmetatable({}, { __mode = "k" })
return
  function(t, k) return t[k] end,
  function(t, k, v) t[k] = v end,
  function(t, k, ...) return t[k](t, ...) end,
  function(t, kind)
    local function holds_functions(v)
      for _, f in pairs(v) do if type(f) == "function" then return true end end
      return false
    end
    local names = {}
    for k, v in pairs(t) do
      if type(k) == "string" then
        local is
        if type(v) == "function" then is = "function"
        elseif type(v) == "table" and holds_functions(v) then is = "class"
        else is = "value" end
        if is == kind then names[#names + 1] = k end
      end
    end
    table.sort(names)
    return names
  end,
  function(t, k) return function(...) return t[k](t, ...) end end,
  function(t, name) classes[t] = name end,
  function(t)
    local mt = getmetatable(t)
    if mt == nil then return nil end
    return classes[mt]
  end
"#;

struct Host {
    l: L,
    index: Any,
    set: Any,
    send: Any,
    names: Any,
    bind: Any,
    name_class: Any,
    class_name: Any,
}

// The state is the runtime's, which runs on the world's thread; the
// pointers are only read there.
unsafe impl Send for Host {}
unsafe impl Sync for Host {}

static HOST: OnceLock<Host> = OnceLock::new();
static LANG: AtomicU32 = AtomicU32::new(0);

fn host() -> &'static Host {
    HOST.get().expect("the Lua state is open")
}

fn lang() -> LangId {
    LANG.load(Ordering::Relaxed)
}

/// Open the state of `runtime`, once for the process, and load the
/// helpers into it.
pub(crate) fn open(runtime: &mut TieredRuntime) -> Result<(), String> {
    if HOST.get().is_some() {
        return Err("Lua's state is open already: one Lua runtime per process".to_owned());
    }
    let l = zyntax_lua::open_host(runtime)?;
    let helpers = unsafe { run_chunk_n(l, HELPERS, "=caribou", 7)? };
    for &h in &helpers {
        unsafe { keep(l, h) };
    }
    let _ = HOST.set(Host {
        l,
        index: helpers[0],
        set: helpers[1],
        send: helpers[2],
        names: helpers[3],
        bind: helpers[4],
        name_class: helpers[5],
        class_name: helpers[6],
    });
    foreign::add_crossing(&LuaCrossing);
    Ok(())
}

/// The language this state is, once the world assigned it.
pub(crate) fn assign(lang: LangId) {
    LANG.store(lang, Ordering::Relaxed);
}

unsafe fn push(l: L, v: Any) {
    unsafe { state::state(l) }.push(v as lua::Any);
}

unsafe fn at(l: L, idx: c_int) -> Any {
    unsafe { state::state(l) }.value(idx) as Any
}

unsafe fn push_str(l: L, s: &str) {
    unsafe { zlc_pushlstring(l, s.as_ptr().cast(), s.len()) };
}

/// The text of the value at `idx`, as an error reports it.
unsafe fn text_at(l: L, idx: c_int) -> String {
    let p = unsafe { zlc_tolstring(l, idx, std::ptr::null_mut()) };
    if p.is_null() {
        return "(error object is not a string)".to_owned();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// How many holders outside Lua each kept value has, by address.
static KEPT: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);
/// Values whose proxies died, released at the next crossing into Lua:
/// a proxy dies inside a collection, which cannot call into Lua.
static DEAD: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Keep `v` in the registry under its own address, one holder more.
unsafe fn keep(l: L, v: Any) {
    let mut kept = KEPT.lock().unwrap();
    let count = kept
        .get_or_insert_with(HashMap::new)
        .entry(v as usize)
        .or_insert(0);
    *count += 1;
    if *count == 1 {
        unsafe {
            push(l, v);
            zlc_rawsetp(l, REGISTRYINDEX, v as *const c_void);
        }
    }
}

/// Release what dead proxies held: a value with no holder left leaves
/// the registry.
unsafe fn release_dead(l: L) {
    let dead = std::mem::take(&mut *DEAD.lock().unwrap());
    if dead.is_empty() {
        return;
    }
    let mut kept = KEPT.lock().unwrap();
    let kept = kept.get_or_insert_with(HashMap::new);
    for v in dead {
        let Some(count) = kept.get_mut(&v) else {
            continue;
        };
        *count -= 1;
        if *count == 0 {
            kept.remove(&v);
            unsafe {
                zlc_pushnil(l);
                zlc_rawsetp(l, REGISTRYINDEX, v as *const c_void);
            }
        }
    }
}

/// Load `source` as a chunk named `name` and run it, protected: its first
/// `n` results, or the error's text.
unsafe fn run_chunk_n(l: L, source: &str, name: &str, n: usize) -> Result<Vec<Any>, String> {
    unsafe {
        release_dead(l);
        let top = zlc_gettop(l);
        let restore = |r| {
            zlc_settop(l, top);
            r
        };
        let mut ty = 0;
        zlc_getglobal(l, c"load".as_ptr(), &mut ty);
        push_str(l, source);
        push_str(l, name);
        let mut status = 0;
        if zlc_pcall(l, 2, 2, 0, &mut status) != OK || status != 0 {
            return restore(Err(text_at(l, -1)));
        }
        // `load` gives the function, or nil and the message.
        if zlc_type(l, -2) == LUA_TNIL {
            return restore(Err(text_at(l, -1)));
        }
        zlc_settop(l, -2);
        if zlc_pcall(l, 0, n as c_int, 0, &mut status) != OK || status != 0 {
            return restore(Err(text_at(l, -1)));
        }
        let results = (0..n).map(|i| at(l, -(n as c_int) + i as c_int)).collect();
        restore(Ok(results))
    }
}

/// Call `f` with `args`, protected: its first result, or the error's text.
fn pcall(f: Any, args: &[Any]) -> Result<Any, String> {
    let l = host().l;
    unsafe {
        release_dead(l);
        let top = zlc_gettop(l);
        push(l, f);
        for &a in args {
            push(l, a);
        }
        let mut status = 0;
        let r = zlc_pcall(l, args.len() as c_int, 1, 0, &mut status);
        let out = if r != OK {
            Err("the call was abandoned".to_owned())
        } else if status != 0 {
            Err(text_at(l, -1))
        } else {
            Ok(at(l, -1))
        };
        zlc_settop(l, top);
        out
    }
}

/// Call `f` protected, keeping `n` results, nil for any it does not
/// give, as `local a, b = f()` keeps two.
fn pcall_n(f: Any, args: &[Any], n: usize) -> Result<Vec<Any>, String> {
    let l = host().l;
    unsafe {
        release_dead(l);
        let top = zlc_gettop(l);
        push(l, f);
        for &a in args {
            push(l, a);
        }
        let mut status = 0;
        let r = zlc_pcall(l, args.len() as c_int, n as c_int, 0, &mut status);
        let out = if r != OK {
            Err("the call was abandoned".to_owned())
        } else if status != 0 {
            Err(text_at(l, -1))
        } else {
            Ok((0..n as c_int).map(|i| at(l, i - n as c_int)).collect())
        };
        zlc_settop(l, top);
        out
    }
}

/// How many parameters the function `f` names: the most its record's
/// arity word takes, none for a variadic one.
fn params(f: Any) -> Option<usize> {
    if type_of(f) != LUA_TFUNCTION {
        return None;
    }
    // A function value is its record: code, arity word, then its cells.
    let record = unsafe { lua::items(lua::list_of(f as lua::Any)) };
    let word = match unsafe { zforeign::read(*record.get(1)? as Any) } {
        zforeign::Value::Int(word) => word,
        _ => return None,
    };
    Some(if word < 0 {
        0
    } else {
        (word & 0xFFFF) as usize
    })
}

/// A Lua value's type, as `type` names it.
fn type_of(v: Any) -> c_int {
    unsafe { lua::type_of(v as lua::Any) }
}

/// Run module `name`'s chunk and publish the table it returns, as
/// [`module_of`] reads it.
pub(crate) fn run_module(name: &str, source: &str, file: &str) -> Result<RunModule, String> {
    let exports = exports_of(source, file)?;
    let h = host();
    let module = unsafe { run_chunk_n(h.l, source, &format!("@{file}"), 1)? }[0];
    if type_of(module) != LUA_TTABLE {
        return Ok(RunModule {
            functions: Vec::new(),
            classes: Vec::new(),
        });
    }
    unsafe { keep(h.l, module) };
    module_of(name, &exports, Some(module))
}

/// What running module `name`'s chunk publishes, read from its source
/// alone, each value null.
pub(crate) fn describe_module(name: &str, source: &str, file: &str) -> Result<RunModule, String> {
    module_of(name, &exports_of(source, file)?, None)
}

fn exports_of(source: &str, file: &str) -> Result<Exports, String> {
    zyntax_lua::exports(source).map_err(|e| e.render(file, source, false))
}

/// The functions and classes of module `name`, whose chunk exports
/// `exports`: those its types know, as they know them, then, when `live`
/// is the table running the chunk returned, those only running shows. A
/// class is a table holding functions (see [`class_of`]). A value is
/// read from `live`, and is null without it; a name the types know that
/// `live` holds nil under is left out. Metafields are no member.
fn module_of(name: &str, exports: &Exports, live: Option<Any>) -> Result<RunModule, String> {
    let root = exports.table(&exports.value);
    let classes_of = || {
        root.iter()
            .flat_map(|t| &t.fields)
            .filter_map(|(key, value)| {
                Some((key, exports.table(value).filter(|t| holds_functions(t))?))
            })
    };
    // A class goes by its `@class` name in annotations, else by its own.
    let mut types = Types(HashMap::new());
    for (key, class) in classes_of() {
        if let Some(declared) = &class.class {
            types
                .0
                .insert(declared.name.clone(), run_type_name(lang(), name, key));
        }
    }
    for (key, _) in classes_of() {
        types
            .0
            .entry(key.clone())
            .or_insert_with(|| run_type_name(lang(), name, key));
    }
    let mut functions = Vec::new();
    let mut classes = Vec::new();
    let mut known = HashSet::new();
    for (key, value) in root.iter().flat_map(|t| &t.fields) {
        if let Exported::Function(f) = value {
            known.insert(key.as_str());
            functions.extend(function_in(live, key, Some(types.signature(f, false)))?);
        } else if let Some(class) = exports.table(value).filter(|t| holds_functions(t)) {
            known.insert(key.as_str());
            let object = match live {
                Some(t) => match field(t, key)? {
                    Some(object) => Some(object),
                    None => continue,
                },
                None => None,
            };
            classes.push(class_of(key, Some(class), exports, &types, object)?);
        }
    }
    let Some(live) = live else {
        return Ok(RunModule { functions, classes });
    };
    for key in names_of(live, "function")? {
        if !known.contains(key.as_str()) {
            functions.extend(function_in(Some(live), &key, None)?);
        }
    }
    for key in names_of(live, "class")? {
        if !known.contains(key.as_str())
            && let Some(object) = field(live, &key)?
        {
            classes.push(class_of(&key, None, exports, &types, Some(object))?);
        }
    }
    Ok(RunModule { functions, classes })
}

/// The module's classes by the names an annotation may give them, with
/// the type each is published under.
struct Types(HashMap<String, String>);

impl Types {
    /// The type of the core a declared type is. A class of the module is
    /// its published type; a type the core has no counterpart for, or
    /// that may be nil, is `Dyn`.
    fn of(&self, ty: &LuaType) -> TypeRef {
        match ty {
            LuaType::Integer => TypeRef::Int,
            LuaType::Number => TypeRef::Float,
            LuaType::String => TypeRef::Str,
            LuaType::Boolean => TypeRef::Bool,
            LuaType::Nil => TypeRef::Void,
            LuaType::Function => TypeRef::Fun,
            LuaType::Fun { params, returns } => TypeRef::Function {
                params: params.iter().map(|t| self.of(t)).collect(),
                ret: Box::new(returns.first().map_or(TypeRef::Void, |t| self.of(t))),
            },
            LuaType::Named(name) => self
                .0
                .get(name)
                .map_or(TypeRef::Dyn, |t| TypeRef::Object(t.clone())),
            _ => TypeRef::Dyn,
        }
    }

    /// The parameter and result types of `f`, its receiver left out
    /// when `receiver`: as its annotations declare them, `Dyn` where
    /// they do not. A variadic function takes no fixed parameters, as
    /// its arity word says.
    fn signature(&self, f: &ExportedFunction, receiver: bool) -> (Vec<TypeRef>, TypeRef) {
        let skip = usize::from(receiver);
        let count = if f.variadic {
            0
        } else {
            f.params.len().saturating_sub(skip)
        };
        match &f.signature {
            Some(sig) => (
                sig.params
                    .iter()
                    .skip(skip)
                    .take(count)
                    .map(|t| self.of(t))
                    .collect(),
                self.results(&sig.returns),
            ),
            None => (vec![TypeRef::Dyn; count], TypeRef::Dyn),
        }
    }

    /// The type of what a call gives: its one result, or, for several,
    /// a tuple of them, each under its `@return` name, else `_1`, `_2`,
    /// by its place. `Dyn` when none is declared.
    fn results(&self, returns: &[Returned]) -> TypeRef {
        match returns {
            [] => TypeRef::Dyn,
            [one] => self.of(&one.ty),
            several => TypeRef::Tuple(
                several
                    .iter()
                    .enumerate()
                    .map(|(i, r)| TupleField {
                        name: r.name.clone().unwrap_or_else(|| format!("_{}", i + 1)),
                        ty: self.of(&r.ty),
                    })
                    .collect(),
            ),
        }
    }
}

/// Class `name`: the fields its types know, `known`, then those only
/// its running table `live` shows. `new` is its constructor, bound to
/// the class when declared with `:`; any other method (a function taking
/// `self` first) is called on an instance, any other function on the
/// class, and any other field is a static. Its instances' fields are
/// the `@field`s its `@class` declares, then the fields of the tables it
/// is the metatable of.
fn class_of(
    name: &str,
    known: Option<&ExportedTable>,
    exports: &Exports,
    types: &Types,
    live: Option<Any>,
) -> Result<RunClass, String> {
    let mut functions = Vec::new();
    let mut methods = Vec::new();
    let mut statics = Vec::new();
    let mut ctor = None;
    let mut seen = HashSet::new();
    for (key, value) in known.iter().flat_map(|t| &t.fields) {
        seen.insert(key.as_str());
        let f = match value {
            Exported::Function(f) => f,
            Exported::Value(Some(ty)) => {
                statics.push((key.clone(), types.of(ty)));
                continue;
            }
            _ => {
                statics.push((key.clone(), TypeRef::Dyn));
                continue;
            }
        };
        let signature = types.signature(f, f.method);
        if key == "new" {
            ctor = if f.method {
                bound_in(live, key, signature)?
            } else {
                function_in(live, key, Some(signature))?
            };
        } else if f.method {
            methods.extend(function_in(live, key, Some(signature))?);
        } else {
            functions.extend(function_in(live, key, Some(signature))?);
        }
    }
    if let Some(t) = live {
        for key in names_of(t, "function")? {
            if seen.contains(key.as_str()) {
                continue;
            }
            let f = function_in(live, &key, None)?;
            if key == "new" {
                ctor = f;
            } else {
                functions.extend(f);
            }
        }
        statics.extend(
            names_of(t, "value")?
                .into_iter()
                .filter(|key| !seen.contains(key.as_str()))
                .map(|key| (key, TypeRef::Dyn)),
        );
    }
    let is_method = |key: &str| methods.iter().any(|m: &RunFunction| m.name == key);
    let mut fields: Vec<(String, TypeRef)> = Vec::new();
    for (key, ty) in known
        .iter()
        .flat_map(|t| t.class.iter())
        .flat_map(|c| &c.fields)
    {
        if !is_method(key) {
            fields.push((key.clone(), types.of(ty)));
        }
    }
    for &i in known.iter().flat_map(|t| &t.instances) {
        for (key, _) in &exports.tables[i].fields {
            if !fields.iter().any(|(f, _)| f == key) && !is_method(key) {
                fields.push((key.clone(), TypeRef::Dyn));
            }
        }
    }
    Ok(RunClass {
        name: name.to_owned(),
        object: live.map_or(Value::null(), |t| rooted(proxy(t))),
        statics,
        fields,
        functions,
        methods,
        ctor,
    })
}

/// Function `name` of the table `live`, or of a module described when
/// there is none, with the types its annotations declare, `declared`;
/// without them, as many `Dyn` parameters as its arity word counts.
/// `None` when `live` holds nil under the name.
fn function_in(
    live: Option<Any>,
    name: &str,
    declared: Option<(Vec<TypeRef>, TypeRef)>,
) -> Result<Option<RunFunction>, String> {
    let (value, (params, ret)) = match live {
        Some(t) => {
            let Some(f) = field(t, name)? else {
                return Ok(None);
            };
            let signature = declared
                .unwrap_or_else(|| (vec![TypeRef::Dyn; params(f).unwrap_or(0)], TypeRef::Dyn));
            let value = match &signature.1 {
                TypeRef::Tuple(fields) => {
                    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
                    results_proxy(f, data::tuple_names(&names))
                }
                _ => proxy(f),
            };
            (rooted(value), signature)
        }
        None => (
            Value::null(),
            declared.unwrap_or((Vec::new(), TypeRef::Dyn)),
        ),
    };
    Ok(Some(RunFunction {
        name: name.to_owned(),
        params,
        ret,
        value,
    }))
}

/// Method `name` of the table `live` bound to the table as its receiver,
/// with the types `(params, ret)`; as [`function_in`] otherwise.
fn bound_in(
    live: Option<Any>,
    name: &str,
    (params, ret): (Vec<TypeRef>, TypeRef),
) -> Result<Option<RunFunction>, String> {
    let value = match live {
        Some(t) => {
            if field(t, name)?.is_none() {
                return Ok(None);
            }
            rooted(proxy(pcall(host().bind, &[t, zforeign::string(name)])?))
        }
        None => Value::null(),
    };
    Ok(Some(RunFunction {
        name: name.to_owned(),
        params,
        ret,
        value,
    }))
}

fn holds_functions(t: &ExportedTable) -> bool {
    t.fields
        .iter()
        .any(|(_, v)| matches!(v, Exported::Function(_)))
}

/// Field `name` of the table `t`, or `None` when it holds nil.
fn field(t: Any, name: &str) -> Result<Option<Any>, String> {
    let v = pcall(host().index, &[t, zforeign::string(name)])?;
    Ok((type_of(v) != LUA_TNIL).then_some(v))
}

/// The names of `t`'s fields of `kind`: `function`, `class` or `value`,
/// metafields left out.
fn names_of(t: Any, kind: &str) -> Result<Vec<String>, String> {
    let names = pcall(host().names, &[t, zforeign::string(kind)])?;
    let mut names = unsafe { strings(host().l, names) };
    names.retain(|name| !is_metafield(name));
    Ok(names)
}

/// `v`, rooted for as long as the module is published.
fn rooted(v: Value) -> Value {
    let _ = heap::handle_new(v.as_object().expect("a proxy") as *mut u8);
    v
}

/// The strings of the sequence `t`.
unsafe fn strings(l: L, t: Any) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let top = zlc_gettop(l);
        push(l, t);
        for i in 1.. {
            let mut ty = 0;
            zlc_rawgeti(l, -1, i, &mut ty);
            if ty == LUA_TNIL {
                break;
            }
            out.push(text_at(l, -1));
            zlc_settop(l, -2);
        }
        zlc_settop(l, top);
    }
    out
}

/// A Lua value held by another language: the value, kept in the registry.
#[repr(C)]
struct Proxy {
    desc: *const TypeDesc,
    value: Any,
    /// The type name it reports, once asked.
    type_name: Option<Symbol>,
    /// For a function whose declared results are several, their names: a
    /// call keeps that many and gives them as one tuple.
    results: Option<&'static [Symbol]>,
}

fn descriptor() -> &'static TypeDesc {
    static DESC: OnceLock<&'static TypeDesc> = OnceLock::new();
    DESC.get_or_init(|| {
        let name = "lua.value";
        let mut d = TypeDesc::new(hl_type {
            kind: hl::HABSTRACT,
            detail: hl_type_detail {
                abs_name: std::ptr::null(),
            },
            vobj_proto: std::ptr::null_mut(),
            mark_bits: std::ptr::null_mut(),
        });
        // What the proxy holds is the program's, not the core heap's.
        d.trace = Some(trace_nothing);
        d.drop = Some(drop_proxy);
        d.protocol = &PROXY_PROTO;
        d.name = name.as_ptr();
        d.name_len = name.len();
        d.lang = lang();
        Box::leak(Box::new(d))
    })
}

unsafe extern "C" fn trace_nothing(_obj: *mut u8, _tracer: *mut heap::Tracer) {}

/// A proxy died: its value is released at the next crossing into Lua.
unsafe extern "C" fn drop_proxy(obj: *mut u8) {
    let v = unsafe { value_of_proxy(obj) } as usize;
    if let Ok(mut dead) = DEAD.lock() {
        dead.push(v);
    }
}

/// `v` as a core object other languages hold: unrooted, for the caller
/// to hand on.
fn proxy(v: Any) -> Value {
    unsafe { keep(host().l, v) };
    let p = unsafe {
        heap::alloc_gen(
            descriptor() as *const TypeDesc as *mut hl_type,
            size_of::<Proxy>(),
            KIND_DYNAMIC | TRACED,
        )
    } as *mut Proxy;
    if p.is_null() {
        heap::out_of_memory("a Lua value");
    }
    unsafe {
        (*p).value = v;
        (*p).type_name = None;
        (*p).results = None;
    }
    Value::object(p as *const c_void)
}

/// The function `f` as a core object whose call keeps the results
/// `names` names and gives them as one tuple.
fn results_proxy(f: Any, names: &'static [Symbol]) -> Value {
    let v = proxy(f);
    let p = v.as_object().expect("a proxy") as *mut Proxy;
    unsafe { (*p).results = Some(names) };
    v
}

unsafe fn value_of_proxy(obj: *mut u8) -> Any {
    unsafe { (*(obj as *const Proxy)).value }
}

/// The reply for a crossing into Lua: its result as a value of the core,
/// or its error raised.
unsafe fn reply(out: *mut Value, f: impl FnOnce() -> Result<Any, String>) -> u8 {
    foreign::as_caller(lang(), || match f() {
        Ok(any) => match unsafe { foreign::value_of(any) } {
            Ok((v, _)) => {
                unsafe { *out = v };
                REPLY_OK
            }
            Err(e) => bridge::raise(Error::new(ErrorKind::Type, &e.message, lang())),
        },
        Err(m) => bridge::raise(Error::new(ErrorKind::Runtime, &m, lang())),
    })
}

fn anys(args: *const Value, n: usize) -> Vec<Any> {
    if n == 0 {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(args, n) }
        .iter()
        .map(|&v| foreign::any_of(v))
        .collect()
}

unsafe extern "C-unwind" fn proxy_call(
    obj: *mut u8,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let proxy = obj as *const Proxy;
    let f = unsafe { (*proxy).value };
    match unsafe { (*proxy).results } {
        Some(names) => unsafe {
            reply_tuple(out, names, || pcall_n(f, &anys(args, n), names.len()))
        },
        None => unsafe { reply(out, || pcall(f, &anys(args, n))) },
    }
}

/// The reply for a call that keeps several results: one tuple of them,
/// each under its name.
unsafe fn reply_tuple(
    out: *mut Value,
    names: &'static [Symbol],
    f: impl FnOnce() -> Result<Vec<Any>, String>,
) -> u8 {
    foreign::as_caller(lang(), || match f() {
        Ok(anys) => {
            // Each value is rooted until the tuple holds it: making the
            // next may collect.
            let mut values = Vec::with_capacity(anys.len());
            let mut handles = Vec::new();
            let mut failed = None;
            for any in anys {
                match unsafe { foreign::value_of(any) } {
                    Ok((v, _)) => {
                        if let Some(p) = v.as_object().filter(|p| !p.is_null()) {
                            handles.push(heap::handle_new(p as *mut u8));
                        }
                        values.push(v);
                    }
                    Err(e) => {
                        failed = Some(e.message);
                        break;
                    }
                }
            }
            let reply = match failed {
                None => {
                    unsafe { *out = Value::object(data::tuple_new(names, &values).cast()) };
                    REPLY_OK
                }
                Some(m) => bridge::raise(Error::new(ErrorKind::Type, &m, lang())),
            };
            for h in handles {
                heap::handle_release(h);
            }
            reply
        }
        Err(m) => bridge::raise(Error::new(ErrorKind::Runtime, &m, lang())),
    })
}

unsafe extern "C-unwind" fn proxy_get(obj: *mut u8, name: Symbol, out: *mut Value) -> u8 {
    let t = unsafe { value_of_proxy(obj) };
    unsafe {
        reply(out, || {
            pcall(host().index, &[t, zforeign::string(name.name())])
        })
    }
}

unsafe extern "C-unwind" fn proxy_set(obj: *mut u8, name: Symbol, value: Value) -> u8 {
    let t = unsafe { value_of_proxy(obj) };
    let mut ignored = Value::null();
    unsafe {
        reply(&mut ignored, || {
            pcall(
                host().set,
                &[t, zforeign::string(name.name()), foreign::any_of(value)],
            )
        })
    }
}

unsafe extern "C-unwind" fn proxy_invoke(
    obj: *mut u8,
    name: Symbol,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let t = unsafe { value_of_proxy(obj) };
    let mut all = vec![t, zforeign::string(name.name())];
    all.extend(anys(args, n));
    unsafe { reply(out, || pcall(host().send, &all)) }
}

unsafe extern "C-unwind" fn proxy_arity(obj: *mut u8, out: *mut usize) -> u8 {
    match params(unsafe { value_of_proxy(obj) }) {
        Some(n) => {
            unsafe { *out = n };
            REPLY_OK
        }
        None => REPLY_UNSUPPORTED,
    }
}

unsafe extern "C-unwind" fn proxy_type_name(obj: *mut u8, out: *mut Symbol) -> u8 {
    let proxy = obj as *mut Proxy;
    if let Some(name) = unsafe { (*proxy).type_name } {
        unsafe { *out = name };
        return REPLY_OK;
    }
    let v = unsafe { (*proxy).value };
    let name = match type_of(v) {
        LUA_TTABLE => class_name(v).unwrap_or_else(|| symbol::intern("lua.table")),
        LUA_TFUNCTION => symbol::intern("lua.function"),
        _ => symbol::intern("lua.userdata"),
    };
    unsafe {
        (*proxy).type_name = Some(name);
        *out = name;
    }
    REPLY_OK
}

/// The type name of the published class whose instance `t` is: the class
/// its metatable is.
fn class_name(t: Any) -> Option<Symbol> {
    let name = pcall(host().class_name, &[t]).ok()?;
    if type_of(name) != LUA_TSTRING {
        return None;
    }
    let bytes = unsafe { lua::bytes_of(lua::string_of(name as lua::Any)) };
    Some(symbol::intern(std::str::from_utf8(bytes).ok()?))
}

/// Record the type names the classes of `iface` were published under,
/// for their instances to report.
pub(crate) fn published(iface: &caribou::registry::Interface) -> Result<(), String> {
    for class in &iface.classes {
        let Some(object) = class.class_object.as_object() else {
            continue;
        };
        if object.is_null() {
            continue;
        }
        let t = unsafe { value_of_proxy(object as *mut u8) };
        pcall(host().name_class, &[t, zforeign::string(&class.type_name)])?;
    }
    Ok(())
}

static PROXY_PROTO: Protocol = Protocol {
    get_member: Some(proxy_get),
    set_member: Some(proxy_set),
    invoke: Some(proxy_invoke),
    call: Some(proxy_call),
    arity: Some(proxy_arity),
    type_name: Some(proxy_type_name),
    ..Protocol::NONE
};

/// Lua's tables and functions cross as proxies, and come back as
/// themselves. A string that is not text is Lua's bytes, which cross as
/// a buffer of them; a buffer comes into Lua as a string of its bytes.
struct LuaCrossing;

impl Crossing for LuaCrossing {
    fn value_of(&self, any: Any) -> Option<Value> {
        // Only a value of the Lua code running now is Lua's: a function
        // value of another Zyntax language is laid out the same way.
        if foreign::caller() != lang() {
            return None;
        }
        match type_of(any) {
            LUA_TTABLE | LUA_TFUNCTION => Some(proxy(any)),
            // Bytes that are not text: a read-only buffer over the string
            // itself, which its proxy keeps alive.
            LUA_TSTRING => {
                let bytes = unsafe { lua::bytes_of(lua::string_of(any as lua::Any)) };
                let owner = proxy(any);
                let _held = heap::handle_new(owner.as_object()? as *mut u8);
                let view = unsafe {
                    data::buffer_view(owner.as_object()? as *mut u8, bytes.as_ptr(), bytes.len())
                };
                heap::handle_release(_held);
                Some(Value::object(view as *const c_void))
            }
            _ => None,
        }
    }

    fn own(&self, v: Value) -> Option<Any> {
        let p = v.as_object()?;
        if p.is_null() || !std::ptr::eq(unsafe { protocol::desc_of(p as *const u8) }, descriptor())
        {
            return None;
        }
        Some(unsafe { value_of_proxy(p as *mut u8) })
    }
}
