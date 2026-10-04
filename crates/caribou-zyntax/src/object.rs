//! Zyntax values standing in the core as its objects: an object of a
//! class a module publishes, and a function value.
//!
//! An object of a reference type is a core object itself: its first word
//! is the descriptor the core gave its type when the module was compiled
//! (`type_header`), so it crosses as itself, its fields read and written
//! in place where the runtime laid them out, and its methods the class's
//! published functions, called with the object first. An object made
//! without that word (before the core's heap was Zyntax's) crosses as a
//! proxy holding its address, one proxy per object for as long as the
//! proxy lives. A function value crosses as a proxy whose call is the
//! value's own call through its record.
//!
//! An error a call leaves pending in the runtime is taken after the call
//! and raised in the core, described by the module that raised it
//! (`Origin`).

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, OnceLock, RwLock};

use caribou::bridge;
use caribou::error::{Error, Str};
use caribou::hash::AddressMap;
use caribou::heap::{self, TypeDesc};
use caribou::native;
use caribou::protocol::{self, Callable, Protocol, REPLY_MISSING, REPLY_OK, REPLY_RAISED};
use caribou::spin::SpinLock;
use caribou::symbol::{self, Symbol};
use caribou::world::LANG_CORE;
use caribou_abi::hl::{self, hl_type, hl_type_detail, hl_type_kind};
use caribou_abi::mem::{KIND_DYNAMIC, TRACED};
use caribou_abi::{ErrorKind, LangId, Value};
use zyntax_compiler::host_heap::{HostFieldKind, HostTypeInfo};
use zyntax_embed::foreign::{self, Any};
use zyntax_embed::{TieredRuntime, ZyntaxString};

/// Where a Zyntax value came from: its language's runtime, and the
/// module whose own description names an error it raises.
#[derive(Clone, Copy)]
pub struct Origin {
    pub lang: LangId,
    pub runtime: *const TieredRuntime,
    /// The module's describing function's cell, for a language whose
    /// modules have one.
    pub describe: Option<*const AtomicUsize>,
}

unsafe impl Send for Origin {}
unsafe impl Sync for Origin {}

impl Origin {
    /// The error the last call into this runtime left pending, taken and
    /// made an error of the core; `None` when the call raised nothing.
    pub fn take_error(&self) -> Option<Value> {
        let runtime = unsafe { self.runtime.as_ref() }?;
        let word = runtime.take_pending_error()?;
        let text = self
            .describe
            .and_then(|cell| {
                let f = unsafe { (*cell).load(Ordering::Acquire) };
                (f != 0).then_some(f)
            })
            .and_then(|f| {
                let describe: extern "C" fn(u64) -> *const c_void =
                    unsafe { std::mem::transmute(f) };
                unsafe { text_of(describe(word)) }
            })
            .unwrap_or_else(|| "an error was raised".to_owned());
        Some(Error::value(Error::new(ErrorKind::User, &text, self.lang)))
    }
}

/// The text of a Zyntax string, read in place; none for a null pointer.
pub(crate) unsafe fn text_of(p: *const c_void) -> Option<String> {
    let string = unsafe { ZyntaxString::from_ptr(p.cast()) }?;
    Some(String::from_utf8_lossy(string.as_bytes()).into_owned())
}

/// A core string as a Zyntax string, allocated as Zyntax allocates its
/// strings: the program may keep it or release it as any of its own.
pub(crate) fn zyntax_string(text: &str) -> *mut c_void {
    ZyntaxString::from_str(text).into_raw().cast()
}

/// One field of a published class: where its object keeps it, and as
/// what.
pub struct Field {
    pub name: Symbol,
    pub offset: usize,
    pub size: usize,
    pub kind: hl_type_kind,
    /// For an object field, its class's type name.
    pub class: Option<Symbol>,
}

