//! Native plugins on the shared ABI (`caribou_abi`): a dynamic library
//! exporting `caribou_abi_version` and `caribou_plugin_entry`, whose table
//! names its functions, the class each hangs in, and their signatures by
//! `TypeTag`.
//!
//! A plugin is a language of its own to the world: `Runtime` registers one
//! `LangId` per plugin, named after it, so every plugin has a namespace
//! with nothing configured, and `import "math:Math" for Math` reaches it
//! as it reaches any language's class. Its table publishes to the registry
//! as one module per class, named after the class, whose methods are the
//! class's symbols; free functions are the statics of a class named after
//! the plugin. Every target is `Callable::Typed` with an `hl_type` built
//! from the tags, and the plugin's language dispatches such a call over
//! `ash_native_call`: scalars by kind, a `DYN` as the `Value` it is, and
//! nothing boxed.
//!
//! What a spoke sees of a plugin is what the driver loaded: no plugin is
//! reached by a path from inside a language.

use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

use caribou::bridge;
use caribou::error::{Error as CoreError, Int64};
use caribou::protocol::{CallSite, Callable, REPLY_OK};
use caribou::registry::{self, ClassIface, Interface, MethodIface, TypeRef};
use caribou::world::Adapter;
use caribou_abi::hl::{
    self, hl_type, hl_type_detail, hl_type_fun, hl_type_fun_closure, hl_type_fun_closure_type,
};
use caribou_abi::{
    ABI_VERSION, ABI_VERSION_SYMBOL, ErrorKind, LangId, PLUGIN_ENTRY_SYMBOL, PluginInfo,
    SymbolDesc, TypeTag, Value, sym,
};

/// A loaded plugin: its library stays open for the process, since the
/// registry holds its function addresses.
pub struct Plugin {
    name: String,
    path: PathBuf,
    symbols: Vec<SymbolDesc>,
    _library: libloading::Library,
}

impl Plugin {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn symbols(&self) -> &[SymbolDesc] {
        &self.symbols
    }
}

#[derive(Debug)]
pub enum Error {
    /// The library did not open, or exports no plugin entry.
    NotAPlugin(PathBuf, String),
    /// Built against another ABI version than this core's.
    AbiVersion {
        path: PathBuf,
        plugin: u32,
        core: u32,
    },
    /// The entry answered nothing.
    NoTable(PathBuf),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotAPlugin(path, why) => write!(f, "{} is not a plugin: {why}", path.display()),
            Error::AbiVersion { path, plugin, core } => write!(
                f,
                "{} is built against ABI version {plugin}; this core is {core}",
                path.display()
            ),
            Error::NoTable(path) => write!(f, "{} answered no table", path.display()),
        }
    }
}

impl std::error::Error for Error {}

/// Open the plugin at `path`: refused unless it exports the two symbols
/// and was built against this ABI version.
pub fn load(path: &Path) -> Result<Plugin, Error> {
    // SAFETY: a plugin's initialisers are its own to run; nothing else is
    // called before its version is checked.
    let library = unsafe { libloading::Library::new(path) }
        .map_err(|e| Error::NotAPlugin(path.to_owned(), e.to_string()))?;
    let version: libloading::Symbol<unsafe extern "C" fn() -> u32> =
        unsafe { library.get(ABI_VERSION_SYMBOL.as_bytes()) }
            .map_err(|e| Error::NotAPlugin(path.to_owned(), e.to_string()))?;
    let plugin = unsafe { version() };
    if plugin != ABI_VERSION {
        return Err(Error::AbiVersion {
            path: path.to_owned(),
            plugin,
            core: ABI_VERSION,
        });
    }
    let entry: libloading::Symbol<unsafe extern "C" fn() -> *const PluginInfo> =
        unsafe { library.get(PLUGIN_ENTRY_SYMBOL.as_bytes()) }
            .map_err(|e| Error::NotAPlugin(path.to_owned(), e.to_string()))?;
    let info = unsafe { entry() };
    if info.is_null() {
        return Err(Error::NoTable(path.to_owned()));
    }
    let info = unsafe { &*info };
    let symbols = if info.symbol_count == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(info.symbols, info.symbol_count) }.to_vec()
    };
    Ok(Plugin {
        name: unsafe { info.name.as_str() }.to_owned(),
        path: path.to_owned(),
        symbols,
        _library: library,
    })
}

