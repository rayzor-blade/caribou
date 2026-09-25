//! Lua embedded as a C host embeds it. The runtime's state opens once
//! (`zyntax_lua::open_host`); a module is loaded with Lua's own `load`
//! and run with a protected call through the C API's cores, which return
//! a status rather than jump; and a Lua table or function that crosses to
//! another language is a core object ([`Proxy`]) answering the protocol
//! through protected calls of the helpers in [`HELPERS`].
//!
//! Every value that crosses, and each helper, is kept in the registry
//! under its own address, so nothing the program releases is still held
//! outside it.

use std::ffi::{CStr, c_int, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use caribou::bridge;
use caribou::error::Error;
use caribou::heap::{self, TypeDesc};
use caribou::protocol::{self, Protocol, REPLY_OK, REPLY_UNSUPPORTED};
use caribou::symbol::{self, Symbol};
use caribou_abi::hl::{self, hl_type, hl_type_detail};
use caribou_abi::mem::{KIND_DYNAMIC, TRACED};
use caribou_abi::{ErrorKind, LangId, Value};
use caribou_zyntax::foreign::{self, Crossing};
use caribou_zyntax::zyntax_embed::TieredRuntime;
use caribou_zyntax::zyntax_embed::foreign::{self as zforeign, Any};
use caribou_zyntax::{RunFunction, RunModule};
use zyntax_lua_capi::api::{
    zlc_getglobal, zlc_gettop, zlc_pushlstring, zlc_rawgeti, zlc_rawsetp, zlc_settop,
    zlc_tolstring, zlc_type,
};
use zyntax_lua_capi::calls::{OK, zlc_pcall};
use zyntax_lua_capi::state::{self, L, REGISTRYINDEX};
use zyntax_lua_capi::values::{self as lua, LUA_TFUNCTION, LUA_TNIL, LUA_TTABLE};

/// The helpers every crossing calls, protected: index, assign, a method
/// call with the receiver first, and a module table's function names.
const HELPERS: &str = r#"
return
  function(t, k) return t[k] end,
  function(t, k, v) t[k] = v end,
  function(t, k, ...) return t[k](t, ...) end,
  function(m)
    local names = {}
    for k, v in pairs(m) do
      if type(k) == "string" and type(v) == "function" then names[#names + 1] = k end
    end
    table.sort(names)
    return names
  end
"#;

struct Host {
    l: L,
    index: Any,
    set: Any,
    send: Any,
    functions: Any,
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
    let helpers = unsafe { run_chunk_n(l, HELPERS, "=caribou", 4)? };
    for &h in &helpers {
        unsafe { keep(l, h) };
    }
    let _ = HOST.set(Host {
        l,
        index: helpers[0],
        set: helpers[1],
        send: helpers[2],
        functions: helpers[3],
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

/// Keep `v` in the registry under its own address.
unsafe fn keep(l: L, v: Any) {
    unsafe {
        push(l, v);
        zlc_rawsetp(l, REGISTRYINDEX, v as *const c_void);
    }
}

/// Load `source` as a chunk named `name` and run it, protected: its first
/// `n` results, or the error's text.
unsafe fn run_chunk_n(l: L, source: &str, name: &str, n: usize) -> Result<Vec<Any>, String> {
    unsafe {
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

/// Run a module's chunk: the functions of the table it returns, each a
/// [`Proxy`] of the function.
pub(crate) fn run_module(source: &str, file: &str) -> Result<RunModule, String> {
    let h = host();
    let module = unsafe { run_chunk_n(h.l, source, &format!("@{file}"), 1)? }[0];
    if type_of(module) != LUA_TTABLE {
        return Ok(RunModule {
            functions: Vec::new(),
        });
    }
    unsafe { keep(h.l, module) };
    let names = pcall(h.functions, &[module])?;
    let mut functions = Vec::new();
    for name in unsafe { strings(h.l, names) } {
        let f = pcall(h.index, &[module, zforeign::string(&name)])?;
        let params = params(f).unwrap_or(0);
        let value = proxy(f);
        // Rooted for as long as the module is published.
        let _ = heap::handle_new(value.as_object().expect("a proxy") as *mut u8);
        functions.push(RunFunction {
            name,
            params,
            value,
        });
    }
    Ok(RunModule { functions })
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
        d.protocol = &PROXY_PROTO;
        d.name = name.as_ptr();
        d.name_len = name.len();
        d.lang = lang();
        Box::leak(Box::new(d))
    })
}

unsafe extern "C" fn trace_nothing(_obj: *mut u8, _tracer: *mut heap::Tracer) {}

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
    unsafe { (*p).value = v };
    Value::object(p as *const c_void)
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
    let f = unsafe { value_of_proxy(obj) };
    unsafe { reply(out, || pcall(f, &anys(args, n))) }
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
    let name = match type_of(unsafe { value_of_proxy(obj) }) {
        LUA_TTABLE => "lua.table",
        LUA_TFUNCTION => "lua.function",
        _ => "lua.userdata",
    };
    unsafe { *out = symbol::intern(name) };
    REPLY_OK
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
/// themselves.
struct LuaCrossing;

impl Crossing for LuaCrossing {
    fn value_of(&self, any: Any) -> Option<Value> {
        // Only a value of the Lua code running now is Lua's: a function
        // value of another Zyntax language is laid out the same way.
        if foreign::caller() != lang() {
            return None;
        }
        matches!(type_of(any), LUA_TTABLE | LUA_TFUNCTION).then(|| proxy(any))
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