/// A class a module publishes, as its objects' proxies answer for it.
pub struct Class {
    desc: TypeDesc,
    pub type_name: Symbol,
    pub origin: Origin,
    fields: Vec<Field>,
    methods: RwLock<HashMap<Symbol, Callable>>,
    /// The descriptors its type's objects carry at word 0, one per layout
    /// the runtime compiled it with.
    directs: RwLock<Vec<usize>>,
}

unsafe impl Send for Class {}
unsafe impl Sync for Class {}

/// Classes by language and type name: the last published of each.
static CLASSES: LazyLock<RwLock<HashMap<(LangId, String), &'static Class>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Make `type_name`'s class, with `fields` laid out as its struct lays
/// them out; its methods follow with [`set_methods`]. A class published
/// again is a new class; the objects that crossed already keep the old.
pub fn publish_class(type_name: &str, origin: Origin, fields: Vec<Field>) -> &'static Class {
    let mut desc = TypeDesc::new(hl_type {
        kind: hl::HABSTRACT,
        detail: hl_type_detail {
            abs_name: std::ptr::null(),
        },
        vobj_proto: std::ptr::null_mut(),
        mark_bits: std::ptr::null_mut(),
    });
    let name: &'static str = Box::leak(type_name.to_owned().into_boxed_str());
    // What a proxy holds is the program's, not the core heap's.
    desc.trace = Some(trace_nothing);
    desc.drop = Some(drop_object);
    desc.protocol = &OBJECT_PROTO;
    desc.name = name.as_ptr();
    desc.name_len = name.len();
    desc.lang = origin.lang;
    let directs = DIRECTS
        .read()
        .unwrap()
        .get(&(origin.lang, type_name.to_owned()))
        .cloned()
        .unwrap_or_default();
    let class: &'static mut Class = Box::leak(Box::new(Class {
        desc,
        type_name: symbol::intern(type_name),
        origin,
        fields,
        methods: RwLock::new(HashMap::new()),
        directs: RwLock::new(directs),
    }));
    class.desc.ext = class as *mut Class as *mut ();
    let class: &'static Class = class;
    CLASSES
        .write()
        .unwrap()
        .insert((origin.lang, type_name.to_owned()), class);
    class
}

/// The instance methods `class`'s objects answer, each its published
/// function taking the object first.
pub fn set_methods(class: &Class, methods: impl IntoIterator<Item = (String, Callable)>) {
    let mut table = class.methods.write().unwrap();
    for (name, target) in methods {
        table.insert(symbol::intern(&name), target);
    }
}

/// The published class of `lang` named `type_name`.
pub fn class(lang: LangId, type_name: &str) -> Option<&'static Class> {
    CLASSES
        .read()
        .unwrap()
        .get(&(lang, type_name.to_owned()))
        .copied()
}

#[repr(C)]
struct ObjectProxy {
    desc: *const TypeDesc,
    word: usize,
}

/// Each live proxy by the address of the object it stands for. Only a
/// proxy's drop takes it out, so one named here is alive; its lock is
/// taken before an allocation, whose collection may run that drop.
fn objects() -> &'static SpinLock<AddressMap<usize>> {
    static OBJECTS: OnceLock<SpinLock<AddressMap<usize>>> = OnceLock::new();
    OBJECTS.get_or_init(|| SpinLock::new(AddressMap::default()))
}

/// The class a proxy of an object answers for, when `p` is one.
///
/// # Safety
/// `p` must be null or a live object of the core.
unsafe fn class_of(p: *const u8) -> Option<&'static Class> {
    if p.is_null() {
        return None;
    }
    let desc = unsafe { protocol::desc_of(p) };
    if desc.is_null() || !std::ptr::eq(unsafe { (*desc).protocol }, &OBJECT_PROTO) {
        return None;
    }
    Some(unsafe { &*((*desc).ext as *const Class) })
}

