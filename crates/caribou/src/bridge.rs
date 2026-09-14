//! The call bridge: how a value in one language calls into another.
//!
//! Every crossing is a protected call. No unwinder crosses a boundary: an
//! error leaving a callee's segment is a pending `Error` value on the
//! current task plus one trace frame, and the receiving language re-raises
//! it natively. A value returning to the language that raised it is
//! unwrapped to the original object, so identity survives a round trip.
//!
//! An object `Value` handed to the bridge has a `TypeDesc` at word zero;
//! that is the protocol's contract, and an adapter whose native objects
//! carry a bare `hl_type` wraps them before they cross.

use core::ffi::c_void;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::RwLock;
use std::sync::atomic::{AtomicPtr, Ordering};

use caribou_abi::hl::{self, hl_type, hl_type_fun, hl_type_kind};
use caribou_abi::{ErrorKind, LangId, Value};

use crate::error::{Error, Rooted};
use crate::heap::{self, Handle};
use crate::protocol::{self, CallSite, Callable, Fault, Reply, Symbol, desc_of};
use crate::sched::{self, TaskId};
use crate::world::LANG_CORE;

// ---------------------------------------------------------------------------
// The pending error, one slot per task
// ---------------------------------------------------------------------------

/// A pending error, rooted while it waits: the slot is not a heap root and
/// the value may be the only reference.
struct Pending {
    value: Value,
    root: Handle,
}

impl Drop for Pending {
    fn drop(&mut self) {
        heap::handle_release(self.root);
    }
}

thread_local! {
    /// Keyed by task rather than kept in the task's host state: a task has
    /// one host-state slot and it belongs to the adapter that spawned it.
    /// A task never leaves the world that made it, so this thread's map
    /// holds exactly one slot per task of this world.
    static PENDING: RefCell<HashMap<TaskId, Pending>> = RefCell::new(HashMap::new());
}

/// Past this many slots an insert first drops the entries of tasks that
/// finished without their error being taken.
const PENDING_SWEEP_AT: usize = 16;

/// Make `err` the current task's pending error, replacing any that was not
/// taken. A protocol entry calls this and then returns `REPLY_RAISED`.
pub fn set_pending(err: Value) {
    let root = match err.as_object() {
        Some(p) if !p.is_null() => heap::handle_new(p as *mut u8),
        _ => Handle::NULL,
    };
    let task = sched::current_task();
    let previous = PENDING.with(|slots| {
        let mut slots = slots.borrow_mut();
        if slots.len() >= PENDING_SWEEP_AT {
            slots.retain(|id, _| sched::task_exists(*id));
        }
        slots.insert(task, Pending { value: err, root })
    });
    drop(previous);
}

/// Take the current task's pending error, clearing the slot. The value is
/// no longer rooted by the slot; root it before allocating.
pub fn take_pending() -> Option<Value> {
    take_pending_rooted().map(|rooted| rooted.value())
}

pub fn has_pending() -> bool {
    let task = sched::current_task();
    PENDING.with(|slots| slots.borrow().contains_key(&task))
}

/// `take_pending` keeping the slot's root, for the bridge's own use.
fn take_pending_rooted() -> Option<Rooted> {
    let task = sched::current_task();
    let pending = PENDING.with(|slots| slots.borrow_mut().remove(&task))?;
    let pending = ManuallyDrop::new(pending);
    Some(Rooted::from_parts(pending.value, pending.root))
}

/// Set `err` pending and answer `REPLY_RAISED`: what a protocol entry
/// returns to raise.
pub fn raise(err: *mut Error) -> u8 {
    set_pending(Error::value(err));
    protocol::REPLY_RAISED
}

// ---------------------------------------------------------------------------
// Guards: where a language that leaves its code by a long jump lands
// ---------------------------------------------------------------------------

/// Run `body(ctx)` where a throw in this language's code lands. Answers
/// `REPLY_OK` when the body returned, `REPLY_RAISED` when a throw landed
/// here, the error pending.
pub type Guard = unsafe extern "C-unwind" fn(
    body: unsafe extern "C-unwind" fn(*mut c_void),
    ctx: *mut c_void,
) -> u8;

/// The one guard a process has: one language leaves by a long jump.
static GUARD: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());

/// What a thread knows about the runs it is in.
struct Runs {
    /// How many guards the thread is under.
    guarded: Cell<u32>,
    /// The site of the innermost run entered without a guard, for a
    /// crossing inside it to mark, else null.
    entry: Cell<*const CallSite>,
}

thread_local! {
    static RUNS: Runs = const {
        Runs {
            guarded: Cell::new(0),
            entry: Cell::new(ptr::null()),
        }
    };
}

/// Register the guard for the language that leaves its code by a long
/// jump, replacing any earlier one.
pub fn set_guard(f: Guard) {
    GUARD.store(f as *mut (), Ordering::Release);
}

/// Whether the caller runs under a guard: a throw in the guarded
/// language's code lands there, so nothing between need catch it.
#[inline]
pub fn guarded() -> bool {
    RUNS.with(|r| r.guarded.get() > 0)
}

/// Run `body(ctx)` as a run entered from `site`, when the caller keeps
/// one. The run is under the guard when the thread is under one
/// already, so a throw cannot land above this run, or when a send from
/// the site has called back before; `prepare` runs first then, for
/// whatever the caller wants to put back after a throw. Otherwise the
/// run goes without, each crossing inside catching for itself, and the
/// first crossing marks the site (`note_reentry`) so the next run from
/// it is guarded. False when a throw landed in the guard, the error
/// pending.
///
/// # Safety
/// `ctx` is whatever `body` takes.
pub unsafe fn enter(
    site: Option<&CallSite>,
    prepare: impl FnOnce(),
    body: unsafe extern "C-unwind" fn(*mut c_void),
    ctx: *mut c_void,
) -> bool {
    RUNS.with(|r| {
        let site = match site {
            Some(site) if !site.reentrant() && r.guarded.get() == 0 => site,
            _ => {
                let f = GUARD.load(Ordering::Acquire);
                if f.is_null() {
                    unsafe { body(ctx) };
                    return true;
                }
                let guard: Guard = unsafe { std::mem::transmute::<*mut (), Guard>(f) };
                prepare();
                r.guarded.set(r.guarded.get() + 1);
                let code = unsafe { guard(body, ctx) };
                r.guarded.set(r.guarded.get() - 1);
                return code == protocol::REPLY_OK;
            }
        };
        let previous = r.entry.replace(site);
        unsafe { body(ctx) };
        r.entry.set(previous);
        true
    })
}

/// A crossing back into the guarded language from a run entered without
/// the guard: the run's site is marked, so the next run from it is
/// guarded and the crossings inside it need not catch for themselves.
#[inline]
pub fn note_reentry() {
    RUNS.with(|r| {
        let entry = r.entry.get();
        if !entry.is_null() {
            unsafe { &*entry }.note_reentrant();
        }
    });
}

