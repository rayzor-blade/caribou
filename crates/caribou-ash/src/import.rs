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
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
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
use caribou::symbol::intern;
use caribou_abi::hl::{self, hl_type, vdynamic};
use caribou_abi::{LangId, Value};

use crate::proto::{self, lang};
use crate::wrenref;

/// The library the natives name.
pub const LIB: &str = "caribou";
/// The class every face extends, whose first field holds the ref.
pub const REF_CLASS: &str = "caribou.Ref";

/// Arguments a native declares, receiver included: `ash_native_call`'s
/// limit less the context word.
const MAX_ARGS: usize = 7;

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
}

impl Kind {
    fn takes_receiver(self) -> bool {
        !matches!(self, Kind::Static)
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
}

#[derive(Debug)]
struct Resolved {
    generation: u64,
    target: Callable,
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
    })
}

/// What a native declares: how many arguments, and how it returns. The
/// arguments are `Dynamic`; the result is typed where the Haxe library
/// could, so a number comes back in a register and not in a box.
#[cfg(feature = "runner")]
fn declared(bytecode: &DecodedBytecode, type_index: usize) -> Option<(usize, Returns)> {
    let fun = bytecode.types.get(type_index)?.fun.as_ref()?;
    let ret = match bytecode.types.get(fun.ret.0)?.kind {
        hl::HF64 => Returns::Float,
        hl::HBOOL => Returns::Bool,
        hl::HVOID => Returns::Nothing,
        _ => Returns::Boxed,
    };
    Some((fun.args.len(), ret))
}

/// The register a native's result comes back in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Returns {
    /// A pointer: a boxed dynamic, or nothing.
    Boxed,
    Nothing,
    Float,
    Bool,
}

/// Give every `caribou` native the program declares its entry and slot:
/// the map ash's resolver takes before it looks for libraries.
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
        let (nargs, returns) = declared(bytecode, native.type_.0)
            .ok_or_else(|| anyhow!("`{}` has no function type", native.name))?;
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
        let entries = match returns {
            Returns::Boxed | Returns::Nothing => &ENTRIES,
            Returns::Float => &ENTRIES_F64,
            Returns::Bool => &ENTRIES_BOOL,
        };
        natives.insert(
            (LIB.to_owned(), native.name.clone()),
            HostNative {
                addr: entries[nargs] as usize,
                context: Arc::as_ptr(&table[slot]) as usize,
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

/// Whether `t` is `caribou.Ref` or extends it.
unsafe fn is_face_type(mut t: *const hl_type) -> bool {
    while let Some(name) = unsafe { proto::obj_name(t) } {
        if name == REF_CLASS {
            return true;
        }
        t = unsafe { (*(*t).detail.obj).super_ };
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

/// Run the call for the slot: the result, or what to throw. Everything
/// owned here is dropped before the throw.
unsafe fn run(s: &Slot, raw: &[*mut vdynamic]) -> Result<Value, *mut vdynamic> {
    let haxe = lang();
    let (receiver, params) = if s.kind.takes_receiver() {
        (raw[0], &raw[1..])
    } else {
        (ptr::null_mut(), raw)
    };
    // On the stack, where the conservative scan sees them across the call.
    let mut args = [Value::null(); MAX_ARGS];
    for (slot, &p) in args.iter_mut().zip(params) {
        *slot = unsafe { proto::dyn_to_value(p) };
    }
    let args = &args[..params.len()];

    let result = match s.kind {
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

/// `slot` is the context word ash passes first: the slot `bind`
/// registered. The result as the native declared it, through `convert`;
/// a value the declaration cannot take is thrown.
unsafe fn enter<R>(
    slot: *const Slot,
    raw: &[*mut vdynamic],
    convert: impl FnOnce(Value) -> Result<R, String>,
) -> R {
    let s = unsafe { &*slot };
    let thrown = match unsafe { run(s, raw) } {
        Ok(v) => match convert(v) {
            Ok(r) => return r,
            Err(m) => proto::throwable(proto::error_value(&s.name, &m)),
        },
        Err(thrown) => thrown,
    };
    unsafe { hlp_throw(thrown.cast()) };
    std::process::abort()
}

fn boxed(v: Value) -> Result<*mut vdynamic, String> {
    unsafe { proto::value_to_dyn(v, hl::HDYN) }
}

fn float(v: Value) -> Result<f64, String> {
    v.as_number()
        .or_else(|| v.as_int().map(f64::from))
        .or_else(|| v.is_null().then_some(0.0))
        .ok_or_else(|| format!("{} is not a Float", bridge::describe(v)))
}

fn boolean(v: Value) -> Result<bool, String> {
    v.as_bool()
        .or_else(|| v.is_null().then_some(false))
        .ok_or_else(|| format!("{} is not a Bool", bridge::describe(v)))
}

// ---------------------------------------------------------------------------
// Entries: one per argument count and result register, the slot first
// ---------------------------------------------------------------------------

macro_rules! entry {
    ($name:ident, $ret:ty, $convert:expr; $($a:ident),*) => {
        unsafe extern "C" fn $name(slot: *const Slot, $($a: *mut vdynamic),*) -> $ret {
            unsafe { enter(slot, &[$($a),*], $convert) }
        }
    };
}

macro_rules! entries {
    ($table:ident, $ret:ty, $convert:expr; $($name:ident: $($a:ident),*;)*) => {
        $(entry!($name, $ret, $convert; $($a),*);)*
        static $table: Entries = Entries([$($name as *const c_void),*]);
    };
}

entries!(ENTRIES, *mut vdynamic, boxed;
    entry0: ;
    entry1: a0;
    entry2: a0, a1;
    entry3: a0, a1, a2;
    entry4: a0, a1, a2, a3;
    entry5: a0, a1, a2, a3, a4;
    entry6: a0, a1, a2, a3, a4, a5;
    entry7: a0, a1, a2, a3, a4, a5, a6;
);

entries!(ENTRIES_F64, f64, float;
    entry_f0: ;
    entry_f1: a0;
    entry_f2: a0, a1;
    entry_f3: a0, a1, a2;
    entry_f4: a0, a1, a2, a3;
    entry_f5: a0, a1, a2, a3, a4;
    entry_f6: a0, a1, a2, a3, a4, a5;
    entry_f7: a0, a1, a2, a3, a4, a5, a6;
);

entries!(ENTRIES_BOOL, bool, boolean;
    entry_b0: ;
    entry_b1: a0;
    entry_b2: a0, a1;
    entry_b3: a0, a1, a2;
    entry_b4: a0, a1, a2, a3;
    entry_b5: a0, a1, a2, a3, a4;
    entry_b6: a0, a1, a2, a3, a4, a5;
    entry_b7: a0, a1, a2, a3, a4, a5, a6;
);

struct Entries([*const c_void; MAX_ARGS + 1]);

// Code addresses.
unsafe impl Sync for Entries {}

impl std::ops::Index<usize> for Entries {
    type Output = *const c_void;
    fn index(&self, nargs: usize) -> &*const c_void {
        &self.0[nargs]
    }
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
    fn every_entry_is_distinct() {
        let mut seen = std::collections::HashSet::new();
        for table in [&ENTRIES, &ENTRIES_F64, &ENTRIES_BOOL] {
            for nargs in 0..=MAX_ARGS {
                assert!(seen.insert(table[nargs] as usize));
            }
        }
    }
}