/// The object at `word`, of `class`, as a value of the core: its live
/// proxy, or a new one.
pub fn object(word: usize, class: &'static Class) -> Value {
    if word == 0 {
        return Value::null();
    }
    let found = objects().lock().get(&word).copied();
    if let Some(p) = found {
        return Value::object(p as *const c_void);
    }
    let p = unsafe {
        heap::alloc_gen(
            &class.desc as *const TypeDesc as *mut hl_type,
            size_of::<ObjectProxy>(),
            KIND_DYNAMIC | TRACED,
        )
    } as *mut ObjectProxy;
    if p.is_null() {
        heap::out_of_memory("a Zyntax object");
    }
    unsafe { (*p).word = word };
    // Another thread may have made one meanwhile; ours is then garbage,
    // and its drop forgets nothing, not being the one kept.
    let kept = *objects().lock().entry(word).or_insert(p as usize);
    Value::object(kept as *const c_void)
}

/// The object `v` is, or stands for as a proxy: its class and address.
pub fn object_of(v: Value) -> Option<(&'static Class, usize)> {
    let p = v.as_object()? as *const u8;
    if let Some(direct) = unsafe { direct_of(p) } {
        return Some((direct.class()?, p as usize));
    }
    let class = unsafe { class_of(p) }?;
    Some((class, unsafe { (*(p as *const ObjectProxy)).word }))
}

unsafe extern "C" fn trace_nothing(_obj: *mut u8, _tracer: *mut heap::Tracer) {}

unsafe extern "C" fn drop_object(obj: *mut u8) {
    let word = unsafe { (*(obj as *const ObjectProxy)).word };
    let mut objects = objects().lock();
    if objects.get(&word) == Some(&(obj as usize)) {
        objects.remove(&word);
    }
}

/// A value of the core as the word a Zyntax parameter, result or field of
/// `kind` holds; `class` names an object kind's class. The error says why
/// it cannot be one.
#[inline]
pub fn word_in(v: Value, kind: hl_type_kind, class: Option<Symbol>) -> Result<u64, String> {
    match kind {
        hl::HDYN => Ok(crate::foreign::any_of(v) as u64),
        hl::HBYTES => match unsafe { Str::text(v) } {
            Some(text) => Ok(zyntax_string(text) as u64),
            None if v.is_null() => Ok(0),
            None => Err(format!("must be a string, not {}", bridge::describe(v))),
        },
        hl::HOBJ => {
            if v.is_null() {
                return Ok(0);
            }
            match object_of(v) {
                Some((found, word)) if class.is_none_or(|name| found.type_name == name) => {
                    Ok(word as u64)
                }
                _ => Err(format!(
                    "must be {}, not {}",
                    class.map_or("an object of the language", |name| name.name()),
                    bridge::describe(v)
                )),
            }
        }
        _ => native::word_of(v, kind).ok_or_else(|| format!("cannot be {}", bridge::describe(v))),
    }
}

/// The word of `kind` a Zyntax function or field gave, as a value of the
/// core; `class` is an object kind's class, `origin` where a function
/// value came from.
///
/// # Safety
/// `word` is a live value of `kind`.
#[inline]
pub unsafe fn value_out(
    word: u64,
    kind: hl_type_kind,
    class: Option<&'static Class>,
    origin: Option<&Origin>,
) -> Result<Value, String> {
    Ok(match kind {
        hl::HDYN => match unsafe { crate::foreign::value_from(word as Any, origin) } {
            Ok((v, _)) => v,
            Err(e) => return Err(e.message),
        },
        hl::HBYTES => match unsafe { text_of(word as *const c_void) } {
            Some(text) => Str::value(Str::new(&text)),
            None => Value::null(),
        },
        hl::HOBJ => match class {
            // An object that carries its type's descriptor is the core's.
            Some(class) if word != 0 && class.carries(word as usize) => {
                Value::object(word as *const c_void)
            }
            Some(class) => object(word as usize, class),
            None if word == 0 => Value::null(),
            None => return Err("an object of a class no module publishes".to_owned()),
        },
        _ => native::value_of(word as i64, kind),
    })
}