/// Run `body(ctx)` under the guard, when there is one. False when a
/// throw landed in the guard, the error pending; the frames between the
/// throw and the guard are gone, so the caller puts back whatever it
/// keeps per thread.
///
/// # Safety
/// `ctx` is whatever `body` takes.
pub unsafe fn run_guarded(
    body: unsafe extern "C-unwind" fn(*mut c_void),
    ctx: *mut c_void,
) -> bool {
    let f = GUARD.load(Ordering::Acquire);
    if f.is_null() {
        unsafe { body(ctx) };
        return true;
    }
    let guard: Guard = unsafe { std::mem::transmute::<*mut (), Guard>(f) };
    let code = RUNS.with(|r| {
        r.guarded.set(r.guarded.get() + 1);
        let code = unsafe { guard(body, ctx) };
        r.guarded.set(r.guarded.get() - 1);
        code
    });
    code == protocol::REPLY_OK
}

// ---------------------------------------------------------------------------
// Typed dispatch, one dispatcher per language
// ---------------------------------------------------------------------------

/// Calls `func` with `nargs` `Value`s marshalled by `sig`, an `hl_type` of
/// kind `HFUN`, and writes the result to `out` as a `Value`. Returns a
/// reply code; on `REPLY_RAISED` the error is pending.
/// `site` is the caller's call site when it keeps one, else null; the
/// dispatcher may leave a direct send in it for the next call.
pub type TypedDispatch = unsafe extern "C-unwind" fn(
    func: *const c_void,
    sig: *const hl_type,
    site: *mut CallSite,
    args: *const Value,
    nargs: usize,
    out: *mut Value,
) -> u8;

/// The dispatchers by language id, read on every typed call: a table of
/// atomics for the ids a process has, the lock only past it.
const DISPATCH_TABLE: usize = 64;
static DISPATCH_BY_LANG: [AtomicPtr<()>; DISPATCH_TABLE] =
    [const { AtomicPtr::new(ptr::null_mut()) }; DISPATCH_TABLE];
static DISPATCHERS: RwLock<Vec<(LangId, TypedDispatch)>> = RwLock::new(Vec::new());

/// Register the dispatcher for typed callables of `lang`, replacing any
/// earlier one. The core's own language has a default.
pub fn set_typed_dispatch(lang: LangId, f: TypedDispatch) {
    if let Some(slot) = DISPATCH_BY_LANG.get(lang as usize) {
        slot.store(f as *mut (), Ordering::Release);
        return;
    }
    let mut table = DISPATCHERS.write().unwrap();
    match table.iter_mut().find(|(l, _)| *l == lang) {
        Some(entry) => entry.1 = f,
        None => table.push((lang, f)),
    }
}

/// The dispatcher for `lang`, if one is registered.
pub fn typed_dispatch(lang: LangId) -> Option<TypedDispatch> {
    let registered = match DISPATCH_BY_LANG.get(lang as usize) {
        Some(slot) => {
            let f = slot.load(Ordering::Acquire);
            (!f.is_null()).then(|| unsafe { std::mem::transmute::<*mut (), TypedDispatch>(f) })
        }
        None => DISPATCHERS
            .read()
            .unwrap()
            .iter()
            .find(|(l, _)| *l == lang)
            .map(|(_, f)| *f),
    };
    registered.or((lang == LANG_CORE).then_some(core_dispatch as TypedDispatch))
}

/// The `hl_type_fun` behind a signature, if it is a function type.
unsafe fn fun_of(sig: *const hl_type) -> Option<*const hl_type_fun> {
    let sig = unsafe { sig.as_ref()? };
    if sig.kind != hl::HFUN && sig.kind != hl::HMETHOD {
        return None;
    }
    let fun = unsafe { sig.detail.fun };
    (!fun.is_null()).then_some(fun as *const hl_type_fun)
}

unsafe fn arg_kind(fun: *const hl_type_fun, i: usize) -> hl_type_kind {
    unsafe { (**(*fun).args.add(i)).kind }
}

unsafe fn ret_kind(fun: *const hl_type_fun) -> hl_type_kind {
    unsafe { (*(*fun).ret).kind }
}

/// How an argument travels on the C ABI: an integer-class register or slot,
/// or a floating-point one. Every integer-class argument shares a register
/// whatever its width, so `i32`, `bool` and a `Value` are one class.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    I,
    F,
}

#[derive(Clone, Copy)]
enum Ret {
    Void,
    Int,
    Float,
}

#[derive(Clone, Copy)]
union Slot {
    int: i64,
    float: f64,
}

/// What the core dispatcher returns to `core_dispatch` before the result is
/// shaped by the return kind.
#[derive(Clone, Copy)]
enum Raw {
    Void,
    Int(i64),
    Float(f64),
}

macro_rules! slot_ty {
    (I) => {
        i64
    };
    (F) => {
        f64
    };
}

/// Expanded inside the call's `unsafe` block: reading the union is part of
/// the same trust as the call.
macro_rules! slot_arg {
    (I, $it:ident) => {
        $it.next().unwrap().int
    };
    (F, $it:ident) => {
        $it.next().unwrap().float
    };
}

/// One arm per argument-class tuple and return class: the C function type
/// the callee is transmuted to. Positional, so it holds on every native ABI
/// the core runs on.
macro_rules! call_by_class {
    ($func:expr, $ret:expr, $slots:expr, $classes:expr; $( [ $( $c:ident ),* ] )* ) => {{
        let mut it = $slots.iter();
        match ($classes, $ret) {
            $(
                ([$(Class::$c),*], Ret::Void) => {
                    let f: unsafe extern "C-unwind" fn($(slot_ty!($c)),*) =
                        unsafe { core::mem::transmute($func) };
                    unsafe { f($(slot_arg!($c, it)),*) };
                    Some(Raw::Void)
                }
                ([$(Class::$c),*], Ret::Int) => {
                    let f: unsafe extern "C-unwind" fn($(slot_ty!($c)),*) -> i64 =
                        unsafe { core::mem::transmute($func) };
                    Some(Raw::Int(unsafe { f($(slot_arg!($c, it)),*) }))
                }
                ([$(Class::$c),*], Ret::Float) => {
                    let f: unsafe extern "C-unwind" fn($(slot_ty!($c)),*) -> f64 =
                        unsafe { core::mem::transmute($func) };
                    Some(Raw::Float(unsafe { f($(slot_arg!($c, it)),*) }))
                }
            )*
            _ => None,
        }
    }};
}

const CORE_MAX_ARGS: usize = 4;

