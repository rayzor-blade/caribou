//! Natives a Haxe program declares against the `caribou` library: the
//! members of classes another language published, reached by name.
//!
//! The build macro emits, for each such class, a Haxe class extending
//! `caribou.Ref` whose members are natives named for what they reach:
//! `game:hud.Hud.draw(_)`, a Wren signature after the namespace, module
//! and class. The name is the whole binding. When the program loads, each
//! such native is registered with ash as one of nine entries, by argument
//! count, and its parsed name as the context word ash's tiers pass first;
//! ash looks for no library under them. The call finds the member through
//! the registry and the bridge when it happens, so nothing has to be
//! published before the program starts, and a class published again
//! answers the next call.
//!
//! A Haxe object of such a class is the face of one foreign object: its
//! first field holds the object's ref (`wrenref.rs`), and the ref knows
//! its face, so the object coming back to Haxe is the same Haxe object.
//! A face is allocated under the class the program declares for the
//! object's published type, else under `caribou.Ref` when the program
//! has it.

// Only a loaded program binds natives; the faces are reached regardless.
#![cfg_attr(not(feature = "runner"), allow(dead_code))]

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

#[cfg(feature = "runner")]
use anyhow::{Result, anyhow, bail};
#[cfg(feature = "runner")]
use ash_core::bytecode::DecodedBytecode;
#[cfg(feature = "runner")]
use ash_core::native_lib::HostNative;
#[cfg(feature = "runner")]
use ash_interp::interpreter::HLInterpreter;
use ash_std::error::hlp_throw;
use ash_std::obj::{hlp_alloc_obj, hlp_get_obj_rt};
use caribou::bridge;
use caribou::protocol::{CallSite, Callable, Symbol};
use caribou::registry::{self, ClassIface, Interface};
use caribou::report;
use caribou::symbol::intern;
use caribou_abi::hl::{self, hl_type, hl_type_kind, vdynamic};
use caribou_abi::{LangId, Value};

use crate::proto::{self, lang};
use crate::wrenref;

/// The library the natives name.
pub const LIB: &str = "caribou";
/// The class every face extends, whose first field holds the ref.
pub const REF_CLASS: &str = "caribou.Ref";

/// Arguments a native declares, receiver included: Wren's widest
/// signature and one more.
const MAX_ARGS: usize = 17;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// `draw(_)`: the receiver, then the parameters.
    Method,
    /// `score`: the receiver.
    Get,
    /// `score=(_)`: the receiver and the value.
    Set,
    /// `static:make()`: the parameters.
    Static,
    /// `construct:new(_)`: the fresh Haxe object, then the parameters; the
    /// call makes the foreign object and binds it to the face.
    Init,
    /// `len`, `index` and `set_index`: a sequence's count and elements
    /// through the bridge, for `caribou.Sequence`. The receiver is a
    /// face, or a Haxe object the bridge wraps.
    Len,
    Index,
    SetIndex,
}

impl Kind {
    fn takes_receiver(self) -> bool {
        !matches!(self, Kind::Static)
    }

    /// The bridge's own operations: natives named for what they do, not
    /// for a member of a published class.
    fn operation(name: &str) -> Option<Kind> {
        Some(match name {
            "len" => Kind::Len,
            "index" => Kind::Index,
            "set_index" => Kind::SetIndex,
            _ => return None,
        })
    }
}

#[derive(Debug)]
struct Slot {
    name: String,
    namespace: String,
    module: String,
    class: String,
    /// The member's Wren signature, or its bare name for a getter or
    /// setter: what the bridge is asked for.
    member: Symbol,
    kind: Kind,
    /// What a static or a constructor resolved to, under the registry
    /// generation it was resolved in: good until something publishes.
    /// Null until first resolved; replaced whole, and the replaced one
    /// left for a reader that still holds it.
    resolved: AtomicPtr<Resolved>,
    /// What the callee's protocol derived for this slot last time.
    site: CallSite,
    /// The kinds the program declared the native with, set by `bind`, and
    /// what the record's words are read by. Null until bound; replaced
    /// whole by a program that declares the name again.
    kinds: AtomicPtr<Kinds>,
    /// Scalars that crossed boxed, through a `Dynamic` parameter or
    /// result, for the run report.
    boxed_in: AtomicUsize,
    boxed_out: AtomicUsize,
}

#[derive(Debug)]
struct Resolved {
    generation: u64,
    target: Callable,
}