/// The proxy at `obj`: its class and the object's address.
unsafe fn proxy(obj: *mut u8) -> (&'static Class, usize) {
    let class = unsafe { class_of(obj) }.expect("a proxy of an object");
    (class, unsafe { (*(obj as *const ObjectProxy)).word })
}

fn raise(kind: ErrorKind, message: &str, lang: LangId) -> u8 {
    bridge::raise(Error::new(kind, message, lang))
}

unsafe extern "C-unwind" fn object_get(obj: *mut u8, name: Symbol, out: *mut Value) -> u8 {
    let (class, word) = unsafe { proxy(obj) };
    unsafe { get_field(&class.fields, &class.origin, word, name, out) }
}

unsafe extern "C-unwind" fn object_set(obj: *mut u8, name: Symbol, value: Value) -> u8 {
    let (class, word) = unsafe { proxy(obj) };
    unsafe { set_field(&class.fields, class.origin.lang, word, name, value) }
}

/// Field `name` of the object at `word`, laid out as `fields` say.
///
/// # Safety
/// `word` is a live object those fields describe.
unsafe fn get_field(
    fields: &[Field],
    origin: &Origin,
    word: usize,
    name: Symbol,
    out: *mut Value,
) -> u8 {
    let Some(field) = fields.iter().find(|f| f.name == name) else {
        return REPLY_MISSING;
    };
    let at = (word + field.offset) as *const u8;
    let raw = unsafe {
        match field.size {
            1 => u64::from(*at),
            2 => u64::from(*(at as *const u16)),
            4 => u64::from(*(at as *const u32)),
            _ => *(at as *const u64),
        }
    };
    let field_class = field
        .class
        .and_then(|name| self::class(origin.lang, name.name()));
    match unsafe { value_out(raw, field.kind, field_class, Some(origin)) } {
        Ok(v) => {
            unsafe { *out = v };
            REPLY_OK
        }
        Err(m) => raise(
            ErrorKind::Type,
            &format!("field `{}`: {m}", name.name()),
            origin.lang,
        ),
    }
}

/// Set field `name` of the object at `word`, laid out as `fields` say.
///
/// # Safety
/// `word` is a live object those fields describe.
unsafe fn set_field(fields: &[Field], lang: LangId, word: usize, name: Symbol, value: Value) -> u8 {
    let Some(field) = fields.iter().find(|f| f.name == name) else {
        return REPLY_MISSING;
    };
    let raw = match word_in(value, field.kind, field.class) {
        Ok(raw) => raw,
        Err(m) => {
            return raise(
                ErrorKind::Type,
                &format!("field `{}` {m}", name.name()),
                lang,
            );
        }
    };
    let at = (word + field.offset) as *mut u8;
    unsafe {
        match field.size {
            1 => *at = raw as u8,
            2 => *(at as *mut u16) = raw as u16,
            4 => *(at as *mut u32) = raw as u32,
            _ => *(at as *mut u64) = raw,
        }
    }
    REPLY_OK
}

unsafe extern "C-unwind" fn object_invoke(
    obj: *mut u8,
    name: Symbol,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let (class, _) = unsafe { proxy(obj) };
    unsafe { invoke_method(class, obj, name, args, n, out) }
}

/// Method `name` of `class` on the object `obj` is to the core.
///
/// # Safety
/// `args` holds `n` values.
unsafe fn invoke_method(
    class: &Class,
    obj: *mut u8,
    name: Symbol,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let Some(target) = class.methods.read().unwrap().get(&name).copied() else {
        return REPLY_MISSING;
    };
    let mut with_self = Vec::with_capacity(n + 1);
    with_self.push(Value::object(obj as *const c_void));
    if n > 0 {
        with_self.extend_from_slice(unsafe { std::slice::from_raw_parts(args, n) });
    }
    match bridge::call(target, &with_self, LANG_CORE) {
        Ok(v) => {
            unsafe { *out = v };
            REPLY_OK
        }
        Err(e) => {
            bridge::set_pending(e);
            REPLY_RAISED
        }
    }
}