/// The default for `LANG_CORE`: up to four arguments of kinds `HI32`,
/// `HBOOL`, `HF64` and `HDYN`, returning `HVOID`, `HI32`, `HBOOL`, `HF64` or
/// `HDYN`. For the core's own callables and tests; a language with a real
/// marshaller registers its own.
unsafe extern "C-unwind" fn core_dispatch(
    func: *const c_void,
    sig: *const hl_type,
    _site: *mut CallSite,
    args: *const Value,
    nargs: usize,
    out: *mut Value,
) -> u8 {
    let Some(fun) = (unsafe { fun_of(sig) }) else {
        return raise(Error::new(
            ErrorKind::Type,
            "signature is not a function type",
            LANG_CORE,
        ));
    };
    if unsafe { (*fun).nargs } as usize != nargs || nargs > CORE_MAX_ARGS {
        return raise(Error::new(
            ErrorKind::Type,
            &format!("the core dispatcher takes at most {CORE_MAX_ARGS} arguments, not {nargs}"),
            LANG_CORE,
        ));
    }
    let args = unsafe { core::slice::from_raw_parts(args, nargs) };
    let mut classes = [Class::I; CORE_MAX_ARGS];
    let mut slots = [Slot { int: 0 }; CORE_MAX_ARGS];
    for (i, &arg) in args.iter().enumerate() {
        let kind = unsafe { arg_kind(fun, i) };
        let converted = match kind {
            hl::HI32 => arg
                .as_int()
                .or_else(|| arg.as_number().map(|n| n as i32))
                .map(|n| (Class::I, Slot { int: n as i64 })),
            hl::HBOOL => arg.as_bool().map(|b| (Class::I, Slot { int: b as i64 })),
            hl::HF64 => arg
                .as_number()
                .or_else(|| arg.as_int().map(f64::from))
                .map(|n| (Class::F, Slot { float: n })),
            hl::HDYN => Some((
                Class::I,
                Slot {
                    int: arg.to_bits() as i64,
                },
            )),
            _ => {
                return raise(Error::new(
                    ErrorKind::Type,
                    &format!(
                        "argument {i} has kind {kind}, which the core dispatcher does not marshal"
                    ),
                    LANG_CORE,
                ));
            }
        };
        match converted {
            Some((class, slot)) => {
                classes[i] = class;
                slots[i] = slot;
            }
            None => {
                return raise(Error::new(
                    ErrorKind::Type,
                    &format!("argument {i} expects kind {kind}, got {}", describe(arg)),
                    LANG_CORE,
                ));
            }
        }
    }
    let ret_kind = unsafe { ret_kind(fun) };
    let ret = match ret_kind {
        hl::HVOID => Ret::Void,
        hl::HI32 | hl::HBOOL | hl::HDYN => Ret::Int,
        hl::HF64 => Ret::Float,
        _ => {
            return raise(Error::new(
                ErrorKind::Type,
                &format!("return kind {ret_kind} is not one the core dispatcher marshals"),
                LANG_CORE,
            ));
        }
    };
    let classes = &classes[..nargs];
    let slots = &slots[..nargs];
    let raw = call_by_class!(func, ret, slots, classes;
        []
        [I] [F]
        [I, I] [I, F] [F, I] [F, F]
        [I, I, I] [I, I, F] [I, F, I] [I, F, F] [F, I, I] [F, I, F] [F, F, I] [F, F, F]
        [I, I, I, I] [I, I, I, F] [I, I, F, I] [I, I, F, F] [I, F, I, I] [I, F, I, F] [I, F, F, I] [I, F, F, F]
        [F, I, I, I] [F, I, I, F] [F, I, F, I] [F, I, F, F] [F, F, I, I] [F, F, I, F] [F, F, F, I] [F, F, F, F]
    );
    let Some(raw) = raw else {
        return protocol::REPLY_UNSUPPORTED;
    };
    let result = match (raw, ret_kind) {
        (Raw::Int(n), hl::HI32) => Value::int(n as i32),
        (Raw::Int(n), hl::HBOOL) => Value::bool(n as u8 != 0),
        (Raw::Int(n), _) => Value::from_bits(n as u64),
        (Raw::Float(n), _) => Value::number(n),
        (Raw::Void, _) => Value::null(),
    };
    unsafe { *out = result };
    protocol::REPLY_OK
}

// ---------------------------------------------------------------------------
// Calls
// ---------------------------------------------------------------------------

/// What came back across a boundary.
enum Outcome {
    Ok(Value),
    /// The callee raised; the error is pending.
    Raised,
    /// The core refuses or the callee answered with a fault; an error of
    /// this kind is built here.
    Fault(ErrorKind, String),
    Panic(String),
}

/// Run `f` inside the protected boundary: a fault is described by
/// `on_fault`, a panic is caught.
#[inline]
fn protected(
    f: impl FnOnce() -> Reply,
    on_fault: impl FnOnce(Fault) -> (ErrorKind, String),
) -> Outcome {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Outcome::Ok(v),
        Ok(Err(Fault::Raised)) => Outcome::Raised,
        Ok(Err(fault)) => {
            let (kind, message) = on_fault(fault);
            Outcome::Fault(kind, message)
        }
        Err(payload) => Outcome::Panic(panic_message(payload.as_ref())),
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_owned()
    }
}

/// Turn an outcome into the bridge's result. Every error leaving here is an
/// `Error` with one more frame for `segment`; one whose native payload
/// belongs to `caller` is unwrapped to that payload.
fn settle(outcome: Outcome, segment: LangId, name: &str, caller: LangId) -> Result<Value, Value> {
    let raised: Rooted = match outcome {
        Outcome::Ok(v) => return Ok(v),
        Outcome::Raised => take_pending_rooted().unwrap_or_else(|| {
            Error::new_rooted(
                ErrorKind::Internal,
                "the callee reported a raise but left no error pending",
                segment,
            )
        }),
        Outcome::Fault(kind, message) => Error::new_rooted(kind, &message, LANG_CORE),
        Outcome::Panic(message) => {
            let e = Error::new_rooted(ErrorKind::Internal, &message, segment);
            // An entry that set its error and then panicked keeps it as the cause.
            if let Some(pending) = take_pending_rooted() {
                unsafe { Error::with_cause(e.ptr() as *mut Error, pending.value()) };
            }
            e
        }
    };
    // A pending value that is not an `Error` is a language's bare error
    // object; it travels wrapped, so it can still be unwrapped at home.
    let err = match unsafe { Error::from_value(raised.value()) } {
        Some(_) => raised,
        None => Error::with_native_rooted(raised.value(), segment),
    };
    let e = err.ptr() as *mut Error;
    unsafe { Error::push_segment(e, segment, name) };
    let native = unsafe { Error::native(e) };
    if !native.is_null() && unsafe { Error::origin(e) } == caller {
        return Err(native);
    }
    Err(err.value())
}

/// A description of `v` for a message: its type's name when it has one.
pub fn describe(v: Value) -> String {
    if v.is_null() {
        "null".to_owned()
    } else if v.is_int() {
        "an int".to_owned()
    } else if v.is_number() {
        "a number".to_owned()
    } else if v.is_bool() {
        "a bool".to_owned()
    } else if let Some(obj) = object_of(v) {
        let name = unsafe { desc_name(obj) };
        if name.is_empty() {
            "an object".to_owned()
        } else {
            format!("a {name}")
        }
    } else {
        "an undefined value".to_owned()
    }
}

/// The object behind `v`, if it is a non-null object.
#[inline]
fn object_of(v: Value) -> Option<*mut u8> {
    match v.as_object() {
        Some(p) if !p.is_null() => Some(p as *mut u8),
        _ => None,
    }
}

unsafe fn desc_name<'a>(obj: *mut u8) -> &'a str {
    let Some(desc) = (unsafe { desc_of(obj).as_ref() }) else {
        return "";
    };
    if desc.name.is_null() || desc.name_len == 0 {
        return "";
    }
    unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(desc.name, desc.name_len)) }
}

/// The language that defines `obj`'s type.
#[inline]
unsafe fn lang_of(obj: *mut u8) -> LangId {
    unsafe { desc_of(obj).as_ref() }.map_or(LANG_CORE, |d| d.lang)
}

/// The language of `v`'s type, for an object.
pub fn language_of(v: Value) -> Option<LangId> {
    object_of(v).map(|obj| unsafe { lang_of(obj) })
}