/// A native's declared argument and result kinds, and once the program
/// has built its types, the result's type: a function result crosses as
/// a closure of that type.
#[derive(Debug)]
struct Kinds {
    args: Vec<hl_type_kind>,
    ret: hl_type_kind,
    /// The native's function type in the program's type table.
    type_index: usize,
    ret_type: *const hl_type,
}

// The callable's pointers belong to the publishing language's program,
// which outlives the slot; the same rule an `Interface` is shared under.
unsafe impl Send for Slot {}
unsafe impl Sync for Slot {}

impl Slot {
    /// The callable a static or constructor slot reaches, resolved once
    /// per registry generation.
    fn target(
        &self,
        resolve: impl FnOnce() -> Result<Callable, String>,
    ) -> Result<Callable, String> {
        let generation = registry::generation();
        let cached = self.resolved.load(Ordering::Acquire);
        if let Some(r) = unsafe { cached.as_ref() }
            && r.generation == generation
        {
            return Ok(r.target);
        }
        let target = resolve()?;
        let fresh = Box::into_raw(Box::new(Resolved { generation, target }));
        self.resolved.store(fresh, Ordering::Release);
        Ok(target)
    }
}

/// Every slot ever bound: a slot's address is a native's context word for
/// the life of the process.
static SLOT_TABLE: RwLock<Vec<Arc<Slot>>> = RwLock::new(Vec::new());

/// Whether a `caribou` native names one of the bridge's own operations
/// rather than a member of a published class.
pub fn is_operation(name: &str) -> bool {
    Kind::operation(name).is_some()
}

/// Every bound native the program has called, as a site of the run
/// report: whether it holds a direct send, how many calls took the plain
/// path, and how many scalars crossed boxed.
pub fn sites() -> Vec<report::Site> {
    let table = SLOT_TABLE.read().unwrap_or_else(|e| e.into_inner());
    table
        .iter()
        .map(|s| report::Site {
            name: match s.kind {
                Kind::Len => "caribou.Sequence.length".to_owned(),
                Kind::Index => "caribou.Sequence.[]".to_owned(),
                Kind::SetIndex => "caribou.Sequence.[]=".to_owned(),
                _ => s.name.clone(),
            },
            direct: s.site.direct().is_some(),
            plain: s.site.plain(),
            boxed_in: s.boxed_in.load(Ordering::Relaxed),
            boxed_out: s.boxed_out.load(Ordering::Relaxed),
            links: links(s),
        })
        .filter(|site| site.direct || site.plain > 0)
        .collect()
}

/// Whether the member behind `s` links at AOT: by the types its class
/// published, when it is published; a member not published yet is
/// judged by the kinds the program declared, where a `Dynamic` is what
/// an untyped export becomes.
fn links(s: &Slot) -> bool {
    if Kind::operation(&s.name).is_some() {
        return false;
    }
    let published = registry::lookup_class(&s.namespace, &s.module, &s.class);
    if let Some((iface, index)) = published {
        let class = &iface.classes[index];
        let member = match s.kind {
            Kind::Init => class.ctor.as_ref(),
            Kind::Len | Kind::Index | Kind::SetIndex => None,
            Kind::Static => static_member(class, s.member),
            Kind::Method => class.methods.iter().find(|m| {
                !m.is_static
                    && matches!(m.target, Callable::WrenMethod { signature, .. } if signature == s.member)
            }),
            Kind::Get | Kind::Set => {
                let name = s.member.name();
                return class
                    .fields
                    .iter()
                    .chain(class.statics.iter())
                    .find(|f| f.name == name)
                    .is_some_and(|f| caribou::link::CType::of(&f.ty).is_some());
            }
        };
        if let Some(m) = member {
            let ret = if s.kind == Kind::Init {
                &registry::TypeRef::Void
            } else {
                &m.ret
            };
            return m
                .params
                .iter()
                .chain(std::iter::once(ret))
                .all(|t| caribou::link::CType::of(t).is_some());
        }
    }
    let kinds = unsafe { s.kinds.load(Ordering::Acquire).as_ref() };
    kinds.is_some_and(|k| k.ret != hl::HDYN && k.args.iter().all(|&kind| kind != hl::HDYN))
}

/// The `hl_type` of the face class per `(namespace, module, class)`, and
/// of `caribou.Ref`, recorded once the interpreter has built its types.
static FACES: RwLock<Option<Faces>> = RwLock::new(None);