unsafe extern "C-unwind" fn object_type_name(obj: *mut u8, out: *mut Symbol) -> u8 {
    let (class, _) = unsafe { proxy(obj) };
    unsafe { *out = class.type_name };
    REPLY_OK
}

static OBJECT_PROTO: Protocol = Protocol {
    get_member: Some(object_get),
    set_member: Some(object_set),
    invoke: Some(object_invoke),
    type_name: Some(object_type_name),
    ..Protocol::NONE
};

// ---------------------------------------------------------------------------
// Objects that are the core's: a reference type's word 0
// ---------------------------------------------------------------------------

/// A reference type as the core sees its objects: each carries this at
/// word 0, so the object is a core object, its fields where the runtime
/// laid them out.
struct Direct {
    desc: TypeDesc,
    lang: LangId,
    type_name: Symbol,
    fields: Vec<Field>,
}

impl Direct {
    /// The published class of the type, for its methods and origin.
    fn class(&self) -> Option<&'static Class> {
        class(self.lang, self.type_name.name())
    }
}

impl Class {
    /// Whether the object at `word` carries one of this class's
    /// descriptors: compared, not read through, so any word is safe.
    #[inline]
    fn carries(&self, word: usize) -> bool {
        // SAFETY: a published class's objects start with their header.
        let header = unsafe { *(word as *const usize) };
        header != 0 && self.directs.read().unwrap().contains(&header)
    }
}

/// Descriptor addresses by language and type name.
type Directs = HashMap<(LangId, String), Vec<usize>>;

/// The descriptors made for each type, which a class published later
/// takes up.
static DIRECTS: LazyLock<RwLock<Directs>> = LazyLock::new(|| RwLock::new(HashMap::new()));

thread_local! {
    /// The language whose module this thread is compiling: whose types the
    /// runtime describes to `type_header`.
    static COMPILING: Cell<Option<LangId>> = const { Cell::new(None) };
}

/// `f`, with the types the runtime describes meanwhile `lang`'s.
pub fn compiling<T>(lang: LangId, f: impl FnOnce() -> T) -> T {
    let previous = COMPILING.with(|c| c.replace(Some(lang)));
    let result = f();
    COMPILING.with(|c| c.set(previous));
    result
}

/// The direct type at `p`'s word 0, when `p` is one of its objects.
///
/// # Safety
/// `p` must be null or a live object of the core.
unsafe fn direct_of(p: *const u8) -> Option<&'static Direct> {
    if p.is_null() {
        return None;
    }
    let desc = unsafe { protocol::desc_of(p) };
    if desc.is_null() || !std::ptr::eq(unsafe { (*desc).protocol }, &DIRECT_PROTO) {
        return None;
    }
    Some(unsafe { &*((*desc).ext as *const Direct) })
}

/// Whether `v` is a Zyntax object itself, carrying its type's descriptor,
/// rather than a proxy of one.
pub fn is_direct(v: Value) -> bool {
    v.as_object()
        .is_some_and(|p| unsafe { direct_of(p as *const u8) }.is_some())
}

/// The class name a qualified Zyntax type (`module.Name`) publishes as.
fn class_name(lang: LangId, qualified: &str) -> String {
    let name = qualified.rsplit('.').next().unwrap_or(qualified);
    format!("{}.{name}", caribou::world::language_name(lang))
}