/// The name `v`'s own language gives its type, when its protocol answers
/// `type_name`, as the interned symbol. Nothing is raised: an entry that
/// raises is `None`, and its pending error is dropped.
pub fn type_symbol(v: Value) -> Option<Symbol> {
    let obj = object_of(v)?;
    let outcome = protected(
        || unsafe { protocol::Send::type_name(obj).map(|s| Value::int(s.0 as i32)) },
        |_| (ErrorKind::Type, String::new()),
    );
    match outcome {
        Outcome::Ok(id) => Some(Symbol(id.as_int()? as u32)),
        Outcome::Raised => {
            drop(take_pending_rooted());
            None
        }
        _ => None,
    }
}

/// [`type_symbol`] as text.
pub fn type_name(v: Value) -> Option<String> {
    type_symbol(v).map(|s| s.name().to_owned())
}

/// How many arguments `v` takes when it is a function of its language,
/// else `None`. Nothing is raised: an entry that raises is `None`, and its
/// pending error is dropped.
pub fn arity(v: Value) -> Option<usize> {
    let obj = object_of(v)?;
    let outcome = protected(
        || unsafe { protocol::Send::arity(obj).map(|n| Value::int(n as i32)) },
        |_| (ErrorKind::Type, String::new()),
    );
    match outcome {
        Outcome::Ok(n) => n.as_int().map(|n| n as usize),
        Outcome::Raised => {
            drop(take_pending_rooted());
            None
        }
        _ => None,
    }
}

/// Whether `v` is a sequence of its language: its protocol answers `len`.
/// Nothing is raised; an entry that raises is `false`.
pub fn is_sequence(v: Value) -> bool {
    len(v, LANG_CORE).is_ok()
}

/// How many elements the sequence `v` has.
pub fn len(v: Value, caller: LangId) -> Result<usize, Value> {
    let Some(obj) = object_of(v) else {
        return Err(sequence_error(v, "len", caller));
    };
    let segment = unsafe { lang_of(obj) };
    let outcome = protected(
        || unsafe { protocol::Send::len(obj).map(|n| Value::int(n as i32)) },
        |fault| sequence_fault(fault, v, "len"),
    );
    settle(outcome, segment, "len", caller).map(|n| n.as_int().unwrap_or(0).max(0) as usize)
}

/// The element of `v` at `key`.
pub fn index(v: Value, key: Value, caller: LangId) -> Result<Value, Value> {
    let Some(obj) = object_of(v) else {
        return Err(sequence_error(v, "index", caller));
    };
    let segment = unsafe { lang_of(obj) };
    let outcome = protected(
        || unsafe { protocol::Send::index(obj, key) },
        |fault| sequence_fault(fault, v, "index"),
    );
    settle(outcome, segment, "index", caller)
}

/// Set the element of `v` at `key`.
pub fn set_index(v: Value, key: Value, value: Value, caller: LangId) -> Result<(), Value> {
    let Some(obj) = object_of(v) else {
        return Err(sequence_error(v, "set_index", caller));
    };
    let segment = unsafe { lang_of(obj) };
    let outcome = protected(
        || unsafe { protocol::Send::set_index(obj, key, value).map(|()| Value::null()) },
        |fault| sequence_fault(fault, v, "set_index"),
    );
    settle(outcome, segment, "set_index", caller).map(|_| ())
}

fn sequence_error(v: Value, what: &str, caller: LangId) -> Value {
    let (kind, message) = sequence_fault(Fault::Unsupported, v, what);
    settle(Outcome::Fault(kind, message), LANG_CORE, what, caller)
        .err()
        .unwrap_or_else(Value::null)
}

fn sequence_fault(fault: Fault, v: Value, what: &str) -> (ErrorKind, String) {
    match fault {
        Fault::Missing => (
            ErrorKind::Index,
            format!("{} has no element there", describe(v)),
        ),
        _ => (
            ErrorKind::Type,
            format!("{} does not answer {what}", describe(v)),
        ),
    }
}

/// Call `callable` with `args` on behalf of `caller`. `Ok` is the callee's
/// result; `Err` is an `Error` value carrying one more trace frame, or the
/// caller's own error object when the error started there.
pub fn call(callable: Callable, args: &[Value], caller: LangId) -> Result<Value, Value> {
    call_named(callable, args, caller, "<callable>")
}

/// `call` with the callee's name for the trace frame.
pub fn call_named(
    callable: Callable,
    args: &[Value],
    caller: LangId,
    name: &str,
) -> Result<Value, Value> {
    call_at_opt(callable, args, caller, name, None)
}

/// [`call_named`] through a call site the caller keeps, for a callable
/// that is a member send: the callee's protocol may cache what it derived
/// there.
#[inline]
pub fn call_at(
    callable: Callable,
    site: &CallSite,
    args: &[Value],
    caller: LangId,
    name: &str,
) -> Result<Value, Value> {
    call_at_opt(callable, args, caller, name, Some(site))
}

/// The direct send `site` holds for `callable`, when it holds one and it
/// takes the call: a typed callable's, the whole call in one function.
/// `None` when the site holds none, or when what it holds declined and
/// the plain path is to fill it again.
#[inline]
pub fn call_direct_at(
    callable: Callable,
    site: &CallSite,
    args: &[Value],
    caller: LangId,
    name: &str,
) -> Option<Result<Value, Value>> {
    let (func, lang) = match callable {
        Callable::Typed { func, lang, .. } => (func as usize, lang),
        Callable::Cell { cell, lang, .. } => (unsafe { *cell } as usize, lang),
        _ => return None,
    };
    Some(match direct(site, func, args)? {
        Ok(v) => Ok(v),
        Err(()) => settle(Outcome::Raised, lang, name, caller),
    })
}

#[inline]
fn call_at_opt(
    callable: Callable,
    args: &[Value],
    caller: LangId,
    name: &str,
    site: Option<&CallSite>,
) -> Result<Value, Value> {
    match callable {
        Callable::Dynamic(v) => {
            let Some(obj) = object_of(v) else {
                let message = format!("{} is not callable", describe(v));
                return settle(
                    Outcome::Fault(ErrorKind::Type, message),
                    LANG_CORE,
                    name,
                    caller,
                );
            };
            let segment = unsafe { lang_of(obj) };
            let outcome = protected(
                || unsafe { protocol::Send::call(obj, args) },
                |fault| match fault {
                    Fault::Missing => (
                        ErrorKind::Runtime,
                        format!("{} has nothing to call", describe(v)),
                    ),
                    _ => (ErrorKind::Type, format!("{} is not callable", describe(v))),
                },
            );
            settle(outcome, segment, name, caller)
        }
        Callable::Typed {
            func,
            signature,
            lang,
        } => {
            if let Some(site) = site
                && let Some(result) = call_direct_at(callable, site, args, caller, name)
            {
                return result;
            }
            let outcome = typed_call(func, signature, lang, args, site);
            settle(outcome, lang, name, caller)
        }
        Callable::Cell {
            cell,
            signature,
            lang,
        } => {
            if let Some(site) = site
                && let Some(result) = call_direct_at(callable, site, args, caller, name)
            {
                return result;
            }
            let func = unsafe { *cell };
            let outcome = typed_call(func, signature, lang, args, site);
            settle(outcome, lang, name, caller)
        }
        Callable::WrenMethod {
            class,
            signature,
            is_static,
        } => {
            let (receiver, args) = if is_static {
                (class, args)
            } else {
                match args.split_first() {
                    Some((&receiver, rest)) => (receiver, rest),
                    None => {
                        let message = format!("`{}` takes a receiver", signature.name());
                        return settle(
                            Outcome::Fault(ErrorKind::Type, message),
                            LANG_CORE,
                            name,
                            caller,
                        );
                    }
                }
            };
            invoke_named(receiver, signature, args, caller, || name, site)
        }
    }
}