struct Faces {
    by_class: HashMap<(String, String, String), usize>,
    fallback: usize,
    /// Answers already found for a published type name.
    by_type: HashMap<(LangId, String), usize>,
}

/// Parse `game:hud.Hud.draw(_)`: namespace, module, class, and the
/// member's Wren signature with its `static:` or `construct:` prefix.
pub(crate) fn parse(name: &str) -> Option<(String, String, String, String)> {
    let (namespace, rest) = name.split_once(':')?;
    // The signature is the last dot-separated piece up to its parameters;
    // the class is the piece before it, and the module is what remains.
    let params = rest.find('(').unwrap_or(rest.len());
    let dot = rest[..params].rfind('.')?;
    let (path, member) = (&rest[..dot], &rest[dot + 1..]);
    let (module, class) = path.rsplit_once('.')?;
    if namespace.is_empty() || module.is_empty() || class.is_empty() || member.is_empty() {
        return None;
    }
    Some((
        namespace.to_owned(),
        module.to_owned(),
        class.to_owned(),
        member.to_owned(),
    ))
}

fn slot_for(name: &str) -> Result<Slot, String> {
    if let Some(kind) = Kind::operation(name) {
        return Ok(Slot {
            name: name.to_owned(),
            namespace: String::new(),
            module: String::new(),
            class: String::new(),
            member: intern(name),
            kind,
            resolved: AtomicPtr::new(ptr::null_mut()),
            site: CallSite::new(),
            kinds: AtomicPtr::new(ptr::null_mut()),
            boxed_in: AtomicUsize::new(0),
            boxed_out: AtomicUsize::new(0),
        });
    }
    let (namespace, module, class, member) = parse(name)
        .ok_or_else(|| format!("`{name}` does not name a member of a published class"))?;
    let (kind, member) = if let Some(sig) = member.strip_prefix("static:") {
        (Kind::Static, sig.to_owned())
    } else if let Some(sig) = member.strip_prefix("construct:") {
        (Kind::Init, sig.to_owned())
    } else if let Some(field) = member.strip_suffix("=(_)") {
        (Kind::Set, field.to_owned())
    } else if member.contains('(') {
        (Kind::Method, member)
    } else {
        (Kind::Get, member)
    };
    Ok(Slot {
        name: name.to_owned(),
        namespace,
        module,
        class,
        member: intern(&member),
        kind,
        resolved: AtomicPtr::new(ptr::null_mut()),
        site: CallSite::new(),
        kinds: AtomicPtr::new(ptr::null_mut()),
        boxed_in: AtomicUsize::new(0),
        boxed_out: AtomicUsize::new(0),
    })
}

/// The kinds a native declares, from its function type.
#[cfg(feature = "runner")]
fn declared(bytecode: &DecodedBytecode, type_index: usize) -> Option<Kinds> {
    let fun = bytecode.types.get(type_index)?.fun.as_ref()?;
    let kind = |t: &ash_core::types::TypeRef| bytecode.types.get(t.0).map(|t| t.kind);
    Some(Kinds {
        args: fun.args.iter().map(kind).collect::<Option<_>>()?,
        ret: kind(&fun.ret)?,
        type_index,
        ret_type: ptr::null(),
    })
}

/// Give every `caribou` native the program declares its entry and slot:
/// the map ash's resolver takes before it looks for libraries. Every
/// native is the one entry, called by record with the slot as its
/// context, so the program may declare each with the types it has.
#[cfg(feature = "runner")]
pub fn bind(bytecode: &DecodedBytecode) -> Result<HashMap<(String, String), HostNative>> {
    let mut table = SLOT_TABLE.write().unwrap();
    let mut natives = HashMap::new();
    for native in bytecode.natives.iter().filter(|n| n.lib == LIB) {
        let slot = match table.iter().position(|s| s.name == native.name) {
            Some(i) => i,
            None => {
                table.push(Arc::new(slot_for(&native.name).map_err(|m| anyhow!(m))?));
                table.len() - 1
            }
        };
        let kinds = declared(bytecode, native.type_.0)
            .ok_or_else(|| anyhow!("`{}` has no function type", native.name))?;
        let nargs = kinds.args.len();
        if nargs > MAX_ARGS {
            bail!(
                "`{}` takes {nargs} arguments; at most {MAX_ARGS}",
                native.name
            );
        }
        let s = &table[slot];
        if s.kind.takes_receiver() && nargs == 0 {
            bail!(
                "`{}` takes a receiver but declares no arguments",
                native.name
            );
        }
        if s.kind == Kind::Set && nargs != 2 || s.kind == Kind::Get && nargs != 1 {
            bail!("`{}` declares {nargs} arguments", native.name);
        }
        s.kinds
            .store(Box::into_raw(Box::new(kinds)), Ordering::Release);
        natives.insert(
            (LIB.to_owned(), native.name.clone()),
            HostNative {
                addr: entry as *const () as usize,
                context: Arc::as_ptr(&table[slot]) as usize,
                record: true,
            },
        );
    }
    Ok(natives)
}