/// The host heap's `type_header` slot: a descriptor for a type the module
/// being compiled allocates, which its objects carry at word 0. Null, and
/// so no header, for a type compiled outside a module load.
///
/// # Safety
/// `info` is valid for the call, as the runtime promises.
pub unsafe extern "C" fn type_header(_cx: *mut c_void, info: *const HostTypeInfo) -> *const c_void {
    let Some(lang) = COMPILING.with(Cell::get) else {
        return std::ptr::null();
    };
    let info = unsafe { &*info };
    let text = |p: *const std::ffi::c_char| {
        (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    };
    let Some(qualified) = text(info.name) else {
        return std::ptr::null();
    };
    let type_name = class_name(lang, &qualified);
    let infos = if info.field_count == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(info.fields, info.field_count as usize) }
    };
    let fields = infos
        .iter()
        .filter_map(|f| {
            let name = text(f.name)?;
            // Frontends reserve `$` fields for their object layout.
            if name.starts_with('$') {
                return None;
            }
            let kind = match (f.kind, f.size) {
                (HostFieldKind::Int | HostFieldKind::UInt, 1) => hl::HUI8,
                (HostFieldKind::Int | HostFieldKind::UInt, 2) => hl::HUI16,
                (HostFieldKind::Int | HostFieldKind::UInt, 4) => hl::HI32,
                (HostFieldKind::Int | HostFieldKind::UInt, _) => hl::HI64,
                (HostFieldKind::Float, 4) => hl::HF32,
                (HostFieldKind::Float, _) => hl::HF64,
                (HostFieldKind::Bool, _) => hl::HBOOL,
                (HostFieldKind::Str, _) => hl::HBYTES,
                (HostFieldKind::Object, _) => hl::HOBJ,
                (HostFieldKind::Any, _) => hl::HDYN,
                // A raw pointer or bytes are the program's own.
                _ => return None,
            };
            Some(Field {
                name: symbol::intern(&name),
                offset: f.offset as usize,
                size: f.size as usize,
                kind,
                class: text(f.type_name).map(|t| symbol::intern(&class_name(lang, &t))),
            })
        })
        .collect();
    let mut desc = TypeDesc::new(hl_type {
        kind: hl::HABSTRACT,
        detail: hl_type_detail {
            abs_name: std::ptr::null(),
        },
        vobj_proto: std::ptr::null_mut(),
        mark_bits: std::ptr::null_mut(),
    });
    let name: &'static str = Box::leak(type_name.clone().into_boxed_str());
    desc.trace = Some(trace_nothing);
    desc.protocol = &DIRECT_PROTO;
    desc.name = name.as_ptr();
    desc.name_len = name.len();
    desc.lang = lang;
    // Kept for the process: an object compiled with this layout carries it
    // for as long as it lives, across reloads.
    let direct: &'static mut Direct = Box::leak(Box::new(Direct {
        desc,
        lang,
        type_name: symbol::intern(&type_name),
        fields,
    }));
    direct.desc.ext = direct as *mut Direct as *mut ();
    let header = &direct.desc as *const TypeDesc as usize;
    DIRECTS
        .write()
        .unwrap()
        .entry((lang, type_name.clone()))
        .or_default()
        .push(header);
    if let Some(class) = class(lang, &type_name) {
        class.directs.write().unwrap().push(header);
    }
    header as *const c_void
}

unsafe extern "C-unwind" fn direct_get(obj: *mut u8, name: Symbol, out: *mut Value) -> u8 {
    let direct = unsafe { direct_of(obj) }.expect("a direct object");
    let Some(class) = direct.class() else {
        return REPLY_MISSING;
    };
    unsafe { get_field(&direct.fields, &class.origin, obj as usize, name, out) }
}

unsafe extern "C-unwind" fn direct_set(obj: *mut u8, name: Symbol, value: Value) -> u8 {
    let direct = unsafe { direct_of(obj) }.expect("a direct object");
    unsafe { set_field(&direct.fields, direct.lang, obj as usize, name, value) }
}

unsafe extern "C-unwind" fn direct_invoke(
    obj: *mut u8,
    name: Symbol,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let direct = unsafe { direct_of(obj) }.expect("a direct object");
    let Some(class) = direct.class() else {
        return REPLY_MISSING;
    };
    unsafe { invoke_method(class, obj, name, args, n, out) }
}

unsafe extern "C-unwind" fn direct_type_name(obj: *mut u8, out: *mut Symbol) -> u8 {
    let direct = unsafe { direct_of(obj) }.expect("a direct object");
    unsafe { *out = direct.type_name };
    REPLY_OK
}