fn typed_call(
    func: *const c_void,
    signature: *const hl_type,
    lang: LangId,
    args: &[Value],
    site: Option<&CallSite>,
) -> Outcome {
    let Some(fun) = (unsafe { fun_of(signature) }) else {
        return Outcome::Fault(
            ErrorKind::Type,
            "typed callable has no function signature".to_owned(),
        );
    };
    let arity = unsafe { (*fun).nargs }.max(0) as usize;
    if arity != args.len() {
        return Outcome::Fault(
            ErrorKind::Type,
            format!("expected {arity} arguments, got {}", args.len()),
        );
    }
    let Some(dispatch) = typed_dispatch(lang) else {
        return Outcome::Fault(
            ErrorKind::Internal,
            format!("no typed dispatcher is registered for language {lang}"),
        );
    };
    let site = site.map_or(ptr::null_mut(), |s| s as *const CallSite as *mut CallSite);
    protected(
        || {
            let mut out = Value::null();
            let code =
                unsafe { dispatch(func, signature, site, args.as_ptr(), args.len(), &mut out) };
            protocol::reply(code, out)
        },
        |fault| match fault {
            Fault::Missing => (ErrorKind::Runtime, "typed callable not found".to_owned()),
            _ => (
                ErrorKind::Type,
                format!("signature not supported by the typed dispatcher for language {lang}"),
            ),
        },
    )
}

/// Call the member `name` of `obj`.
pub fn invoke(obj: Value, name: Symbol, args: &[Value], caller: LangId) -> Result<Value, Value> {
    invoke_named(obj, name, args, caller, || name.name(), None)
}

/// [`invoke`] through a call site the caller keeps: the callee's protocol
/// may cache what it derived there.
#[inline]
pub fn invoke_at(
    obj: Value,
    name: Symbol,
    site: &CallSite,
    args: &[Value],
    caller: LangId,
) -> Result<Value, Value> {
    invoke_named(obj, name, args, caller, || name.name(), Some(site))
}

/// `invoke` with `frame` as the trace frame's name, asked for only when
/// there is an error to name.
#[inline]
fn invoke_named<'f>(
    obj: Value,
    name: Symbol,
    args: &[Value],
    caller: LangId,
    frame: impl FnOnce() -> &'f str,
    site: Option<&CallSite>,
) -> Result<Value, Value> {
    let Some(target) = object_of(obj) else {
        let message = format!("cannot invoke `{}` on {}", name.name(), describe(obj));
        return settle(
            Outcome::Fault(ErrorKind::Type, message),
            LANG_CORE,
            frame(),
            caller,
        );
    };
    if let Some(site) = site
        && let Some(reply) = direct(site, target as usize, args)
    {
        return match reply {
            Ok(v) => Ok(v),
            Err(()) => settle(Outcome::Raised, unsafe { lang_of(target) }, frame(), caller),
        };
    }
    let outcome = protected(
        || match site {
            Some(site) => unsafe { protocol::Send::invoke_at(target, name, site, args) },
            None => unsafe { protocol::Send::invoke(target, name, args) },
        },
        |fault| member_fault(fault, obj, name, "invoke"),
    );
    settle_lazy(outcome, unsafe { lang_of(target) }, frame, caller)
}

/// `settle` with the frame's name asked for only on the error path.
#[inline]
fn settle_lazy<'f>(
    outcome: Outcome,
    segment: LangId,
    name: impl FnOnce() -> &'f str,
    caller: LangId,
) -> Result<Value, Value> {
    if let Outcome::Ok(v) = outcome {
        return Ok(v);
    }
    settle(outcome, segment, name(), caller)
}

/// The site's direct send, when it has one and it answers: the value, or
/// `Err` when the callee raised. `None` when the site has none or it
/// answered that it no longer fits, which also forgets it.
#[inline]
fn direct(site: &CallSite, target: usize, args: &[Value]) -> Option<Result<Value, ()>> {
    let Some(f) = site.direct() else {
        site.note_plain();
        return None;
    };
    let mut out = Value::null();
    let code = unsafe { f(site, target, args.as_ptr(), args.len(), &mut out) };
    match code {
        protocol::REPLY_OK => Some(Ok(out)),
        protocol::REPLY_RAISED => Some(Err(())),
        _ => {
            site.clear_direct();
            site.note_plain();
            None
        }
    }
}

/// Read the member `name` of `obj`.
pub fn get(obj: Value, name: Symbol, caller: LangId) -> Result<Value, Value> {
    get_at_opt(obj, name, None, caller)
}

/// [`get`] through a call site the caller keeps.
#[inline]
pub fn get_at(obj: Value, name: Symbol, site: &CallSite, caller: LangId) -> Result<Value, Value> {
    get_at_opt(obj, name, Some(site), caller)
}

#[inline]
fn get_at_opt(
    obj: Value,
    name: Symbol,
    site: Option<&CallSite>,
    caller: LangId,
) -> Result<Value, Value> {
    let Some(target) = object_of(obj) else {
        let message = format!("cannot read `{}` of {}", name.name(), describe(obj));
        return settle(
            Outcome::Fault(ErrorKind::Type, message),
            LANG_CORE,
            name.name(),
            caller,
        );
    };
    if let Some(site) = site
        && let Some(reply) = direct(site, target as usize, &[])
    {
        return match reply {
            Ok(v) => Ok(v),
            Err(()) => settle(
                Outcome::Raised,
                unsafe { lang_of(target) },
                name.name(),
                caller,
            ),
        };
    }
    let outcome = protected(
        || match site {
            Some(site) => unsafe { protocol::Send::get_member_at(target, name, site) },
            None => unsafe { protocol::Send::get_member(target, name) },
        },
        |fault| member_fault(fault, obj, name, "read"),
    );
    settle_lazy(outcome, unsafe { lang_of(target) }, || name.name(), caller)
}

/// Write the member `name` of `obj`.
pub fn set(obj: Value, name: Symbol, value: Value, caller: LangId) -> Result<(), Value> {
    set_at_opt(obj, name, None, value, caller)
}

/// [`set`] through a call site the caller keeps.
#[inline]
pub fn set_at(
    obj: Value,
    name: Symbol,
    site: &CallSite,
    value: Value,
    caller: LangId,
) -> Result<(), Value> {
    set_at_opt(obj, name, Some(site), value, caller)
}

#[inline]
fn set_at_opt(
    obj: Value,
    name: Symbol,
    site: Option<&CallSite>,
    value: Value,
    caller: LangId,
) -> Result<(), Value> {
    let Some(target) = object_of(obj) else {
        let message = format!("cannot write `{}` of {}", name.name(), describe(obj));
        return settle(
            Outcome::Fault(ErrorKind::Type, message),
            LANG_CORE,
            name.name(),
            caller,
        )
        .map(|_| ());
    };
    if let Some(site) = site
        && let Some(reply) = direct(site, target as usize, &[value])
    {
        return match reply {
            Ok(_) => Ok(()),
            Err(()) => settle(
                Outcome::Raised,
                unsafe { lang_of(target) },
                name.name(),
                caller,
            )
            .map(|_| ()),
        };
    }
    let segment = unsafe { lang_of(target) };
    let outcome = protected(
        || {
            match site {
                Some(site) => unsafe { protocol::Send::set_member_at(target, name, site, value) },
                None => unsafe { protocol::Send::set_member(target, name, value) },
            }
            .map(|()| Value::null())
        },
        |fault| member_fault(fault, obj, name, "write"),
    );
    settle_lazy(outcome, segment, || name.name(), caller).map(|_| ())
}