/// Record the face classes: after the interpreter exists, since the
/// runtime types are its. Every class a bound native names must be in
/// the program; `caribou.Ref` need not be.
#[cfg(feature = "runner")]
pub fn attach_types(bytecode: &DecodedBytecode, interpreter: &HLInterpreter) -> Result<()> {
    let table = SLOT_TABLE.read().unwrap();
    let mut by_class = HashMap::new();
    for s in table.iter() {
        // An operation of the bridge's own names no class.
        if Kind::operation(&s.name).is_some() {
            continue;
        }
        let key = (s.namespace.clone(), s.module.clone(), s.class.clone());
        if by_class.contains_key(&key) {
            continue;
        }
        let name = format!("{}.{}.{}", s.namespace, s.module.replace('/', "."), s.class);
        let index = bytecode
            .type_index_of(&name)
            .ok_or_else(|| anyhow!("the program declares `{}` but no class `{name}`", s.name))?;
        by_class.insert(key, interpreter.c_type_of(index) as usize);
    }
    let fallback = bytecode
        .type_index_of(REF_CLASS)
        .map_or(0, |i| interpreter.c_type_of(i) as usize);
    // The result types, now that they exist.
    for s in table.iter() {
        let kinds = s.kinds.load(Ordering::Acquire);
        let Some(k) = (unsafe { kinds.as_ref() }) else {
            continue;
        };
        let sig = interpreter.c_type_of(k.type_index) as *const hl_type;
        let ret_type = unsafe { sig.as_ref() }
            .and_then(|t| unsafe { t.detail.fun.as_ref() })
            .map_or(ptr::null(), |f| f.ret as *const hl_type);
        let fresh = Box::into_raw(Box::new(Kinds {
            args: k.args.clone(),
            ret: k.ret,
            type_index: k.type_index,
            ret_type,
        }));
        s.kinds.store(fresh, Ordering::Release);
    }
    // Strings cross into Haxe under the program's own `String`.
    if let Some(i) = bytecode.type_index_of("String") {
        proto::set_string_type(interpreter.c_type_of(i).cast());
    }
    *FACES.write().unwrap() = Some(Faces {
        by_class,
        fallback,
        by_type: HashMap::new(),
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Faces
// ---------------------------------------------------------------------------

/// The ref a face holds: its first field.
unsafe fn ref_field(face: *mut vdynamic) -> *mut *mut c_void {
    let t = unsafe { (*face).t };
    let rt = unsafe { hlp_get_obj_rt(t.cast()) };
    let offset = unsafe { *(*rt).fields_indexes } as usize;
    unsafe { (face as *mut u8).add(offset) as *mut *mut c_void }
}

/// Whether `t` is `caribou.Ref` or extends it. Names are compared in
/// place: this runs on every object crossing.
unsafe fn is_face_type(mut t: *const hl_type) -> bool {
    while !t.is_null() && matches!(unsafe { (*t).kind }, hl::HOBJ | hl::HSTRUCT) {
        if unsafe { proto::obj_name_is(t, REF_CLASS) } {
            return true;
        }
        let obj = unsafe { (*t).detail.obj };
        if obj.is_null() {
            return false;
        }
        t = unsafe { (*obj).super_ };
    }
    false
}

/// The foreign object `d` stands for, when `d` is a face holding one.
pub(crate) unsafe fn behind_face(d: *mut vdynamic) -> Option<Value> {
    if !unsafe { is_face_type((*d).t) } {
        return None;
    }
    let r = unsafe { *ref_field(d) };
    if r.is_null() {
        return None;
    }
    Some(wrenref::unwrap_foreign(unsafe {
        wrenref::wrenref_from_abstract(r)
    }))
}

/// The foreign object a face stands for.
unsafe fn behind(face: *mut vdynamic) -> Result<Value, String> {
    if face.is_null() {
        return Err("the receiver is null".to_owned());
    }
    let r = unsafe { *ref_field(face) };
    if r.is_null() {
        return Err("the receiver holds no object: its constructor did not run".to_owned());
    }
    Ok(wrenref::unwrap_foreign(unsafe {
        wrenref::wrenref_from_abstract(r)
    }))
}

/// Make `face` the face of `obj`.
unsafe fn bind_face(face: *mut vdynamic, obj: Value) {
    let r = wrenref::wrap_foreign(obj);
    unsafe { *ref_field(face) = wrenref::wrenref_as_abstract(r) };
    wrenref::set_face(r, face);
}

/// The class the program declares for the object's published type.
fn face_type(v: Value) -> Result<*mut hl_type, String> {
    let lang = bridge::language_of(v).ok_or("not an object")?;
    let type_name = bridge::type_name(v).unwrap_or_default();
    let mut faces = FACES.write().unwrap();
    let faces = faces.as_mut().ok_or("no program is loaded")?;
    let key = (lang, type_name.clone());
    if let Some(&t) = faces.by_type.get(&key) {
        return Ok(t as *mut hl_type);
    }
    let published = registry::class_for_type(lang, &type_name);
    let declared = published.as_ref().and_then(|(iface, index)| {
        let class = &iface.classes[*index].name;
        faces.by_class.iter().find_map(|((ns, module, c), &t)| {
            (c == class
                && registry::resolve(ns, module).as_ref()
                    == Some(&(iface.lang, iface.module.clone())))
            .then_some(t)
        })
    });
    let t = match declared {
        Some(t) => t,
        None if faces.fallback != 0 => faces.fallback,
        None => {
            return Err(format!(
                "no Haxe class stands for `{type_name}`, and the program has no `{REF_CLASS}`"
            ));
        }
    };
    faces.by_type.insert(key, t);
    Ok(t as *mut hl_type)
}

/// The Haxe object for a foreign one: its face, made on first need.
pub(crate) fn face_for(v: Value) -> Result<*mut vdynamic, String> {
    if let Some(r) = wrenref::foreign_ref(v)
        && let Some(face) = wrenref::face(r)
    {
        return Ok(face);
    }
    let t = face_type(v)?;
    let face = unsafe { hlp_alloc_obj(t.cast()) } as *mut vdynamic;
    unsafe { bind_face(face, v) };
    Ok(face)
}

// ---------------------------------------------------------------------------
// The call
// ---------------------------------------------------------------------------

/// The published class the slot names, loading its module on first use.
fn published(s: &Slot) -> Result<(Arc<Interface>, usize), String> {
    registry::lookup_class_or_load(&s.namespace, &s.module, &s.class)?
        .ok_or_else(|| format!("{}:{}.{} is not published", s.namespace, s.module, s.class))
}

fn static_member(class: &ClassIface, member: Symbol) -> Option<&registry::MethodIface> {
    class.methods.iter().find(|m| {
        m.is_static
            && matches!(m.target, caribou::protocol::Callable::WrenMethod { signature, .. } if signature == member)
    })
}

/// A record word as a value, by the kind the program declared for it
/// (`native_lib::HostNative::record`).
pub(crate) unsafe fn word_to_value(word: i64, kind: hl_type_kind) -> Value {
    match kind {
        hl::HVOID => Value::null(),
        hl::HUI8 => Value::int(i32::from(word as u8)),
        hl::HUI16 => Value::int(i32::from(word as u16)),
        hl::HI32 => Value::int(word as i32),
        hl::HI64 => Value::number(word as f64),
        hl::HF32 | hl::HF64 => Value::number(f64::from_bits(word as u64)),
        hl::HBOOL => Value::bool(word as u8 != 0),
        _ => unsafe { proto::dyn_to_value(word as *mut vdynamic) },
    }
}

/// A result as the record word the program reads by `kind`, which `ty`
/// is the type of when known; `None` for a value the kind cannot take.
pub(crate) unsafe fn value_to_word(
    v: Value,
    kind: hl_type_kind,
    ty: *const hl_type,
) -> Result<i64, String> {
    let int = || {
        v.as_int()
            .or_else(|| v.as_number().map(|n| n as i32))
            .or_else(|| v.as_bool().map(i32::from))
            .or_else(|| v.is_null().then_some(0))
    };
    let float = || {
        v.as_number()
            .or_else(|| v.as_int().map(f64::from))
            .or_else(|| v.is_null().then_some(0.0))
    };
    let word = match kind {
        hl::HVOID => Some(0),
        hl::HUI8 | hl::HUI16 | hl::HI32 => int().map(i64::from),
        hl::HI64 => int().map(i64::from),
        hl::HF32 | hl::HF64 => float().map(|n| n.to_bits() as i64),
        hl::HBOOL => v
            .as_bool()
            .or_else(|| v.is_null().then_some(false))
            .map(i64::from),
        // A function of the declared type, when the value is one.
        hl::HFUN if !ty.is_null() && bridge::arity(v).is_some() => {
            Some(crate::callback::function_for_typed(v, ty) as i64)
        }
        _ => return unsafe { proto::value_to_dyn(v, kind) }.map(|p| p as i64),
    };
    word.ok_or_else(|| format!("{} is not a {}", bridge::describe(v), kind_name(kind)))
}

fn kind_name(kind: hl_type_kind) -> &'static str {
    match kind {
        hl::HUI8 | hl::HUI16 | hl::HI32 | hl::HI64 => "Int",
        hl::HF32 | hl::HF64 => "Float",
        hl::HBOOL => "Bool",
        _ => "value of the declared type",
    }
}

/// A number or a bool: what a `Dynamic` boxes.
fn is_scalar(v: Value) -> bool {
    v.is_number() || v.is_int() || v.as_bool().is_some()
}

/// Run the call for the slot on the record `words`: the result, or what
/// to throw. Everything owned here is dropped before the throw.
unsafe fn run(s: &Slot, kinds: &Kinds, words: *const i64) -> Result<Value, *mut vdynamic> {
    let haxe = lang();
    let words = unsafe { std::slice::from_raw_parts(words, kinds.args.len()) };
    let (receiver, params, kinds_of) = if s.kind.takes_receiver() {
        (words[0] as *mut vdynamic, &words[1..], &kinds.args[1..])
    } else {
        (ptr::null_mut(), words, &kinds.args[..])
    };
    // On the stack, where the conservative scan sees them across the call;
    // only the slots in use are written.
    let mut args = [MaybeUninit::<Value>::uninit(); MAX_ARGS];
    for (slot, (&w, &k)) in args.iter_mut().zip(params.iter().zip(kinds_of)) {
        let v = unsafe { word_to_value(w, k) };
        if k == hl::HDYN && is_scalar(v) {
            s.boxed_in.fetch_add(1, Ordering::Relaxed);
        }
        slot.write(v);
    }
    let args = unsafe { args[..params.len()].assume_init_ref() };

    let result = match s.kind {
        Kind::Len | Kind::Index | Kind::SetIndex => {
            // A face's object, else the Haxe object itself, wrapped: a
            // Haxe array is a sequence to the bridge as it is.
            let target = unsafe { behind_face(receiver) }.unwrap_or_else(|| proto::wrap(receiver));
            match s.kind {
                Kind::Len => bridge::len(target, haxe).map(|n| Value::int(n as i32)),
                Kind::Index => bridge::index(target, args[0], haxe),
                _ => bridge::set_index(target, args[0], args[1], haxe).map(|()| Value::null()),
            }
        }
        Kind::Method => unsafe { behind(receiver) }
            .map_err(|m| proto::error_value(&s.name, &m))
            .and_then(|target| bridge::invoke_at(target, s.member, &s.site, args, haxe)),
        Kind::Get => unsafe { behind(receiver) }
            .map_err(|m| proto::error_value(&s.name, &m))
            .and_then(|target| bridge::get_at(target, s.member, &s.site, haxe)),
        Kind::Set => unsafe { behind(receiver) }
            .map_err(|m| proto::error_value(&s.name, &m))
            .and_then(|target| {
                bridge::set_at(target, s.member, &s.site, args[0], haxe).map(|()| Value::null())
            }),
        Kind::Static => s
            .target(|| {
                let (iface, index) = published(s)?;
                static_member(&iface.classes[index], s.member)
                    .map(|m| m.target)
                    .ok_or_else(|| {
                        format!(
                            "{}:{}.{} has no static {}",
                            s.namespace,
                            s.module,
                            s.class,
                            s.member.name()
                        )
                    })
            })
            .map_err(|m| proto::error_value(&s.name, &m))
            .and_then(|target| bridge::call_at(target, &s.site, args, haxe, &s.name)),
        Kind::Init => s
            .target(|| {
                let (iface, index) = published(s)?;
                iface.classes[index]
                    .ctor
                    .as_ref()
                    .map(|c| c.target)
                    .ok_or_else(|| {
                        format!(
                            "{}:{}.{} has no constructor",
                            s.namespace, s.module, s.class
                        )
                    })
            })
            .map_err(|m| proto::error_value(&s.name, &m))
            .and_then(|target| bridge::call_at(target, &s.site, args, haxe, &s.name))
            .map(|obj| {
                unsafe { bind_face(receiver, obj) };
                Value::null()
            }),
    };
    result.map_err(proto::throwable)
}

/// The one entry behind every native, called by record with the slot
/// `bind` registered as its context: the words are read by the kinds the
/// program declared, and the result goes back as the word it reads by
/// the declared return kind. A value the declaration cannot take is
/// thrown.
unsafe extern "C" fn entry(slot: *const Slot, words: *const i64) -> i64 {
    let s = unsafe { &*slot };
    let kinds =
        unsafe { s.kinds.load(Ordering::Acquire).as_ref() }.expect("a bound slot has its kinds");
    let thrown = match unsafe { run(s, kinds, words) } {
        Ok(v) => {
            if kinds.ret == hl::HDYN && is_scalar(v) {
                s.boxed_out.fetch_add(1, Ordering::Relaxed);
            }
            match unsafe { value_to_word(v, kinds.ret, kinds.ret_type) } {
                Ok(word) => return word,
                Err(m) => proto::throwable(proto::error_value(&s.name, &m)),
            }
        }
        Err(thrown) => thrown,
    };
    unsafe { hlp_throw(thrown.cast()) };
    std::process::abort()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parse_into_their_parts() {
        let s = slot_for("game:hud.Hud.draw(_)").unwrap();
        assert_eq!(
            (s.namespace.as_str(), s.module.as_str(), s.class.as_str()),
            ("game", "hud", "Hud")
        );
        assert_eq!((s.kind, s.member.name()), (Kind::Method, "draw(_)"));
        let s = slot_for("game:ui/hud.Hud.score").unwrap();
        assert_eq!(
            (s.module.as_str(), s.kind, s.member.name()),
            ("ui/hud", Kind::Get, "score")
        );
        let s = slot_for("game:hud.Hud.score=(_)").unwrap();
        assert_eq!((s.kind, s.member.name()), (Kind::Set, "score"));
        let s = slot_for("game:hud.Hud.static:make(_,_)").unwrap();
        assert_eq!((s.kind, s.member.name()), (Kind::Static, "make(_,_)"));
        let s = slot_for("game:hud.Hud.construct:new()").unwrap();
        assert_eq!((s.kind, s.member.name()), (Kind::Init, "new()"));
        for bad in ["draw(_)", "game:Hud.draw(_)", "game:hud.Hud.", ":hud.Hud.x"] {
            assert!(slot_for(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn words_round_trip_by_kind() {
        unsafe {
            assert_eq!(word_to_value(7, hl::HI32).as_int(), Some(7));
            assert_eq!(
                word_to_value(-1i32 as u32 as i64, hl::HI32).as_int(),
                Some(-1)
            );
            assert_eq!(word_to_value(1, hl::HBOOL).as_bool(), Some(true));
            let bits = 2.5f64.to_bits() as i64;
            assert_eq!(word_to_value(bits, hl::HF64).as_number(), Some(2.5));
            assert_eq!(
                value_to_word(Value::number(2.5), hl::HF64, ptr::null()),
                Ok(bits)
            );
            assert_eq!(
                value_to_word(Value::int(3), hl::HF64, ptr::null()),
                Ok(3.0f64.to_bits() as i64)
            );
            assert_eq!(
                value_to_word(Value::bool(true), hl::HBOOL, ptr::null()),
                Ok(1)
            );
            assert_eq!(value_to_word(Value::null(), hl::HVOID, ptr::null()), Ok(0));
            assert!(value_to_word(Value::bool(true), hl::HF64, ptr::null()).is_err());
        }
    }
}