/// Open every plugin in `dir`, in name order: each file with the
/// platform's library extension that exports the entry. A library that
/// is not a plugin is passed over; one built against another ABI is an
/// error, since the program named it.
pub fn load_dir(dir: &Path) -> Result<Vec<Plugin>, Error> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e == std::env::consts::DLL_EXTENSION)
        })
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        match load(&path) {
            Ok(plugin) => out.push(plugin),
            Err(Error::NotAPlugin(..)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// The adapter: every loaded plugin as a language of the world.
pub struct Runtime {
    plugins: Vec<Plugin>,
}

impl Runtime {
    pub fn new(plugins: Vec<Plugin>) -> Runtime {
        Runtime { plugins }
    }

    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }
}

impl Adapter for Runtime {
    fn languages(&self) -> Vec<String> {
        self.plugins.iter().map(|p| p.name.clone()).collect()
    }

    /// Publish each plugin's table under its language, and give the
    /// language its dispatcher.
    fn assign_languages(&mut self, ids: &[LangId]) {
        for (plugin, &lang) in self.plugins.iter().zip(ids) {
            bridge::set_typed_dispatch(lang, dispatch);
            for iface in interfaces(plugin, lang) {
                if let Err(e) = registry::publish(iface) {
                    eprintln!("caribou: plugin {}: {e}", plugin.name);
                }
            }
        }
    }
}

/// The plugin's table as interfaces: one module per class, and the free
/// functions on a class named after the plugin.
fn interfaces(plugin: &Plugin, lang: LangId) -> Vec<Interface> {
    let mut by_class: Vec<(String, Vec<MethodIface>)> = Vec::new();
    for desc in &plugin.symbols {
        let class = unsafe { desc.class.as_str() };
        let class = if class.is_empty() {
            capitalised(&plugin.name)
        } else {
            class.to_owned()
        };
        let params: &[TypeTag] = &desc.params[..desc.param_count as usize];
        let method = MethodIface {
            name: unsafe { desc.method.as_str() }.to_owned(),
            is_static: desc.flags & sym::STATIC != 0,
            params: params.iter().map(|&t| type_ref(t)).collect(),
            ret: type_ref(desc.ret),
            target: Callable::Typed {
                func: desc.func,
                signature: signature(params, desc.ret),
                lang,
            },
        };
        match by_class.iter_mut().find(|(name, _)| *name == class) {
            Some((_, methods)) => methods.push(method),
            None => by_class.push((class, vec![method])),
        }
    }
    by_class
        .into_iter()
        .map(|(class, methods)| Interface {
            lang,
            module: class.clone(),
            classes: vec![ClassIface {
                name: class.clone(),
                type_name: format!("{}.{class}", plugin.name),
                superclass: None,
                fields: Vec::new(),
                statics: Vec::new(),
                methods,
                ctor: None,
                class_object: Value::null(),
            }],
        })
        .collect()
}

fn capitalised(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The registry's type for a tag.
fn type_ref(tag: TypeTag) -> TypeRef {
    match tag.kind() {
        hl::HVOID => TypeRef::Void,
        hl::HUI8 | hl::HUI16 | hl::HI32 | hl::HI64 => TypeRef::Int,
        hl::HF32 | hl::HF64 => TypeRef::Float,
        hl::HBOOL => TypeRef::Bool,
        _ => TypeRef::Dyn,
    }
}

// ---------------------------------------------------------------------------
// Signatures: an hl_type per distinct list of tags, kept for the process
// ---------------------------------------------------------------------------

/// The `hl_type` of a scalar kind, one per kind, for the signatures' args.
fn kind_type(kind: hl::hl_type_kind) -> *mut hl_type {
    static KINDS: LazyLock<RwLock<HashMap<hl::hl_type_kind, usize>>> =
        LazyLock::new(|| RwLock::new(HashMap::new()));
    if let Some(&t) = KINDS.read().unwrap().get(&kind) {
        return t as *mut hl_type;
    }
    let t = Box::into_raw(Box::new(hl_type {
        kind,
        detail: hl_type_detail {
            abs_name: std::ptr::null(),
        },
        vobj_proto: std::ptr::null_mut(),
        mark_bits: std::ptr::null_mut(),
    }));
    KINDS.write().unwrap().insert(kind, t as usize);
    t
}

/// The `HFUN` `hl_type` for `params` and `ret`, the same object for the
/// same tags: what a `Callable::Typed` carries and `dispatch` reads.
fn signature(params: &[TypeTag], ret: TypeTag) -> *const hl_type {
    static SIGNATURES: LazyLock<RwLock<HashMap<Vec<u8>, usize>>> =
        LazyLock::new(|| RwLock::new(HashMap::new()));
    let key: Vec<u8> = params
        .iter()
        .map(|t| t.0)
        .chain(std::iter::once(ret.0))
        .collect();
    if let Some(&t) = SIGNATURES.read().unwrap().get(&key) {
        return t as *const hl_type;
    }
    let args: Vec<*mut hl_type> = params.iter().map(|t| kind_type(t.kind())).collect();
    let args = Box::leak(args.into_boxed_slice());
    let fun = Box::into_raw(Box::new(hl_type_fun {
        args: args.as_mut_ptr(),
        ret: kind_type(ret.kind()),
        nargs: params.len() as i32,
        parent: std::ptr::null_mut(),
        closure_type: hl_type_fun_closure_type {
            kind: hl::HVOID,
            p: std::ptr::null_mut(),
        },
        closure: hl_type_fun_closure {
            args: std::ptr::null_mut(),
            ret: std::ptr::null_mut(),
            nargs: 0,
            parent: std::ptr::null_mut(),
        },
    }));
    let t = Box::into_raw(Box::new(hl_type {
        kind: hl::HFUN,
        detail: hl_type_detail { fun },
        vobj_proto: std::ptr::null_mut(),
        mark_bits: std::ptr::null_mut(),
    }));
    SIGNATURES.write().unwrap().insert(key, t as usize);
    t
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Where `ash_native_call` reads an argument of a kind: an integer or
/// pointer word, an `f32`, an `f64`.
fn word_kind(kind: hl::hl_type_kind) -> u8 {
    match kind {
        hl::HF32 => 1,
        hl::HF64 => 2,
        _ => 0,
    }
}

fn raise(lang: LangId, message: &str) -> u8 {
    bridge::raise(CoreError::new(ErrorKind::Type, message, lang))
}

/// The word for `v` as an argument of `kind`: an integer as itself, a
/// float as its bits, a bool as 0 or 1, a `DYN` as the value's bits.
fn word_of(v: Value, kind: hl::hl_type_kind) -> Option<u64> {
    Some(match kind {
        hl::HUI8 | hl::HUI16 | hl::HI32 | hl::HI64 => Int64::of(v)? as u64,
        hl::HF32 => (v.as_number().or_else(|| v.as_int().map(f64::from))? as f32)
            .to_bits()
            .into(),
        hl::HF64 => v
            .as_number()
            .or_else(|| v.as_int().map(f64::from))?
            .to_bits(),
        hl::HBOOL => u64::from(v.as_bool()?),
        hl::HDYN => v.to_bits(),
        _ => return None,
    })
}

/// The value a result word of `kind` is.
fn value_of(word: i64, kind: hl::hl_type_kind) -> Value {
    match kind {
        hl::HVOID => Value::null(),
        hl::HUI8 => Value::int(i32::from(word as u8)),
        hl::HUI16 => Value::int(i32::from(word as u16)),
        hl::HI32 => Value::int(word as i32),
        hl::HI64 => Int64::value(word),
        hl::HF32 => Value::number(f64::from(f32::from_bits(word as u32))),
        hl::HF64 => Value::number(f64::from_bits(word as u64)),
        hl::HBOOL => Value::bool(word & 1 != 0),
        hl::HDYN => Value::from_bits(word as u64),
        _ => Value::null(),
    }
}

/// The plugin languages' typed dispatch: the signature's kinds decide
/// how each `Value` is passed and how the result comes back.
unsafe extern "C-unwind" fn dispatch(
    func: *const c_void,
    sig: *const hl_type,
    _site: *mut CallSite,
    args: *const Value,
    nargs: usize,
    out: *mut Value,
) -> u8 {
    let lang = caribou::world::LANG_CORE;
    let fun = unsafe { &*(*sig).detail.fun };
    let kinds: Vec<hl::hl_type_kind> = (0..fun.nargs as usize)
        .map(|i| unsafe { (**fun.args.add(i)).kind })
        .collect();
    if kinds.len() != nargs {
        return raise(
            lang,
            &format!(
                "the plugin function takes {} arguments, not {nargs}",
                kinds.len()
            ),
        );
    }
    let args = if nargs == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, nargs) }
    };
    let mut words = Vec::with_capacity(nargs);
    let mut word_kinds = Vec::with_capacity(nargs);
    for (i, (&v, &kind)) in args.iter().zip(&kinds).enumerate() {
        let Some(word) = word_of(v, kind) else {
            return raise(
                lang,
                &format!(
                    "argument {} of the plugin function cannot be {}",
                    i + 1,
                    bridge::describe(v)
                ),
            );
        };
        words.push(word);
        word_kinds.push(word_kind(kind));
    }
    let ret_kind = unsafe { (*fun.ret).kind };
    let pattern = ash_native_call::pattern_of(&word_kinds);
    let Some(word) = (unsafe {
        ash_native_call::dispatch_by_pattern(
            func as *mut c_void,
            &words,
            word_kind(ret_kind),
            pattern,
        )
    }) else {
        return raise(
            lang,
            "the plugin function's signature is not one the core can call",
        );
    };
    unsafe { *out = value_of(word, ret_kind) };
    REPLY_OK
}