fn member_fault(fault: Fault, obj: Value, name: Symbol, verb: &str) -> (ErrorKind, String) {
    match fault {
        Fault::Missing => (
            ErrorKind::Runtime,
            format!("{} has no member `{}`", describe(obj), name.name()),
        ),
        _ => (
            ErrorKind::Type,
            format!(
                "{} does not answer {verb} for `{}`",
                describe(obj),
                name.name()
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Str;
    use crate::heap::TypeDesc;
    use crate::protocol::{Protocol, REPLY_MISSING, REPLY_OK, REPLY_UNSUPPORTED};
    use crate::symbol::intern;
    use caribou_abi::hl::hl_type_detail;
    use core::ptr;

    /// Tests read values that come back unrooted, so each holds the GC
    /// lock: no collection another thread starts can run meanwhile.
    fn locked() -> heap::GcGuard {
        heap::gc_guard()
    }

    const LANG_A: LangId = 41;
    const LANG_B: LangId = 42;

    // A test object: word zero and a tag saying how its `call` behaves.
    #[repr(C)]
    struct Probe {
        desc: *const TypeDesc,
        behaviour: u32,
    }

    const SUMS: u32 = 0;
    const RAISES: u32 = 1;
    const PANICS: u32 = 2;
    const RAISES_BARE: u32 = 3;
    const MISSING: u32 = 4;

    unsafe extern "C-unwind" fn probe_call(
        obj: *mut u8,
        args: *const Value,
        n: usize,
        out: *mut Value,
    ) -> u8 {
        let probe = unsafe { &*(obj as *const Probe) };
        let args = unsafe { core::slice::from_raw_parts(args, n) };
        match probe.behaviour {
            SUMS => {
                let sum: i32 = args.iter().filter_map(|v| v.as_int()).sum();
                unsafe { *out = Value::int(sum) };
                REPLY_OK
            }
            RAISES => raise(Error::new(ErrorKind::User, "boom", LANG_A)),
            RAISES_BARE => {
                set_pending(Str::value(Str::new("bare payload")));
                protocol::REPLY_RAISED
            }
            PANICS => panic!("entry panicked on purpose"),
            _ => REPLY_MISSING,
        }
    }

    unsafe extern "C-unwind" fn probe_invoke(
        obj: *mut u8,
        name: Symbol,
        args: *const Value,
        n: usize,
        out: *mut Value,
    ) -> u8 {
        if name.name() == "sum" {
            unsafe { probe_call(obj, args, n, out) }
        } else {
            REPLY_MISSING
        }
    }

    unsafe extern "C-unwind" fn probe_get(obj: *mut u8, name: Symbol, out: *mut Value) -> u8 {
        if name.name() == "behaviour" {
            unsafe { *out = Value::int((*(obj as *const Probe)).behaviour as i32) };
            REPLY_OK
        } else {
            REPLY_MISSING
        }
    }

    unsafe extern "C-unwind" fn probe_set(_obj: *mut u8, _name: Symbol, _value: Value) -> u8 {
        REPLY_UNSUPPORTED
    }

    static PROBE_PROTO: Protocol = Protocol {
        call: Some(probe_call),
        invoke: Some(probe_invoke),
        get_member: Some(probe_get),
        set_member: Some(probe_set),
        ..Protocol::NONE
    };

    const fn probe_type() -> hl_type {
        hl_type {
            kind: hl::HABSTRACT,
            detail: hl_type_detail {
                abs_name: ptr::null(),
            },
            vobj_proto: ptr::null_mut(),
            mark_bits: ptr::null_mut(),
        }
    }

    static PROBE_DESC: TypeDesc = {
        let mut d = TypeDesc::new(probe_type());
        d.protocol = &PROBE_PROTO;
        d.name = "Probe".as_ptr();
        d.name_len = 5;
        d.lang = LANG_A;
        d
    };

    static MUTE_DESC: TypeDesc = {
        let mut d = TypeDesc::new(probe_type());
        d.name = "Mute".as_ptr();
        d.name_len = 4;
        d.lang = LANG_A;
        d
    };

    fn probe(behaviour: u32) -> Box<Probe> {
        Box::new(Probe {
            desc: &PROBE_DESC,
            behaviour,
        })
    }

    fn value_of(p: &Probe) -> Value {
        Value::object(p as *const Probe as *const c_void)
    }

    unsafe fn error(v: Value) -> *mut Error {
        unsafe { Error::from_value(v) }.expect("an Error value")
    }

    #[test]
    fn the_pending_slot_holds_one_error_per_task_and_take_clears_it() {
        let _lock = locked();
        assert!(!has_pending());
        assert_eq!(take_pending(), None);
        let e = Error::new_rooted(ErrorKind::User, "first", LANG_A);
        set_pending(e.value());
        assert!(has_pending());
        let again = Error::new_rooted(ErrorKind::User, "second", LANG_A);
        set_pending(again.value());
        assert_eq!(take_pending(), Some(again.value()));
        assert!(!has_pending());
        assert_eq!(take_pending(), None);
    }

    #[test]
    fn a_dynamic_call_returns_the_entrys_value() {
        let _lock = locked();
        let p = probe(SUMS);
        let r = call(
            Callable::Dynamic(value_of(&p)),
            &[Value::int(2), Value::int(3), Value::int(5)],
            LANG_B,
        );
        assert_eq!(r, Ok(Value::int(10)));
        assert!(!has_pending());
    }

    /// A Wren method is an `invoke` of its signature: on the first argument
    /// for an instance method, on the class for a static one.
    #[test]
    fn a_wren_method_invokes_its_signature_on_the_receiver() {
        let _lock = locked();
        let p = probe(SUMS);
        let sum = intern("sum");
        let r = call(
            Callable::WrenMethod {
                class: Value::null(),
                signature: sum,
                is_static: false,
            },
            &[value_of(&p), Value::int(2), Value::int(3)],
            LANG_B,
        );
        assert_eq!(r, Ok(Value::int(5)));
        let r = call(
            Callable::WrenMethod {
                class: value_of(&p),
                signature: sum,
                is_static: true,
            },
            &[Value::int(4), Value::int(3)],
            LANG_B,
        );
        assert_eq!(r, Ok(Value::int(7)));
        // No receiver to send to; a member the receiver lacks.
        let err = call(
            Callable::WrenMethod {
                class: Value::null(),
                signature: sum,
                is_static: false,
            },
            &[],
            LANG_B,
        )
        .unwrap_err();
        assert_eq!(unsafe { Error::kind(error(err)) }, ErrorKind::Type);
        let err = call_named(
            Callable::WrenMethod {
                class: Value::null(),
                signature: intern("nope(_)"),
                is_static: false,
            },
            &[value_of(&p), Value::int(1)],
            LANG_B,
            "Probe.nope",
        )
        .unwrap_err();
        let e = unsafe { error(err) };
        unsafe {
            assert_eq!(Error::kind(e), ErrorKind::Runtime);
            assert_eq!(Error::frames(e)[0].name_str(), "Probe.nope");
        }
    }

    #[test]
    fn a_raising_entry_yields_its_error_with_one_frame() {
        let _lock = locked();
        let p = probe(RAISES);
        let err = call_named(Callable::Dynamic(value_of(&p)), &[], LANG_B, "explode").unwrap_err();
        let e = unsafe { error(err) };
        unsafe {
            assert_eq!(Error::kind(e), ErrorKind::User);
            assert_eq!(Error::message_str(e), "boom");
            assert_eq!(Error::origin(e), LANG_A);
            let frames = Error::frames(e);
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].lang, LANG_A);
            assert_eq!(frames[0].name_str(), "explode");
            assert_eq!(frames[0].source_str(), None);
            assert_eq!(frames[0].span(), None);
        }
        assert!(!has_pending(), "the slot is cleared by the call");
        // Through `call`, the frame names the callable generically.
        let err = call(Callable::Dynamic(value_of(&p)), &[], LANG_B).unwrap_err();
        assert_eq!(
            unsafe { Error::frames(error(err))[0].name_str() },
            "<callable>"
        );
    }

    #[test]
    fn a_non_callable_object_is_a_type_error() {
        let _lock = locked();
        let mute = Probe {
            desc: &MUTE_DESC,
            behaviour: 0,
        };
        let err = call(Callable::Dynamic(value_of(&mute)), &[], LANG_B).unwrap_err();
        let e = unsafe { error(err) };
        unsafe {
            assert_eq!(Error::kind(e), ErrorKind::Type);
            assert_eq!(Error::message_str(e), "a Mute is not callable");
            assert_eq!(Error::frames(e).len(), 1);
            assert_eq!(Error::frames(e)[0].lang, LANG_A);
        }
        // Non-objects too, with the core as the segment.
        let err = call(Callable::Dynamic(Value::int(7)), &[], LANG_B).unwrap_err();
        let e = unsafe { error(err) };
        unsafe {
            assert_eq!(Error::kind(e), ErrorKind::Type);
            assert_eq!(Error::message_str(e), "an int is not callable");
            assert_eq!(Error::frames(e)[0].lang, LANG_CORE);
        }
        let err = call(Callable::Dynamic(Value::null()), &[], LANG_B).unwrap_err();
        assert_eq!(
            unsafe { Error::message_str(error(err)) },
            "null is not callable"
        );
        // An entry answering `Missing` is a runtime error.
        let p = probe(MISSING);
        let err = call(Callable::Dynamic(value_of(&p)), &[], LANG_B).unwrap_err();
        assert_eq!(unsafe { Error::kind(error(err)) }, ErrorKind::Runtime);
    }

    #[test]
    fn a_panicking_entry_becomes_an_internal_error() {
        let _lock = locked();
        let p = probe(PANICS);
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r = call(Callable::Dynamic(value_of(&p)), &[], LANG_B);
        std::panic::set_hook(hook);
        let e = unsafe { error(r.unwrap_err()) };
        unsafe {
            assert_eq!(Error::kind(e), ErrorKind::Internal);
            assert_eq!(Error::message_str(e), "entry panicked on purpose");
            assert_eq!(Error::frames(e).len(), 1);
        }
    }

    #[test]
    fn an_error_returning_home_is_unwrapped_to_its_native_object() {
        let _lock = locked();
        let native = Str::new_rooted("the original exception");
        let make = || {
            let e = Error::new_rooted(ErrorKind::User, "wrapped", LANG_A);
            unsafe { Error::set_native(e.ptr() as *mut Error, native.value()) };
            e
        };
        // Raised by LANG_A's entry, back to a LANG_A caller: the native object.
        let e = make();
        let outcome = Outcome::Raised;
        set_pending(e.value());
        let r = settle(outcome, LANG_A, "f", LANG_A);
        assert_eq!(r, Err(native.value()));
        // To another caller: the Error itself, native still attached.
        let e = make();
        set_pending(e.value());
        let r = settle(Outcome::Raised, LANG_A, "f", LANG_B);
        let err = unsafe { error(r.unwrap_err()) };
        unsafe {
            assert_eq!(Error::native(err), native.value());
            assert_eq!(Error::message_str(err), "wrapped");
        }
        // An Error without a native payload is never unwrapped.
        let plain = Error::new_rooted(ErrorKind::User, "plain", LANG_A);
        set_pending(plain.value());
        assert_eq!(
            settle(Outcome::Raised, LANG_A, "f", LANG_A),
            Err(plain.value())
        );
    }

    #[test]
    fn a_bare_pending_value_travels_wrapped_and_unwraps_at_home() {
        let _lock = locked();
        let p = probe(RAISES_BARE);
        let err = call(Callable::Dynamic(value_of(&p)), &[], LANG_B).unwrap_err();
        let e = unsafe { error(err) };
        unsafe {
            assert_eq!(Error::kind(e), ErrorKind::User);
            assert_eq!(Error::origin(e), LANG_A);
            assert_eq!(Str::text(Error::native(e)), Some("bare payload"));
        }
        let home = call(Callable::Dynamic(value_of(&p)), &[], LANG_A).unwrap_err();
        assert_eq!(unsafe { Str::text(home) }, Some("bare payload"));
    }

    #[test]
    fn invoke_get_and_set_map_faults_the_same_way() {
        let _lock = locked();
        let p = probe(SUMS);
        let v = value_of(&p);
        assert_eq!(
            invoke(
                v,
                Symbol::intern("sum"),
                &[Value::int(1), Value::int(2)],
                LANG_B
            ),
            Ok(Value::int(3))
        );
        let err = invoke(v, Symbol::intern("nope"), &[], LANG_B).unwrap_err();
        let e = unsafe { error(err) };
        unsafe {
            assert_eq!(Error::kind(e), ErrorKind::Runtime);
            assert_eq!(Error::message_str(e), "a Probe has no member `nope`");
            assert_eq!(Error::frames(e)[0].name_str(), "nope");
        }
        assert_eq!(
            get(v, Symbol::intern("behaviour"), LANG_B),
            Ok(Value::int(SUMS as i32))
        );
        let err = get(v, Symbol::intern("nope"), LANG_B).unwrap_err();
        assert_eq!(unsafe { Error::kind(error(err)) }, ErrorKind::Runtime);
        let err = set(v, Symbol::intern("behaviour"), Value::int(1), LANG_B).unwrap_err();
        let e = unsafe { error(err) };
        unsafe {
            assert_eq!(Error::kind(e), ErrorKind::Type);
            assert_eq!(
                Error::message_str(e),
                "a Probe does not answer write for `behaviour`"
            );
        }
        let err = invoke(Value::null(), Symbol::intern("x"), &[], LANG_B).unwrap_err();
        assert_eq!(
            unsafe { Error::message_str(error(err)) },
            "cannot invoke `x` on null"
        );
    }

    // ── typed calls ────────────────────────────────────────────────────

    fn leak_type(kind: hl_type_kind) -> *mut hl_type {
        Box::into_raw(Box::new(hl_type {
            kind,
            detail: hl_type_detail {
                abs_name: ptr::null(),
            },
            vobj_proto: ptr::null_mut(),
            mark_bits: ptr::null_mut(),
        }))
    }

    /// An `HFUN` type over the given argument and return kinds.
    fn signature(args: &[hl_type_kind], ret: hl_type_kind) -> *const hl_type {
        let args: Vec<*mut hl_type> = args.iter().map(|&k| leak_type(k)).collect();
        let nargs = args.len() as i32;
        let fun = Box::into_raw(Box::new(hl_type_fun {
            args: Box::leak(args.into_boxed_slice()).as_mut_ptr(),
            ret: leak_type(ret),
            nargs,
            parent: ptr::null_mut(),
            closure_type: hl::hl_type_fun_closure_type {
                kind: hl::HVOID,
                p: ptr::null_mut(),
            },
            closure: hl::hl_type_fun_closure {
                args: ptr::null_mut(),
                ret: ptr::null_mut(),
                nargs: 0,
                parent: ptr::null_mut(),
            },
        }));
        Box::into_raw(Box::new(hl_type {
            kind: hl::HFUN,
            detail: hl_type_detail { fun },
            vobj_proto: ptr::null_mut(),
            mark_bits: ptr::null_mut(),
        }))
    }

    extern "C" fn scale(n: i32, factor: f64) -> f64 {
        n as f64 * factor
    }

    extern "C" fn mix(a: f64, flag: bool, b: i32, c: f64) -> i32 {
        (if flag { a + c } else { a - c }) as i32 + b
    }

    extern "C" fn negate(b: bool) -> bool {
        !b
    }

    extern "C" fn identity(v: Value) -> Value {
        v
    }

    extern "C" fn tally(a: i32, b: i32, c: i32, d: i32) -> i32 {
        a + b + c + d
    }

    static TOUCHED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

    extern "C" fn touch(n: i32) {
        TOUCHED.store(n, std::sync::atomic::Ordering::SeqCst);
    }

    fn typed(func: *const (), sig: *const hl_type) -> Callable {
        Callable::Typed {
            func: func as *const c_void,
            signature: sig,
            lang: LANG_CORE,
        }
    }

    #[test]
    fn typed_calls_go_through_the_core_dispatcher() {
        let _lock = locked();
        let sig = signature(&[hl::HI32, hl::HF64], hl::HF64);
        let r = call(
            typed(scale as *const (), sig),
            &[Value::int(3), Value::number(1.5)],
            LANG_B,
        );
        assert_eq!(r, Ok(Value::number(4.5)));
        // An int where a double is wanted, and a whole number for an int.
        let r = call(
            typed(scale as *const (), sig),
            &[Value::number(4.0), Value::int(2)],
            LANG_B,
        );
        assert_eq!(r, Ok(Value::number(8.0)));

        let sig = signature(&[hl::HF64, hl::HBOOL, hl::HI32, hl::HF64], hl::HI32);
        let args = [
            Value::number(10.0),
            Value::bool(true),
            Value::int(5),
            Value::number(2.0),
        ];
        assert_eq!(
            call(typed(mix as *const (), sig), &args, LANG_B),
            Ok(Value::int(17))
        );
        let args = [
            Value::number(10.0),
            Value::bool(false),
            Value::int(5),
            Value::number(2.0),
        ];
        assert_eq!(
            call(typed(mix as *const (), sig), &args, LANG_B),
            Ok(Value::int(13))
        );

        let sig = signature(&[hl::HBOOL], hl::HBOOL);
        assert_eq!(
            call(
                typed(negate as *const (), sig),
                &[Value::bool(false)],
                LANG_B
            ),
            Ok(Value::bool(true))
        );

        let sig = signature(&[hl::HDYN], hl::HDYN);
        let s = Str::new_rooted("through");
        assert_eq!(
            call(typed(identity as *const (), sig), &[s.value()], LANG_B),
            Ok(s.value())
        );

        let sig = signature(&[hl::HI32; 4], hl::HI32);
        let args = [Value::int(1), Value::int(2), Value::int(3), Value::int(-10)];
        assert_eq!(
            call(typed(tally as *const (), sig), &args, LANG_B),
            Ok(Value::int(-4))
        );

        let sig = signature(&[hl::HI32], hl::HVOID);
        assert_eq!(
            call(typed(touch as *const (), sig), &[Value::int(99)], LANG_B),
            Ok(Value::null())
        );
        assert_eq!(TOUCHED.load(std::sync::atomic::Ordering::SeqCst), 99);
    }

    #[test]
    fn a_typed_arity_or_kind_mismatch_is_a_type_error() {
        let _lock = locked();
        let sig = signature(&[hl::HI32, hl::HF64], hl::HF64);
        let err = call(typed(scale as *const (), sig), &[Value::int(3)], LANG_B).unwrap_err();
        let e = unsafe { error(err) };
        unsafe {
            assert_eq!(Error::kind(e), ErrorKind::Type);
            assert_eq!(Error::message_str(e), "expected 2 arguments, got 1");
            assert_eq!(Error::frames(e).len(), 1);
            assert_eq!(Error::frames(e)[0].lang, LANG_CORE);
        }
        // The right count but a value the kind cannot take.
        let err = call(
            typed(scale as *const (), sig),
            &[Value::bool(true), Value::number(1.0)],
            LANG_B,
        )
        .unwrap_err();
        let e = unsafe { error(err) };
        unsafe {
            assert_eq!(Error::kind(e), ErrorKind::Type);
            assert_eq!(
                Error::message_str(e),
                "argument 0 expects kind 3, got a bool"
            );
        }
        // A kind the core dispatcher does not marshal.
        let sig = signature(&[hl::HBYTES], hl::HVOID);
        let err = call(typed(touch as *const (), sig), &[Value::null()], LANG_B).unwrap_err();
        assert_eq!(unsafe { Error::kind(error(err)) }, ErrorKind::Type);
        // A language with no dispatcher.
        let sig = signature(&[], hl::HVOID);
        let err = call(
            Callable::Typed {
                func: touch as *const c_void,
                signature: sig,
                lang: 9_999,
            },
            &[],
            LANG_B,
        )
        .unwrap_err();
        assert_eq!(unsafe { Error::kind(error(err)) }, ErrorKind::Internal);
        // Not a function type at all.
        let err = call(typed(touch as *const (), leak_type(hl::HI32)), &[], LANG_B).unwrap_err();
        assert_eq!(unsafe { Error::kind(error(err)) }, ErrorKind::Type);
    }

    #[test]
    fn a_registered_dispatcher_replaces_the_default_for_its_language() {
        let _lock = locked();
        unsafe extern "C-unwind" fn always_seven(
            _func: *const c_void,
            _sig: *const hl_type,
            _site: *mut CallSite,
            _args: *const Value,
            _nargs: usize,
            out: *mut Value,
        ) -> u8 {
            unsafe { *out = Value::int(7) };
            REPLY_OK
        }
        const LANG_T: LangId = 4_242;
        assert!(typed_dispatch(LANG_T).is_none());
        set_typed_dispatch(LANG_T, always_seven);
        assert!(typed_dispatch(LANG_T).is_some());
        let sig = signature(&[], hl::HI32);
        let r = call(
            Callable::Typed {
                func: ptr::null(),
                signature: sig,
                lang: LANG_T,
            },
            &[],
            LANG_B,
        );
        assert_eq!(r, Ok(Value::int(7)));
    }
}