static DIRECT_PROTO: Protocol = Protocol {
    get_member: Some(direct_get),
    set_member: Some(direct_set),
    invoke: Some(direct_invoke),
    type_name: Some(direct_type_name),
    ..Protocol::NONE
};

/// A function value of a Zyntax program, as the core holds it.
#[repr(C)]
struct FunctionProxy {
    desc: *const TypeDesc,
    value: Any,
    origin: Origin,
}

fn function_descriptor() -> &'static TypeDesc {
    static DESC: OnceLock<&'static TypeDesc> = OnceLock::new();
    DESC.get_or_init(|| {
        let name = "zyntax.function";
        let mut d = TypeDesc::new(hl_type {
            kind: hl::HABSTRACT,
            detail: hl_type_detail {
                abs_name: std::ptr::null(),
            },
            vobj_proto: std::ptr::null_mut(),
            mark_bits: std::ptr::null_mut(),
        });
        d.trace = Some(trace_nothing);
        d.protocol = &FUNCTION_PROTO;
        d.name = name.as_ptr();
        d.name_len = name.len();
        Box::leak(Box::new(d))
    })
}

/// The function value `f` of the program `origin` names, as a value of
/// the core.
pub fn function(f: Any, origin: Origin) -> Value {
    let p = unsafe {
        heap::alloc_gen(
            function_descriptor() as *const TypeDesc as *mut hl_type,
            size_of::<FunctionProxy>(),
            KIND_DYNAMIC | TRACED,
        )
    } as *mut FunctionProxy;
    if p.is_null() {
        heap::out_of_memory("a Zyntax function");
    }
    unsafe {
        (*p).value = f;
        (*p).origin = origin;
    }
    Value::object(p as *const c_void)
}

/// The function value `v` stands for, when it is one.
pub fn function_of(v: Value) -> Option<Any> {
    let p = v.as_object()? as *const u8;
    if p.is_null() || !std::ptr::eq(unsafe { protocol::desc_of(p) }, function_descriptor()) {
        return None;
    }
    Some(unsafe { (*(p as *const FunctionProxy)).value })
}

unsafe extern "C-unwind" fn function_call(
    obj: *mut u8,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let proxy = unsafe { &*(obj as *const FunctionProxy) };
    let origin = proxy.origin;
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, n) }
    };
    if args.len() > foreign::MAX_CALL_ARITY {
        return raise(
            ErrorKind::Type,
            &format!(
                "a call through a function value passes at most {} arguments",
                foreign::MAX_CALL_ARITY
            ),
            origin.lang,
        );
    }
    let mut anys = [std::ptr::null_mut(); foreign::MAX_CALL_ARITY];
    for (slot, &v) in anys.iter_mut().zip(args) {
        *slot = crate::foreign::any_of(v);
    }
    let anys = &anys[..args.len()];
    crate::foreign::as_caller(origin.lang, || {
        let result = unsafe { foreign::call_function(proxy.value, anys) };
        if let Some(error) = origin.take_error() {
            bridge::set_pending(error);
            return REPLY_RAISED;
        }
        match result {
            Ok(any) => match unsafe { crate::foreign::value_from(any, Some(&origin)) } {
                Ok((v, _)) => {
                    unsafe { *out = v };
                    REPLY_OK
                }
                Err(e) => raise(ErrorKind::Type, &e.message, origin.lang),
            },
            Err(e) => raise(ErrorKind::Type, &e.message, origin.lang),
        }
    })
}

unsafe extern "C-unwind" fn function_arity(obj: *mut u8, out: *mut usize) -> u8 {
    let proxy = unsafe { &*(obj as *const FunctionProxy) };
    match unsafe { foreign::function_arity(proxy.value) } {
        Some((_, most)) => {
            unsafe { *out = most };
            REPLY_OK
        }
        None => protocol::REPLY_UNSUPPORTED,
    }
}

static FUNCTION_PROTO: Protocol = Protocol {
    call: Some(function_call),
    arity: Some(function_arity),
    ..Protocol::NONE
};
