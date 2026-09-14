//! Ash's half of the bridge: the typed dispatcher for Haxe callables, and
//! the object protocol for Haxe objects.
//!
//! A Haxe object's word zero is a bare `hl_type`, which has no protocol
//! slot, so a Haxe object crosses wrapped: a `HaxeRef`, a core object under
//! [`HAXE_DESC`] whose one field is the `vdynamic` it stands for. `wrap`
//! makes one; nothing caches them, so two wrappers of one object are equal
//! and hash alike but are distinct objects. Every entry here reads the
//! wrapper's field and works on the Haxe object through ash's own dynamic
//! access: `hlp_dyn_getp`, `hlp_dyn_setp`, `hlp_dyn_call`, by the field
//! hash a symbol carries for its name.
//!
//! A Haxe `String` crosses as a value, not wrapped: it becomes a core `Str`
//! on the way out, and a core `Str` becomes a fresh `String` on the way in,
//! allocated under the type the loaded program's `String` class carries.
//!
//! The dispatcher takes a typed callable (a code pointer and its
//! `hl_type_fun`), boxes each argument by the signature's kind into the
//! `vdynamic` `hlp_dyn_call` takes, and unboxes the result by the return
//! kind. Every call into Haxe code runs under a HashLink trap whose setjmp
//! frame is C (`trap.c`), so a `hl_throw` inside lands there instead of
//! unwinding through Rust; the thrown value becomes a core `Error` with the
//! exception as its native payload, and the entry answers `Raised`.

use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{OnceLock, RwLock};

use ash_std::bytes::hlp_alloc_bytes;
use ash_std::error::{
    hlp_clear_exc_value, hlp_get_exc_value, hlp_remove_trap_in, hlp_setup_trap_in,
};
use ash_std::fun::hlp_dyn_call;
use ash_std::obj::{
    hl_get_obj_proto, hlp_alloc_dynamic, hlp_alloc_dynbool, hlp_alloc_obj, hlp_dyn_getp,
    hlp_dyn_setp, hlp_lookup_find, hlp_obj_has_field,
};
use ash_std::strings::hlp_value_to_string;
use ash_std::types::{hlt_bytes, hlt_dyn, hlt_f64, hlt_i32, hlt_i64};
use caribou::bridge;
use caribou::error::{Error, Str};
use caribou::heap::{self, Handle, Tracer, TypeDesc};
use caribou::protocol::{
    CallSite, Callable, Protocol, REPLY_MISSING, REPLY_OK, REPLY_RAISED, REPLY_UNSUPPORTED, Symbol,
    desc_of,
};
use caribou::registry::ClassIface;
use caribou_abi::hl::{
    self, hl_field_lookup, hl_module_context, hl_runtime_obj, hl_type, hl_type_detail, hl_type_fun,
    hl_type_kind, uchar, vclosure, vdynamic,
};
use caribou_abi::mem::{KIND_DYNAMIC, KIND_NOPTR, TRACED};
use caribou_abi::{ErrorKind, LangId, Value};

use crate::callback;
use crate::import;

/// `HL_MAX_ARGS`: what `hlp_dyn_call` takes.
const MAX_ARGS: usize = 9;

// ---------------------------------------------------------------------------
// The wrapper
// ---------------------------------------------------------------------------

/// A Haxe object as a core object: the descriptor, then the object.
#[repr(C)]
struct HaxeRef {
    desc: *const TypeDesc,
    obj: *mut vdynamic,
}

unsafe extern "C" fn trace_ref(obj: *mut u8, tracer: *mut Tracer) {
    let r = unsafe { &*(obj as *const HaxeRef) };
    unsafe { (*tracer).mark(r.obj as *const u8) };
}

pub(crate) const fn haxe_type() -> hl_type {
    hl_type {
        kind: hl::HABSTRACT,
        detail: hl_type_detail {
            abs_name: ptr::null(),
        },
        vobj_proto: ptr::null_mut(),
        mark_bits: ptr::null_mut(),
    }
}

/// Word zero of every wrapper. Mutable for one field: `lang` is the id the
/// world assigns, written by `set_lang` at registration and read from then
/// on.
static mut HAXE_DESC: TypeDesc = {
    let mut d = TypeDesc::new(haxe_type());
    d.trace = Some(trace_ref);
    d.protocol = &HAXE_PROTO;
    d.name = "haxe object".as_ptr();
    d.name_len = "haxe object".len();
    d
};

fn haxe_desc() -> *const TypeDesc {
    &raw const HAXE_DESC
}

/// The language id Haxe objects carry: what the world assigned through
/// [`Runtime`](crate::Runtime), or the core's id before any registration.
pub fn lang() -> LangId {
    unsafe { (*haxe_desc()).lang }
}

pub(crate) fn set_lang(lang: LangId) {
    unsafe { HAXE_DESC.lang = lang };
}

/// `obj` as a bridge value: a fresh wrapper, or `null` for null. The result
/// is not rooted; store it or root it before allocating.
pub fn wrap(obj: *mut vdynamic) -> Value {
    if obj.is_null() {
        return Value::null();
    }
    Value::object(alloc_wrapper(obj) as *const c_void)
}

/// A fresh wrapper of `obj`, unrooted. `obj` itself is a raw address on
/// the caller's stack, which the conservative scan sees.
fn alloc_wrapper(obj: *mut vdynamic) -> *mut HaxeRef {
    let p = unsafe {
        heap::alloc_gen(
            haxe_desc() as *mut hl_type,
            size_of::<HaxeRef>(),
            KIND_DYNAMIC | TRACED,
        )
    } as *mut HaxeRef;
    if p.is_null() {
        heap::out_of_memory("a haxe object wrapper");
    }
    unsafe { (*p).obj = obj };
    p
}

/// [`wrap`], with a handle the caller releases.
fn wrap_rooted(obj: *mut vdynamic) -> (Value, Handle) {
    let p = alloc_wrapper(obj);
    (
        Value::object(p as *const c_void),
        heap::handle_new(p as *mut u8),
    )
}

/// The Haxe object behind `v`, if `v` is a wrapper.
pub fn unwrap(v: Value) -> Option<*mut vdynamic> {
    let p = v.as_object()? as *mut u8;
    if p.is_null() || !ptr::eq(unsafe { desc_of(p) }, haxe_desc()) {
        return None;
    }
    Some(unsafe { (*(p as *const HaxeRef)).obj })
}

unsafe fn inner(obj: *mut u8) -> *mut vdynamic {
    unsafe { (*(obj as *const HaxeRef)).obj }
}

// ---------------------------------------------------------------------------
// The trap
// ---------------------------------------------------------------------------

unsafe extern "C" {
    fn caribou_ash_run_with_hl_trap(
        setup: unsafe extern "C" fn(*mut c_void, usize, usize) -> *mut c_void,
        remove: unsafe extern "C" fn(*mut c_void),
        callback: unsafe extern "C" fn(*mut c_void),
        context: *mut c_void,
    ) -> i32;
}

/// Run `f` under a HashLink trap; `Err` is what it threw. A throw abandons
/// the frames inside `f` without running their drops, so `f` owns nothing
/// that needs one: it reads and writes slots the caller prepared. The
/// trap's context lives in the C frame that sets the jump, so the runtime
/// allocates and pools nothing for it.
///
/// Under a guard (`bridge::guarded`) no trap is armed: a throw lands in
/// the guard, which is the language that entered Haxe taking the error
/// at its own boundary, and every frame between is one that owns nothing.
fn trapped<F: FnMut()>(mut f: F) -> Result<(), *mut vdynamic> {
    if bridge::guarded() {
        f();
        return Ok(());
    }
    bridge::note_reentry();
    trapped_always(f)
}

/// [`trapped`], guard or no guard: what a guard itself is made of.
fn trapped_always<F: FnMut()>(mut f: F) -> Result<(), *mut vdynamic> {
    unsafe extern "C" fn thunk<F: FnMut()>(context: *mut c_void) {
        unsafe { (*(context as *mut F))() }
    }
    // The shim tells the runtime the lock is not held; see `trap.c`.
    debug_assert_eq!(heap::gc_lock_held_depth(), 0);
    let threw = unsafe {
        caribou_ash_run_with_hl_trap(
            hlp_setup_trap_in,
            hlp_remove_trap_in,
            thunk::<F>,
            &mut f as *mut F as *mut c_void,
        )
    };
    match threw {
        0 => Ok(()),
        1 => {
            let exception = unsafe { hlp_get_exc_value() };
            unsafe { hlp_clear_exc_value() };
            Err(exception.cast())
        }
        _ => panic!("the runtime's trap context outgrew the frame that holds it"),
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// The text at a NUL-terminated UTF-16 pointer.
unsafe fn utf16z(p: *const uchar) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

/// The runtime's own messages, by the words they use.
fn kind_of_message(message: &str) -> ErrorKind {
    if message.starts_with("Null access") {
        ErrorKind::NullAccess
    } else if message.contains("ut of bounds") {
        ErrorKind::Index
    } else if message.starts_with("Divide by zero") {
        ErrorKind::Arithmetic
    } else if message.starts_with("Stack overflow") {
        ErrorKind::StackOverflow
    } else if message.starts_with("Out of memory") {
        ErrorKind::OutOfMemory
    } else {
        ErrorKind::Runtime
    }
}

/// A thrown value's kind and message, read without running Haxe code: a
/// bytes value is the runtime's own error, a String is the program's, and
/// any other object is named by its class.
unsafe fn describe_exception(exc: *mut vdynamic) -> (ErrorKind, String) {
    if exc.is_null() {
        return (ErrorKind::User, "null".to_owned());
    }
    let t = unsafe { (*exc).t };
    if t.is_null() {
        return (ErrorKind::User, "an exception".to_owned());
    }
    match unsafe { (*t).kind } {
        hl::HBYTES => {
            let message = unsafe { utf16z((*exc).v.bytes as *const uchar) };
            (kind_of_message(&message), message)
        }
        hl::HOBJ => {
            let obj = unsafe { (*t).detail.obj };
            let name = unsafe { utf16z((*obj).name) };
            if name == "String" {
                (ErrorKind::User, unsafe { string_text(exc) })
            } else if let Some(message) = unsafe { exception_message(exc) } {
                // A `haxe.Exception`: its message, read where it keeps it,
                // so no user code runs while the error is built.
                (ErrorKind::User, message)
            } else {
                (ErrorKind::User, name)
            }
        }
        kind => (ErrorKind::User, format!("a thrown value of kind {kind}")),
    }
}

/// The message a `haxe.Exception` keeps in `__exceptionMessage`, for an
/// object that has that field holding a `String`.
unsafe fn exception_message(exc: *mut vdynamic) -> Option<String> {
    static FIELD: OnceLock<Symbol> = OnceLock::new();
    let field = *FIELD.get_or_init(|| caribou::symbol::intern("__exceptionMessage"));
    let (offset, ft) = unsafe { field_of((*exc).t, field_hash(field)) }?;
    if unsafe { (*ft).kind } != hl::HOBJ {
        return None;
    }
    let s = unsafe { *((exc as *const u8).add(offset) as *const *mut vdynamic) };
    if s.is_null() {
        return None;
    }
    Some(unsafe { string_text(s) })
}

/// Raise `exc` as it was thrown: a core `Error` of Haxe's language whose
/// native payload is the exception, wrapped.
unsafe fn raise_exception(exc: *mut vdynamic) -> u8 {
    let (kind, message) = unsafe { describe_exception(exc) };
    let (native, root) = if exc.is_null() {
        (Value::null(), Handle::NULL)
    } else {
        wrap_rooted(exc)
    };
    let e = Error::new(kind, &message, lang());
    unsafe { Error::set_native(e, native) };
    heap::handle_release(root);
    bridge::raise(e)
}

fn raise_core(kind: ErrorKind, message: &str) -> u8 {
    bridge::raise(Error::new(kind, message, lang()))
}

/// Haxe's guard for the bridge: `body` runs under a trap, and a throw
/// that reaches it is the pending error, as a throw at a crossing is.
pub(crate) unsafe extern "C-unwind" fn guard(
    body: unsafe extern "C-unwind" fn(*mut c_void),
    ctx: *mut c_void,
) -> u8 {
    match trapped_always(|| unsafe { body(ctx) }) {
        Ok(()) => REPLY_OK,
        Err(exception) => unsafe { raise_exception(exception) },
    }
}

/// A core `Error` of Haxe's language for a failure at `name`, as a value.
pub(crate) fn error_value(name: &str, message: &str) -> Value {
    let e = Error::new(ErrorKind::Runtime, message, lang());
    unsafe { Error::push_segment(e, lang(), name) };
    Error::value(e)
}

/// What Haxe catches for a bridge error: the exception itself when the
/// error carries one, else its message as a `String`, or as bytes before
/// a program has published its `String` type, as the runtime's own errors
/// are thrown.
pub(crate) fn throwable(e: Value) -> *mut vdynamic {
    if let Some(exc) = unwrap(e) {
        return exc;
    }
    let native = unsafe { Error::from_value(e) }.map(|err| unsafe { Error::native(err) });
    if let Some(exc) = native.and_then(unwrap) {
        return exc;
    }
    let message = match unsafe { Error::from_value(e) } {
        Some(err) => unsafe { Error::message_str(err) }.to_owned(),
        None => unsafe { Str::text(e) }
            .map(str::to_owned)
            .unwrap_or_else(|| bridge::describe(e)),
    };
    if let Some(s) = unsafe { alloc_string(&message) } {
        return s;
    }
    let units: Vec<uchar> = message.encode_utf16().chain([0]).collect();
    let bytes = unsafe { hlp_alloc_bytes((units.len() * 2) as i32) } as *mut uchar;
    unsafe { ptr::copy_nonoverlapping(units.as_ptr(), bytes, units.len()) };
    let d = unsafe { hlp_alloc_dynamic(hlt_bytes()) };
    unsafe { (*d).v.bytes = bytes.cast() };
    d.cast()
}

// ---------------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------------

/// The loaded program's `String` type, for allocating one; null until a
/// program publishes.
static STRING_TYPE: AtomicPtr<hl_type> = AtomicPtr::new(ptr::null_mut());

pub(crate) fn set_string_type(t: *mut hl_type) {
    STRING_TYPE.store(t, Ordering::Release);
}

/// `String`'s fields as HashLink lays them out: the bytes, then the length
/// in UTF-16 units.
const STRING_BYTES: usize = size_of::<*mut hl_type>();
const STRING_LENGTH: usize = STRING_BYTES + size_of::<*const uchar>();

/// The name of `t` when it is an object type.
pub(crate) unsafe fn obj_name(t: *const hl_type) -> Option<String> {
    let t = unsafe { t.as_ref()? };
    if !matches!(t.kind, hl::HOBJ | hl::HSTRUCT) {
        return None;
    }
    let obj = unsafe { t.detail.obj };
    (!obj.is_null()).then(|| unsafe { utf16z((*obj).name) })
}

unsafe fn is_string(d: *mut vdynamic) -> bool {
    let t = unsafe { (*d).t };
    let known = STRING_TYPE.load(Ordering::Acquire);
    if !known.is_null() {
        return std::ptr::eq(t, known);
    }
    unsafe { obj_name_is(t, "String") }
}

/// Whether the object type `t` is named `name`, read without a copy.
pub(crate) unsafe fn obj_name_is(t: *const hl_type, name: &str) -> bool {
    let Some(t) = (unsafe { t.as_ref() }) else {
        return false;
    };
    if !matches!(t.kind, hl::HOBJ | hl::HSTRUCT) {
        return false;
    }
    let obj = unsafe { t.detail.obj };
    if obj.is_null() {
        return false;
    }
    let mut p = unsafe { (*obj).name };
    if p.is_null() {
        return false;
    }
    for unit in name.encode_utf16() {
        if unsafe { *p } != unit {
            return false;
        }
        p = unsafe { p.add(1) };
    }
    (unsafe { *p }) == 0
}

/// The text of a `String` object.
unsafe fn string_text(d: *mut vdynamic) -> String {
    let base = d as *const u8;
    let bytes = unsafe { *(base.add(STRING_BYTES) as *const *const uchar) };
    let len = unsafe { *(base.add(STRING_LENGTH) as *const i32) };
    if bytes.is_null() || len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(bytes, len as usize) })
}

/// A `String` object holding `text`, or `None` before a program has
/// published its `String` type. Unrooted, like every fresh Haxe object:
/// the caller keeps it where the scanner sees it.
unsafe fn alloc_string(text: &str) -> Option<*mut vdynamic> {
    let t = STRING_TYPE.load(Ordering::Acquire);
    if t.is_null() {
        return None;
    }
    let units: Vec<uchar> = text.encode_utf16().collect();
    let bytes = unsafe { hlp_alloc_bytes(((units.len() + 1) * 2) as i32) } as *mut uchar;
    unsafe {
        ptr::copy_nonoverlapping(units.as_ptr(), bytes, units.len());
        *bytes.add(units.len()) = 0;
    }
    let s = unsafe { hlp_alloc_obj(t.cast()) } as *mut vdynamic;
    let base = s as *mut u8;
    unsafe {
        *(base.add(STRING_BYTES) as *mut *const uchar) = bytes;
        *(base.add(STRING_LENGTH) as *mut i32) = units.len() as i32;
    }
    Some(s)
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A boxed dynamic as a bridge value: a scalar unboxed by its type, a
/// `String` copied into a core `Str`, a face the foreign object it stands
/// for, any other object wrapped.
pub(crate) unsafe fn dyn_to_value(d: *mut vdynamic) -> Value {
    if d.is_null() {
        return Value::null();
    }
    let t = unsafe { (*d).t };
    if t.is_null() {
        return wrap(d);
    }
    let v = unsafe { (*d).v };
    match unsafe { (*t).kind } {
        hl::HVOID => Value::null(),
        hl::HUI8 => Value::int(i32::from(unsafe { v.ui8 })),
        hl::HUI16 => Value::int(i32::from(unsafe { v.ui16 })),
        hl::HI32 => Value::int(unsafe { v.i }),
        hl::HI64 => Value::number(unsafe { v.i64_ } as f64),
        hl::HF32 => Value::number(f64::from(unsafe { v.f })),
        hl::HF64 => Value::number(unsafe { v.d }),
        hl::HBOOL => Value::bool(unsafe { v.b }),
        hl::HOBJ if unsafe { is_string(d) } => {
            let text = unsafe { string_text(d) };
            Str::value(Str::new(&text))
        }
        hl::HOBJ => match unsafe { import::behind_face(d) } {
            Some(obj) => obj,
            None => wrap(d),
        },
        // A function of another language goes home as itself.
        hl::HFUN => match unsafe { callback::behind(d) } {
            Some(function) => function,
            None => wrap(d),
        },
        _ => wrap(d),
    }
}

unsafe fn box_int(n: i32) -> *mut vdynamic {
    let d = unsafe { hlp_alloc_dynamic(hlt_i32()) };
    unsafe { (*d).v.i = n };
    d.cast()
}

unsafe fn box_i64(n: i64) -> *mut vdynamic {
    let d = unsafe { hlp_alloc_dynamic(hlt_i64()) };
    unsafe { (*d).v.i64_ = n };
    d.cast()
}

unsafe fn box_f64(n: f64) -> *mut vdynamic {
    let d = unsafe { hlp_alloc_dynamic(hlt_f64()) };
    unsafe { (*d).v.d = n };
    d.cast()
}

/// A bridge value as the boxed dynamic `hlp_dyn_call` and `hlp_dyn_setp`
/// take for a slot of kind `kind`; the runtime casts the box to the slot's
/// exact type. A core `Str` becomes a `String`; an object of another
/// language becomes its face (`import.rs`).
pub(crate) unsafe fn value_to_dyn(v: Value, kind: hl_type_kind) -> Result<*mut vdynamic, String> {
    let int = || {
        v.as_int()
            .or_else(|| v.as_number().map(|n| n as i32))
            .or_else(|| v.as_bool().map(i32::from))
    };
    let float = || v.as_number().or_else(|| v.as_int().map(f64::from));
    let boxed = match kind {
        hl::HUI8 | hl::HUI16 | hl::HI32 => int().map(|n| unsafe { box_int(n) }),
        hl::HI64 => int().map(|n| unsafe { box_i64(i64::from(n)) }),
        hl::HF32 | hl::HF64 => float().map(|n| unsafe { box_f64(n) }),
        hl::HBOOL => v.as_bool().map(|b| unsafe { hlp_alloc_dynbool(b) }.cast()),
        hl::HVOID => None,
        _ => {
            if v.is_null() {
                Some(ptr::null_mut())
            } else if let Some(obj) = unwrap(v) {
                Some(obj)
            } else if let Some(text) = unsafe { Str::text(v) } {
                match unsafe { alloc_string(text) } {
                    Some(s) => Some(s),
                    None => {
                        return Err("a string cannot cross into Haxe before a program has published its String type".to_owned());
                    }
                }
            } else if let Some(n) = v.as_int() {
                Some(unsafe { box_int(n) })
            } else if let Some(n) = v.as_number() {
                Some(unsafe { box_f64(n) })
            } else if let Some(b) = v.as_bool() {
                Some(unsafe { hlp_alloc_dynbool(b) }.cast())
            } else if bridge::arity(v).is_some() {
                Some(callback::function_for(v))
            } else if v.as_object().is_some() {
                return import::face_for(v);
            } else {
                None
            }
        }
    };
    boxed.ok_or_else(|| {
        format!(
            "{} cannot cross into Haxe as kind {kind}",
            bridge::describe(v)
        )
    })
}

/// The `hl_type_fun` behind a function type.
unsafe fn fun_of(t: *const hl_type) -> Option<*const hl_type_fun> {
    let t = unsafe { t.as_ref()? };
    if t.kind != hl::HFUN && t.kind != hl::HMETHOD {
        return None;
    }
    let fun = unsafe { t.detail.fun };
    (!fun.is_null()).then_some(fun as *const hl_type_fun)
}

// ---------------------------------------------------------------------------
// Calls
// ---------------------------------------------------------------------------

/// Call `closure` with `args` boxed by its signature, under a trap, and
/// write the result unboxed by the return kind. `closure` may be a bound
/// method closure or one built on the stack around a bare code pointer.
unsafe fn call_closure(closure: *mut vclosure, args: &[Value], out: *mut Value) -> u8 {
    let Some(fun) = (unsafe { fun_of((*closure).t) }) else {
        return raise_core(ErrorKind::Type, "the closure has no function type");
    };
    let arity = unsafe { (*fun).nargs }.max(0) as usize;
    if arity != args.len() {
        return raise_core(
            ErrorKind::Type,
            &format!("expected {arity} arguments, got {}", args.len()),
        );
    }
    if arity > MAX_ARGS {
        return raise_core(
            ErrorKind::Type,
            &format!("a call takes at most {MAX_ARGS} arguments, not {arity}"),
        );
    }
    // Compiled code with at most a bound value is called by its signature.
    let has_value = unsafe { (*closure).hasValue };
    if has_value <= 1
        && let Some(code) = unsafe { code_of((*closure).fun as usize) }
    {
        let bound = (has_value == 1).then(|| unsafe { (*closure).value });
        match unsafe { direct_call(code, (*closure).t, bound, args, out) } {
            Some(Ok(())) => return REPLY_OK,
            Some(Err(exception)) => return unsafe { raise_exception(exception) },
            None => {}
        }
    }
    // Boxed before the trap: a throw abandons whatever the trap's frames
    // hold, and the boxes are on this frame's stack for the scanner.
    let mut boxed: [*mut vdynamic; MAX_ARGS] = [ptr::null_mut(); MAX_ARGS];
    for (i, &arg) in args.iter().enumerate() {
        let kind = unsafe { (**(*fun).args.add(i)).kind };
        boxed[i] = match unsafe { value_to_dyn(arg, kind) } {
            Ok(d) => d,
            Err(message) => {
                return raise_core(ErrorKind::Type, &format!("argument {i}: {message}"));
            }
        };
    }
    let mut result: *mut vdynamic = ptr::null_mut();
    let nargs = args.len() as i32;
    let call = trapped(|| {
        result = unsafe { hlp_dyn_call(closure.cast(), boxed.as_mut_ptr().cast(), nargs) }.cast();
    });
    match call {
        Err(exception) => unsafe { raise_exception(exception) },
        Ok(()) => {
            unsafe { *out = dyn_to_value(result) };
            REPLY_OK
        }
    }
}

/// Pointers below this are the interpreter's stubs, `findex + 1`, not
/// code: what ash's own compiled callers test before a direct call.
const STUB_SENTINEL_LIMIT: usize = 0x100000;

/// The loaded program's module context, for the function cells a stub
/// sentinel names. One program per process.
static MODULE_CONTEXT: AtomicPtr<hl_module_context> = AtomicPtr::new(ptr::null_mut());

pub(crate) fn set_module_context(m: *mut hl_module_context) {
    MODULE_CONTEXT.store(m, Ordering::Release);
}

/// The code behind a closure's `fun`: itself when it is code, else what
/// the function's cell holds now, when that is code; a closure made
/// before its function promoted still names the stub.
unsafe fn code_of(fun: usize) -> Option<*const c_void> {
    if fun >= STUB_SENTINEL_LIMIT {
        return Some(fun as *const c_void);
    }
    let m = MODULE_CONTEXT.load(Ordering::Acquire);
    if fun == 0 || m.is_null() {
        return None;
    }
    let entry = unsafe { *(*m).functions_ptrs.add(fun - 1) } as usize;
    (entry >= STUB_SENTINEL_LIMIT).then_some(entry as *const c_void)
}

/// A signature's kinds, read once: what each argument is placed as and
/// what the result is read as.
struct Kinds {
    n: usize,
    arg: [hl_type_kind; MAX_ARGS],
    /// `ash_native_call`'s codes per argument, 0 integer, 1 `f32`, 2
    /// `f64`, folded into the pattern its table is keyed by.
    pattern: u32,
    ret: hl_type_kind,
    ret_code: u8,
}

/// The kinds of `fun`, `None` when it takes more than a direct call
/// places.
unsafe fn kinds_of(fun: *const hl_type_fun) -> Option<Kinds> {
    let n = unsafe { (*fun).nargs }.max(0) as usize;
    if n > MAX_ARGS {
        return None;
    }
    let mut kinds = Kinds {
        n,
        arg: [hl::HVOID; MAX_ARGS],
        pattern: 0,
        ret: unsafe { (*(*fun).ret).kind },
        ret_code: 0,
    };
    let mut codes = [0u8; MAX_ARGS];
    for a in 0..n {
        let kind = unsafe { (**(*fun).args.add(a)).kind };
        kinds.arg[a] = kind;
        codes[a] = match kind {
            hl::HF64 => 2,
            hl::HF32 => 1,
            _ => 0,
        };
    }
    kinds.pattern = ash_native_call::pattern_of(&codes[..n]);
    kinds.ret_code = match kinds.ret {
        hl::HF64 => 2,
        hl::HF32 => 1,
        _ => 0,
    };
    Some(kinds)
}

/// One signature's kinds in the list `kinds_for` keeps: pushed at the
/// head once, read without a lock from then on.
struct Known {
    sig: usize,
    kinds: Kinds,
    next: *const Known,
}

static KNOWN: AtomicPtr<Known> = AtomicPtr::new(ptr::null_mut());

/// The kinds of `sig`, kept once per signature for the direct sends that
/// name it. Readers take no lock: the list only grows, at its head.
fn kinds_for(sig: *const hl_type) -> Option<&'static Kinds> {
    let mut node = KNOWN.load(Ordering::Acquire) as *const Known;
    while let Some(k) = unsafe { node.as_ref() } {
        if k.sig == sig as usize {
            return Some(&k.kinds);
        }
        node = k.next;
    }
    let fun = unsafe { fun_of(sig) }?;
    let kinds = unsafe { kinds_of(fun) }?;
    let fresh = Box::into_raw(Box::new(Known {
        sig: sig as usize,
        kinds,
        next: ptr::null(),
    }));
    let mut head = KNOWN.load(Ordering::Acquire);
    loop {
        unsafe { (*fresh).next = head };
        match KNOWN.compare_exchange(head, fresh, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(unsafe { &(*fresh).kinds }),
            Err(seen) => head = seen,
        }
    }
}

/// Call compiled code directly by its signature: each argument placed as
/// its kind wants, integers, pointers, `f32` or `f64`, and the result
/// read back the same way, with no box on either side. `None` when the
/// signature is one the table does not cover, or an argument cannot take
/// its kind; the caller then goes through `hlp_dyn_call`.
unsafe fn direct_call(
    func: *const c_void,
    sig: *const hl_type,
    bound: Option<*mut c_void>,
    args: &[Value],
    out: *mut Value,
) -> Option<Result<(), *mut vdynamic>> {
    let kinds = kinds_for(sig)?;
    unsafe { call_by_kinds(func, kinds, bound, args, out) }
}

/// `direct_call` with the signature's kinds already read.
#[inline]
unsafe fn call_by_kinds(
    func: *const c_void,
    sig: &Kinds,
    bound: Option<*mut c_void>,
    args: &[Value],
    out: *mut Value,
) -> Option<Result<(), *mut vdynamic>> {
    // A bound value goes first, as a pointer; the pattern then shifts by
    // one integer digit.
    let lead = usize::from(bound.is_some());
    let n = args.len() + lead;
    if n > MAX_ARGS || args.len() != sig.n {
        return None;
    }
    let mut words = [0u64; MAX_ARGS];
    let pattern = if let Some(value) = bound {
        words[0] = value as u64;
        sig.pattern * 3
    } else {
        sig.pattern
    };
    for (a, &arg) in args.iter().enumerate() {
        let i = a + lead;
        let kind = sig.arg[a];
        match kind {
            hl::HF64 => {
                words[i] = arg
                    .as_number()
                    .or_else(|| arg.as_int().map(f64::from))?
                    .to_bits();
            }
            hl::HF32 => {
                let f = arg.as_number().or_else(|| arg.as_int().map(f64::from))? as f32;
                words[i] = u64::from(f.to_bits());
            }
            hl::HUI8 | hl::HUI16 | hl::HI32 | hl::HBOOL => {
                let v = arg
                    .as_int()
                    .or_else(|| arg.as_number().map(|n| n as i32))
                    .or_else(|| arg.as_bool().map(i32::from))?;
                words[i] = i64::from(v) as u64;
            }
            hl::HI64 => {
                let v = arg
                    .as_int()
                    .map(i64::from)
                    .or_else(|| arg.as_number().map(|n| n as i64))?;
                words[i] = v as u64;
            }
            // Pointers: what the boxed path would pass, checked against the
            // declared kind, since nothing casts on a direct call.
            hl::HOBJ
            | hl::HSTRUCT
            | hl::HFUN
            | hl::HDYN
            | hl::HBYTES
            | hl::HARRAY
            | hl::HVIRTUAL
            | hl::HDYNOBJ
            | hl::HABSTRACT
            | hl::HENUM
            | hl::HREF
            | hl::HNULL
            | hl::HTYPE => {
                let p = unsafe { value_to_dyn(arg, kind) }.ok()?;
                if !p.is_null()
                    && kind != hl::HDYN
                    && kind != hl::HNULL
                    && unsafe { kind_of(p) } != kind
                {
                    return None;
                }
                words[i] = p as u64;
            }
            _ => return None,
        }
    }
    let ret_kind = sig.ret;
    let mut raw: Option<i64> = None;
    let call = trapped(|| {
        raw = unsafe {
            ash_native_call::dispatch_by_pattern(
                func as *mut c_void,
                &words[..n],
                sig.ret_code,
                pattern,
            )
        };
    });
    match call {
        Err(exception) => Some(Err(exception)),
        Ok(()) => {
            let raw = raw?;
            let v = match ret_kind {
                hl::HVOID => Value::null(),
                hl::HF64 => Value::number(f64::from_bits(raw as u64)),
                hl::HF32 => Value::number(f64::from(f32::from_bits(raw as u32))),
                hl::HBOOL => Value::bool(raw & 1 != 0),
                hl::HUI8 => Value::int(i32::from(raw as u8)),
                hl::HUI16 => Value::int(i32::from(raw as u16)),
                hl::HI32 => Value::int(raw as i32),
                hl::HI64 => Value::number(raw as f64),
                _ => unsafe { dyn_to_value(raw as *mut vdynamic) },
            };
            unsafe { *out = v };
            Some(Ok(()))
        }
    }
}

/// The direct send for a typed call site: the kinds it was filled with,
/// then `call_by_kinds`. A stub, or a value the kinds cannot take, is
/// left to the plain path.
unsafe extern "C-unwind" fn direct_typed(
    site: *const CallSite,
    func: usize,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    if func < STUB_SENTINEL_LIMIT {
        return REPLY_MISSING;
    }
    let (_, kinds, _) = unsafe { &*site }.words();
    let kinds = unsafe { &*(kinds as *const Kinds) };
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, n) }
    };
    match unsafe { call_by_kinds(func as *const c_void, kinds, None, args, out) } {
        Some(Ok(())) => REPLY_OK,
        Some(Err(exception)) => unsafe { raise_exception(exception) },
        None => REPLY_MISSING,
    }
}

/// The typed dispatcher for Haxe: `func` under `sig`. Compiled code is
/// called directly by its signature; one of the interpreter's stubs takes
/// `hlp_dyn_call` and the closure runner ash registered, as every dynamic
/// call in ash does.
pub(crate) unsafe extern "C-unwind" fn dispatch(
    func: *const c_void,
    sig: *const hl_type,
    site: *mut CallSite,
    args: *const Value,
    nargs: usize,
    out: *mut Value,
) -> u8 {
    let args = if nargs == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, nargs) }
    };
    if func as usize >= STUB_SENTINEL_LIMIT
        && let Some(kinds) = kinds_for(sig)
        && kinds.n == nargs
    {
        match unsafe { call_by_kinds(func, kinds, None, args, out) } {
            Some(Ok(())) => {
                // The next call from this site goes straight to the code.
                if let Some(site) = unsafe { site.as_ref() } {
                    site.set_direct(
                        direct_typed,
                        sig as usize,
                        kinds as *const Kinds as usize,
                        0,
                    );
                }
                return REPLY_OK;
            }
            Some(Err(exception)) => return unsafe { raise_exception(exception) },
            None => {}
        }
    }
    let mut closure = vclosure {
        t: sig as *mut hl_type,
        fun: func as *mut c_void,
        hasValue: 0,
        stackCount: 0,
        value: ptr::null_mut(),
    };
    unsafe { call_closure(&mut closure, args, out) }
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

/// A class's constructor as a callable: allocates an instance of `t` and
/// runs `__constructor__` on it. A core object with no children, rooted
/// for the process by the handle `constructor` takes and keeps.
#[repr(C)]
struct HaxeCtor {
    desc: *const TypeDesc,
    t: *mut hl_type,
    /// `__constructor__`'s cell in the module context.
    cell: *const *const c_void,
    sig: *const hl_type,
    /// The dispatcher's site for the constructor's call.
    site: CallSite,
}

static mut CTOR_DESC: TypeDesc = {
    let mut d = TypeDesc::new(haxe_type());
    d.protocol = &CTOR_PROTO;
    d.name = "haxe constructor".as_ptr();
    d.name_len = "haxe constructor".len();
    d
};

/// The constructor of the class whose instance type is `t`, as a value
/// the registry can hold: `cell` and `sig` are `__constructor__`'s cell in
/// the module context and its full type, `this` first.
pub(crate) fn constructor(
    t: *mut hl_type,
    cell: *const *const c_void,
    sig: *const hl_type,
) -> Value {
    unsafe { CTOR_DESC.lang = lang() };
    let _lock = heap::gc_guard();
    let p = unsafe {
        heap::alloc_gen(
            &raw mut CTOR_DESC as *mut hl_type,
            size_of::<HaxeCtor>(),
            KIND_NOPTR,
        )
    } as *mut HaxeCtor;
    if p.is_null() {
        heap::out_of_memory("a haxe constructor");
    }
    unsafe {
        (*p).desc = &raw const CTOR_DESC;
        (*p).t = t;
        (*p).cell = cell;
        (*p).sig = sig;
        ptr::addr_of_mut!((*p).site).write(CallSite::new());
    }
    // Kept for the process, as the interface that names it is.
    let _keep = heap::handle_new(p as *mut u8);
    Value::object(p as *const c_void)
}

/// Allocate, construct, wrap; the wrapper is the result.
unsafe extern "C-unwind" fn ctor_call(
    obj: *mut u8,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let ctor = unsafe { &*(obj as *const HaxeCtor) };
    if n >= MAX_ARGS {
        return REPLY_UNSUPPORTED;
    }
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, n) }
    };
    // The instance is a raw address on this frame, which the conservative
    // scan sees through the constructor. Its `this` is a wrapper on this
    // frame too: the dispatcher unwraps it and nothing keeps it.
    let instance = unsafe { hlp_alloc_obj(ctor.t.cast()) } as *mut vdynamic;
    let this = HaxeRef {
        desc: haxe_desc(),
        obj: instance,
    };
    let mut with_this = [MaybeUninit::<Value>::uninit(); MAX_ARGS];
    with_this[0].write(Value::object(&this as *const HaxeRef as *const c_void));
    for (slot, &arg) in with_this[1..].iter_mut().zip(args) {
        slot.write(arg);
    }
    let mut ignored = Value::null();
    let code = unsafe {
        typed_send(
            *ctor.cell,
            ctor.sig,
            &ctor.site,
            with_this.as_ptr().cast(),
            n + 1,
            &mut ignored,
        )
    };
    if code == REPLY_OK {
        unsafe { *out = wrap(instance) };
    }
    code
}

/// `dispatch` through a site of this adapter's own: the direct send it
/// left there first, as the bridge sends, and the dispatcher when the
/// site has none or it no longer fits.
unsafe fn typed_send(
    func: *const c_void,
    sig: *const hl_type,
    site: &CallSite,
    args: *const Value,
    nargs: usize,
    out: *mut Value,
) -> u8 {
    if let Some(f) = site.direct() {
        let code = unsafe { f(site, func as usize, args, nargs, out) };
        if code == REPLY_OK || code == REPLY_RAISED {
            return code;
        }
        site.clear_direct();
    }
    unsafe {
        dispatch(
            func,
            sig,
            site as *const CallSite as *mut CallSite,
            args,
            nargs,
            out,
        )
    }
}

static CTOR_PROTO: Protocol = Protocol {
    call: Some(ctor_call),
    ..Protocol::NONE
};

// ---------------------------------------------------------------------------
// The class object
// ---------------------------------------------------------------------------

/// A Haxe class as a value the registry can hold: the receiver of its
/// static fields. It names the instance type and finds the class object,
/// the `hl.Class` instance the program's entry function stores in the
/// type's global, at each use, since it is made after the program
/// publishes and replaced by a reload.
#[repr(C)]
struct HaxeClass {
    desc: *const TypeDesc,
    t: *mut hl_type,
}

static mut CLASS_DESC: TypeDesc = {
    let mut d = TypeDesc::new(haxe_type());
    d.protocol = &CLASS_PROTO;
    d.name = "haxe class".as_ptr();
    d.name_len = "haxe class".len();
    d
};

/// The class whose instance type is `t`, as a value.
pub(crate) fn class_object(t: *mut hl_type) -> Value {
    unsafe { CLASS_DESC.lang = lang() };
    let _lock = heap::gc_guard();
    let p = unsafe {
        heap::alloc_gen(
            &raw mut CLASS_DESC as *mut hl_type,
            size_of::<HaxeClass>(),
            KIND_NOPTR,
        )
    } as *mut HaxeClass;
    if p.is_null() {
        heap::out_of_memory("a haxe class");
    }
    unsafe {
        (*p).desc = &raw const CLASS_DESC;
        (*p).t = t;
    }
    // Kept for the process, as the interface that names it is.
    let _keep = heap::handle_new(p as *mut u8);
    Value::object(p as *const c_void)
}

/// The `hl.Class` instance of the class, once the program has made it.
unsafe fn class_instance(obj: *mut u8) -> Option<*mut vdynamic> {
    let t = unsafe { (*(obj as *const HaxeClass)).t };
    let global = unsafe { (*(*t).detail.obj).global_value };
    if global.is_null() {
        return None;
    }
    let instance = unsafe { *global } as *mut vdynamic;
    (!instance.is_null()).then_some(instance)
}

/// Run `f` on the class instance wrapped as a Haxe object, so the Haxe
/// protocol answers for it.
unsafe fn on_class_instance(obj: *mut u8, f: impl FnOnce(*mut u8) -> u8) -> u8 {
    let Some(instance) = (unsafe { class_instance(obj) }) else {
        return raise_core(
            ErrorKind::Runtime,
            "the class has no class object yet: the program has not started",
        );
    };
    let (wrapper, root) = wrap_rooted(instance);
    let code = f(wrapper.as_object().unwrap() as *mut u8);
    heap::handle_release(root);
    code
}

unsafe extern "C-unwind" fn class_get_member(obj: *mut u8, name: Symbol, out: *mut Value) -> u8 {
    unsafe { on_class_instance(obj, |w| get_member(w, name, out)) }
}

unsafe extern "C-unwind" fn class_set_member(obj: *mut u8, name: Symbol, value: Value) -> u8 {
    unsafe { on_class_instance(obj, |w| set_member(w, name, value)) }
}

unsafe extern "C-unwind" fn class_invoke(
    obj: *mut u8,
    name: Symbol,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    unsafe { on_class_instance(obj, |w| invoke(w, name, args, n, out)) }
}

unsafe extern "C-unwind" fn class_type_name(obj: *mut u8, out: *mut Symbol) -> u8 {
    let t = unsafe { (*(obj as *const HaxeClass)).t };
    let name = unsafe { obj_name(t) }.unwrap_or_default();
    unsafe { *out = caribou::symbol::intern(&name) };
    REPLY_OK
}

static CLASS_PROTO: Protocol = Protocol {
    get_member: Some(class_get_member),
    set_member: Some(class_set_member),
    invoke: Some(class_invoke),
    type_name: Some(class_type_name),
    ..Protocol::NONE
};

/// A new instance of `class`, constructed with `args`: the class's
/// constructor callable through the bridge, on behalf of Haxe.
pub fn construct(class: &ClassIface, args: &[Value]) -> Result<Value, Value> {
    let Some(ctor) = &class.ctor else {
        let e = Error::new(
            ErrorKind::Runtime,
            &format!("{} has no constructor", class.name),
            lang(),
        );
        return Err(Error::value(e));
    };
    bridge::call_named(ctor.target, args, lang(), &format!("{}.new", class.name))
}

/// Whether `callable` is a constructor made here.
pub fn is_constructor(callable: Callable) -> bool {
    match callable {
        Callable::Dynamic(v) => v.as_object().is_some_and(|p| {
            !p.is_null() && ptr::eq(unsafe { desc_of(p as *mut u8) }, &raw const CTOR_DESC)
        }),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

/// HashLink's field hash of a symbol's name: the symbol carries it.
#[inline]
fn field_hash(name: Symbol) -> i32 {
    name.hash()
}

/// A declared field of an object type: its byte offset and type, found
/// up the class chain in the runtime's lookup tables.
unsafe fn field_of(t: *mut hl_type, hfield: i32) -> Option<(usize, *mut hl_type)> {
    if !matches!(unsafe { (*t).kind }, hl::HOBJ | hl::HSTRUCT) {
        return None;
    }
    let mut rt = unsafe { hl_get_obj_proto(t.cast()) } as *mut hl_runtime_obj;
    while !rt.is_null() {
        let f = unsafe { hlp_lookup_find((*rt).lookup.cast(), (*rt).nlookup, hfield) }
            as *mut hl_field_lookup;
        if !f.is_null() {
            // A field's entry holds its byte offset; a method's is negative.
            let offset = unsafe { (*f).field_index };
            if offset < 0 {
                return None;
            }
            return Some((offset as usize, unsafe { (*f).t }));
        }
        rt = unsafe { (*rt).parent };
    }
    None
}

/// Read the field at `offset` of `d` as a value, by its type's kind.
unsafe fn read_field(d: *mut vdynamic, offset: usize, t: *mut hl_type) -> Option<Value> {
    let at = unsafe { (d as *mut u8).add(offset) };
    Some(match unsafe { (*t).kind } {
        hl::HUI8 => Value::int(i32::from(unsafe { *at })),
        hl::HUI16 => Value::int(i32::from(unsafe { *(at as *const u16) })),
        hl::HI32 => Value::int(unsafe { *(at as *const i32) }),
        hl::HI64 => Value::number(unsafe { *(at as *const i64) } as f64),
        hl::HF32 => Value::number(f64::from(unsafe { *(at as *const f32) })),
        hl::HF64 => Value::number(unsafe { *(at as *const f64) }),
        hl::HBOOL => Value::bool(unsafe { *at } != 0),
        hl::HVOID => return None,
        _ => unsafe { dyn_to_value(*(at as *const *mut vdynamic)) },
    })
}

/// Write `value` into the field at `offset` of `d`, by its type's kind.
/// `None` when the value cannot take the kind.
unsafe fn write_field(
    d: *mut vdynamic,
    offset: usize,
    t: *mut hl_type,
    value: Value,
) -> Option<Result<(), String>> {
    let at = unsafe { (d as *mut u8).add(offset) };
    let kind = unsafe { (*t).kind };
    let int = || {
        value
            .as_int()
            .or_else(|| value.as_number().map(|n| n as i32))
            .or_else(|| value.as_bool().map(i32::from))
    };
    let float = || value.as_number().or_else(|| value.as_int().map(f64::from));
    unsafe {
        match kind {
            hl::HUI8 => *at = int()? as u8,
            hl::HUI16 => *(at as *mut u16) = int()? as u16,
            hl::HI32 => *(at as *mut i32) = int()?,
            hl::HI64 => *(at as *mut i64) = int().map(i64::from)?,
            hl::HF32 => *(at as *mut f32) = float()? as f32,
            hl::HF64 => *(at as *mut f64) = float()?,
            hl::HBOOL => *at = u8::from(value.as_bool()?),
            hl::HVOID => return None,
            _ => {
                let p = match value_to_dyn(value, kind) {
                    Ok(p) => p,
                    Err(message) => return Some(Err(message)),
                };
                if !p.is_null() && kind != hl::HDYN && kind != hl::HNULL && kind_of(p) != kind {
                    return None;
                }
                *(at as *mut *mut vdynamic) = p;
            }
        }
    }
    Some(Ok(()))
}

unsafe fn kind_of(d: *mut vdynamic) -> hl_type_kind {
    let t = unsafe { (*d).t };
    if t.is_null() {
        hl::HVOID
    } else {
        unsafe { (*t).kind }
    }
}

/// Whether the object's kind carries members the runtime can look up by
/// name.
fn has_members(kind: hl_type_kind) -> bool {
    matches!(kind, hl::HOBJ | hl::HSTRUCT | hl::HVIRTUAL | hl::HDYNOBJ)
}

/// The method `hfield` of a class instance, by the runtime's lookup chain:
/// its slot in the instance's own method table, so an override wins and a
/// promoted body is what the slot holds, and its full type with `this`
/// first. `None` for a field or an unknown name.
unsafe fn find_method(
    d: *mut vdynamic,
    hfield: i32,
) -> Option<(*const *const c_void, *const hl_type)> {
    unsafe { method_of((*d).t, hfield) }
}

/// `find_method` by the type.
unsafe fn method_of(
    t: *mut hl_type,
    hfield: i32,
) -> Option<(*const *const c_void, *const hl_type)> {
    if !matches!(unsafe { (*t).kind }, hl::HOBJ | hl::HSTRUCT) {
        return None;
    }
    let leaf = unsafe { hl_get_obj_proto(t.cast()) } as *mut hl_runtime_obj;
    if leaf.is_null() || unsafe { (*leaf).methods.is_null() } {
        return None;
    }
    let mut rt = leaf;
    while !rt.is_null() {
        let f = unsafe { hlp_lookup_find((*rt).lookup.cast(), (*rt).nlookup, hfield) }
            as *mut hl_field_lookup;
        if !f.is_null() {
            let index = unsafe { (*f).field_index };
            if index >= 0 {
                return None;
            }
            let index = (-index - 1) as usize;
            if index >= unsafe { (*leaf).nmethods } as usize {
                return None;
            }
            let slot = unsafe { (*leaf).methods.add(index) } as *const *const c_void;
            return Some((slot, unsafe { (*f).t }));
        }
        rt = unsafe { (*rt).parent };
    }
    None
}

/// `hlp_dyn_getp` as a dynamic, under a trap: a field's value boxed, or a
/// method as a bound closure.
unsafe fn get_dyn(d: *mut vdynamic, hfield: i32) -> Result<*mut vdynamic, *mut vdynamic> {
    let mut got: *mut vdynamic = ptr::null_mut();
    trapped(|| {
        got = unsafe { hlp_dyn_getp(d.cast(), hfield, hlt_dyn()) }.cast();
    })
    .map(|()| got)
}

/// A field, a property through `__get_field`, or a method as a bound
/// closure; `Missing` for a name the object's type does not declare and a
/// dynamic object does not hold.
unsafe extern "C-unwind" fn get_member(obj: *mut u8, name: Symbol, out: *mut Value) -> u8 {
    get_at(obj, name, None, out)
}

unsafe extern "C-unwind" fn get_member_at(
    obj: *mut u8,
    name: Symbol,
    site: *mut CallSite,
    out: *mut Value,
) -> u8 {
    get_at(obj, name, unsafe { site.as_ref() }, out)
}

/// The direct send of a field read: the object's type must be the one
/// the site was filled for, and then the field is read where it lies.
unsafe extern "C-unwind" fn direct_get(
    site: *const CallSite,
    obj: usize,
    _args: *const Value,
    _n: usize,
    out: *mut Value,
) -> u8 {
    let d = unsafe { inner(obj as *mut u8) };
    let (key, offset, t) = unsafe { &*site }.words();
    if unsafe { (*d).t } as usize != key {
        return REPLY_MISSING;
    }
    match unsafe { read_field(d, offset, t as *mut hl_type) } {
        Some(v) => {
            unsafe { *out = v };
            REPLY_OK
        }
        None => REPLY_MISSING,
    }
}

/// The direct send of a field write, as [`direct_get`]; a value the
/// field's kind cannot take is left to the plain path.
unsafe extern "C-unwind" fn direct_set(
    site: *const CallSite,
    obj: usize,
    args: *const Value,
    n: usize,
    _out: *mut Value,
) -> u8 {
    let d = unsafe { inner(obj as *mut u8) };
    let (key, offset, t) = unsafe { &*site }.words();
    if n != 1 || unsafe { (*d).t } as usize != key {
        return REPLY_MISSING;
    }
    match unsafe { write_field(d, offset, t as *mut hl_type, *args) } {
        Some(Ok(())) => REPLY_OK,
        Some(Err(message)) => raise_core(ErrorKind::Type, &message),
        None => REPLY_MISSING,
    }
}

/// A declared field's place in an object of type `t`, from `site` when it
/// was filled for `t`, else from the runtime's lookup, left in `site`.
unsafe fn field_at(
    t: *mut hl_type,
    name: Symbol,
    site: Option<&CallSite>,
) -> Option<(usize, *mut hl_type)> {
    if let Some((offset, ft)) = site.and_then(|s| s.get(t as usize)) {
        return Some((offset, ft as *mut hl_type));
    }
    let found = unsafe { field_of(t, field_hash(name)) };
    if let (Some(site), Some((offset, ft))) = (site, found) {
        site.set(t as usize, offset, ft as usize);
    }
    found
}

fn get_at(obj: *mut u8, name: Symbol, site: Option<&CallSite>, out: *mut Value) -> u8 {
    let d = unsafe { inner(obj) };
    if !has_members(unsafe { kind_of(d) }) {
        return REPLY_UNSUPPORTED;
    }
    // A declared field is read where it lies, and the site keeps that as
    // its direct send.
    if let Some((offset, t)) = unsafe { field_at((*d).t, name, site) }
        && let Some(v) = unsafe { read_field(d, offset, t) }
    {
        if let Some(site) = site {
            site.set_direct(direct_get, unsafe { (*d).t } as usize, offset, t as usize);
        }
        unsafe { *out = v };
        return REPLY_OK;
    }
    let hfield = field_hash(name);
    let declared = unsafe { hlp_obj_has_field(d.cast(), hfield) }
        || unsafe { find_method(d, hfield) }.is_some();
    match unsafe { get_dyn(d, hfield) } {
        Err(exception) => unsafe { raise_exception(exception) },
        Ok(got) if got.is_null() && !declared => REPLY_MISSING,
        Ok(got) => {
            unsafe { *out = dyn_to_value(got) };
            REPLY_OK
        }
    }
}

/// A declared field, or any field of a dynamic object.
unsafe extern "C-unwind" fn set_member(obj: *mut u8, name: Symbol, value: Value) -> u8 {
    set_at(obj, name, None, value)
}

unsafe extern "C-unwind" fn set_member_at(
    obj: *mut u8,
    name: Symbol,
    site: *mut CallSite,
    value: Value,
) -> u8 {
    set_at(obj, name, unsafe { site.as_ref() }, value)
}

fn set_at(obj: *mut u8, name: Symbol, site: Option<&CallSite>, value: Value) -> u8 {
    let d = unsafe { inner(obj) };
    let kind = unsafe { kind_of(d) };
    if !has_members(kind) {
        return REPLY_UNSUPPORTED;
    }
    // A declared field is written where it lies, when the value takes its
    // kind, and the site keeps that as its direct send; else the runtime's
    // own conversion.
    if let Some((offset, t)) = unsafe { field_at((*d).t, name, site) } {
        match unsafe { write_field(d, offset, t, value) } {
            Some(Ok(())) => {
                if let Some(site) = site {
                    site.set_direct(direct_set, unsafe { (*d).t } as usize, offset, t as usize);
                }
                return REPLY_OK;
            }
            Some(Err(message)) => return raise_core(ErrorKind::Type, &message),
            None => {}
        }
    }
    let hfield = field_hash(name);
    if kind != hl::HDYNOBJ && !unsafe { hlp_obj_has_field(d.cast(), hfield) } {
        return REPLY_MISSING;
    }
    let boxed = match unsafe { value_to_dyn(value, hl::HDYN) } {
        Ok(b) => b,
        Err(message) => return raise_core(ErrorKind::Type, &message),
    };
    match trapped(|| unsafe { hlp_dyn_setp(d.cast(), hfield, hlt_dyn(), boxed.cast()) }) {
        Err(exception) => unsafe { raise_exception(exception) },
        Ok(()) => REPLY_OK,
    }
}

/// A method of the object's class through the typed dispatcher, with the
/// object as `this`; else a closure held in a field of that name.
unsafe extern "C-unwind" fn invoke(
    obj: *mut u8,
    name: Symbol,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    invoke_at_opt(obj, name, None, args, n, out)
}

unsafe extern "C-unwind" fn invoke_at(
    obj: *mut u8,
    name: Symbol,
    site: *mut CallSite,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    invoke_at_opt(obj, name, unsafe { site.as_ref() }, args, n, out)
}

/// The method `name` of an object of type `t`: its slot in the type's
/// method table and its signature, from `site` when it was filled for
/// `t`, else from the runtime's lookup, left in `site`. The slot is read
/// per call: it is where the runtime installs a promoted body.
unsafe fn method_at(
    d: *mut vdynamic,
    name: Symbol,
    site: Option<&CallSite>,
) -> Option<(*const c_void, *const hl_type)> {
    let t = unsafe { (*d).t };
    if let Some((slot, sig)) = site.and_then(|s| s.get(t as usize)) {
        return Some((
            unsafe { *(slot as *const *const c_void) },
            sig as *const hl_type,
        ));
    }
    let (slot, sig) = unsafe { find_method(d, field_hash(name)) }?;
    if let Some(site) = site {
        site.set(t as usize, slot as usize, sig as usize);
    }
    Some((unsafe { *slot }, sig))
}

fn invoke_at_opt(
    obj: *mut u8,
    name: Symbol,
    site: Option<&CallSite>,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let d = unsafe { inner(obj) };
    if !has_members(unsafe { kind_of(d) }) {
        return REPLY_UNSUPPORTED;
    }
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, n) }
    };
    if let Some((func, sig)) = unsafe { method_at(d, name, site) } {
        // `this` first; on the stack for what a compiled body takes.
        let this = Value::object(obj as *const c_void);
        if n < 10 {
            let mut with_this = [MaybeUninit::<Value>::uninit(); 10];
            with_this[0].write(this);
            for (slot, &arg) in with_this[1..=n].iter_mut().zip(args) {
                slot.write(arg);
            }
            let with_this = with_this.as_ptr().cast::<Value>();
            return unsafe { dispatch(func, sig, ptr::null_mut(), with_this, n + 1, out) };
        }
        let mut with_this = Vec::with_capacity(n + 1);
        with_this.push(this);
        with_this.extend_from_slice(args);
        return unsafe {
            dispatch(
                func,
                sig,
                ptr::null_mut(),
                with_this.as_ptr(),
                with_this.len(),
                out,
            )
        };
    }
    let hfield = field_hash(name);
    if !unsafe { hlp_obj_has_field(d.cast(), hfield) } {
        return REPLY_MISSING;
    }
    let closure = match unsafe { get_dyn(d, hfield) } {
        Err(exception) => return unsafe { raise_exception(exception) },
        Ok(got) => got,
    };
    if closure.is_null() || unsafe { kind_of(closure) } != hl::HFUN {
        return raise_core(
            ErrorKind::Type,
            &format!("`{}` is not a function", name.name()),
        );
    }
    unsafe { call_closure(closure as *mut vclosure, args, out) }
}

/// A closure, called.
/// A closure's arity: what its visible type declares, or none for a
/// variadic one.
unsafe extern "C-unwind" fn arity(obj: *mut u8, out: *mut usize) -> u8 {
    let d = unsafe { inner(obj) };
    if unsafe { kind_of(d) } != hl::HFUN {
        return REPLY_UNSUPPORTED;
    }
    let Some(fun) = (unsafe { fun_of((*d).t) }) else {
        return REPLY_UNSUPPORTED;
    };
    unsafe { *out = (*fun).nargs.max(0) as usize };
    REPLY_OK
}

unsafe extern "C-unwind" fn call(
    obj: *mut u8,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let d = unsafe { inner(obj) };
    if unsafe { kind_of(d) } != hl::HFUN {
        return REPLY_UNSUPPORTED;
    }
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, n) }
    };
    unsafe { call_closure(d as *mut vclosure, args, out) }
}

/// `Std.string` of the object, as a core string. Runs the object's own
/// `toString`, so under a trap.
unsafe extern "C-unwind" fn to_string(obj: *mut u8, out: *mut Value) -> u8 {
    let d = unsafe { inner(obj) };
    let mut text: *const uchar = ptr::null();
    let mut len: i32 = 0;
    let rendered = trapped(|| {
        text = unsafe { hlp_value_to_string(d.cast(), &mut len) } as *const uchar;
    });
    if let Err(exception) = rendered {
        return unsafe { raise_exception(exception) };
    }
    let s = if text.is_null() || len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len as usize) })
    };
    unsafe { *out = Str::value(Str::new(&s)) };
    REPLY_OK
}

/// The object's address: identity, whichever wrapper holds it.
unsafe extern "C-unwind" fn hash(obj: *mut u8, out: *mut u64) -> u8 {
    unsafe { *out = inner(obj) as usize as u64 };
    REPLY_OK
}

/// Identity of the wrapped objects.
unsafe extern "C-unwind" fn equals(obj: *mut u8, other: Value, out: *mut bool) -> u8 {
    unsafe { *out = unwrap(other) == Some(inner(obj)) };
    REPLY_OK
}

unsafe extern "C-unwind" fn unwrap_native(obj: *mut u8, out: *mut *mut c_void) -> u8 {
    unsafe { *out = inner(obj).cast() };
    REPLY_OK
}

/// The class name of an object or struct, interned: what the registry
/// publishes it under. Kept per type, since every object crossing asks.
unsafe extern "C-unwind" fn type_name(obj: *mut u8, out: *mut Symbol) -> u8 {
    static NAMES: RwLock<Vec<(usize, Symbol)>> = RwLock::new(Vec::new());
    let d = unsafe { inner(obj) };
    let t = unsafe { (*d).t } as usize;
    if let Some(&(_, sym)) = NAMES.read().unwrap().iter().find(|(key, _)| *key == t) {
        unsafe { *out = sym };
        return REPLY_OK;
    }
    match unsafe { obj_name(t as *const hl_type) } {
        Some(name) => {
            let sym = caribou::symbol::intern(&name);
            NAMES.write().unwrap().push((t, sym));
            unsafe { *out = sym };
            REPLY_OK
        }
        None => REPLY_UNSUPPORTED,
    }
}

// ---------------------------------------------------------------------------
// Sequences: HashLink's arrays
// ---------------------------------------------------------------------------

/// The names an array answers through: `getDyn` and `setDyn` of
/// `hl.types.ArrayAccess`, which every array kind overrides, and its
/// length, a declared field on `ArrayBase` and a `get_length` method on
/// `ArrayDyn`.
struct ArrayNames {
    length: Symbol,
    get_length: Symbol,
    get_dyn: Symbol,
    set_dyn: Symbol,
}

fn array_names() -> &'static ArrayNames {
    static NAMES: OnceLock<ArrayNames> = OnceLock::new();
    NAMES.get_or_init(|| ArrayNames {
        length: caribou::symbol::intern("length"),
        get_length: caribou::symbol::intern("get_length"),
        get_dyn: caribou::symbol::intern("getDyn"),
        set_dyn: caribou::symbol::intern("setDyn"),
    })
}

/// A method of an array type: its slot, read per call since a promoted
/// body lands there, and its signature with the kinds read.
#[derive(Clone, Copy)]
struct Slot {
    at: *const *const c_void,
    sig: *const hl_type,
    kinds: &'static Kinds,
}

unsafe impl Sync for Slot {}
unsafe impl Send for Slot {}

/// Where an array type keeps its length.
#[derive(Clone, Copy)]
enum Length {
    Field(usize, *mut hl_type),
    Method(Slot),
}

/// How an array type answers: its length, `getDyn` and `setDyn`.
#[derive(Clone, Copy)]
struct ArrayShape {
    length: Length,
    get_dyn: Slot,
    set_dyn: Slot,
}

/// One type's answer in the list `shape_of` keeps: `None` for a type that
/// is not an array, so the question costs one walk either way.
struct Shaped {
    t: usize,
    shape: Option<ArrayShape>,
    next: *const Shaped,
}

static SHAPES: AtomicPtr<Shaped> = AtomicPtr::new(ptr::null_mut());

/// The array shape of `d`'s type, if it is one. Kept once per type;
/// readers take no lock, the list only grows at its head.
fn shape_of(d: *mut vdynamic) -> Option<&'static ArrayShape> {
    let t = unsafe { (*d).t };
    let mut node = SHAPES.load(Ordering::Acquire) as *const Shaped;
    while let Some(s) = unsafe { node.as_ref() } {
        if s.t == t as usize {
            return s.shape.as_ref();
        }
        node = s.next;
    }
    let fresh = Box::into_raw(Box::new(Shaped {
        t: t as usize,
        shape: unsafe { array_shape(t) },
        next: ptr::null(),
    }));
    let mut head = SHAPES.load(Ordering::Acquire);
    loop {
        unsafe { (*fresh).next = head };
        match SHAPES.compare_exchange(head, fresh, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return unsafe { (*fresh).shape.as_ref() },
            Err(seen) => head = seen,
        }
    }
}

unsafe fn slot_of(t: *mut hl_type, name: Symbol) -> Option<Slot> {
    let (at, sig) = unsafe { method_of(t, field_hash(name)) }?;
    Some(Slot {
        at,
        sig,
        kinds: kinds_for(sig)?,
    })
}

/// Whether `t` is a HashLink array: a `getDyn` and a `setDyn` method,
/// and a length to read.
unsafe fn array_shape(t: *mut hl_type) -> Option<ArrayShape> {
    let names = array_names();
    let get_dyn = unsafe { slot_of(t, names.get_dyn) }?;
    let set_dyn = unsafe { slot_of(t, names.set_dyn) }?;
    let length = match unsafe { field_of(t, field_hash(names.length)) } {
        Some((offset, ft)) => Length::Field(offset, ft),
        None => Length::Method(unsafe { slot_of(t, names.get_length) }?),
    };
    Some(ArrayShape {
        length,
        get_dyn,
        set_dyn,
    })
}

/// Call an array's method on it, `this` first: compiled code by its
/// kinds, an interpreter stub through `dispatch`.
unsafe fn send_array(slot: Slot, obj: *mut u8, args: &[Value], out: *mut Value) -> u8 {
    let mut with_this = [Value::object(obj as *const c_void); 3];
    with_this[1..=args.len()].copy_from_slice(args);
    let with_this = &with_this[..=args.len()];
    let func = unsafe { *slot.at };
    if func as usize >= STUB_SENTINEL_LIMIT
        && let Some(sent) = unsafe { call_by_kinds(func, slot.kinds, None, with_this, out) }
    {
        return match sent {
            Ok(()) => REPLY_OK,
            Err(exception) => unsafe { raise_exception(exception) },
        };
    }
    unsafe {
        dispatch(
            func,
            slot.sig,
            ptr::null_mut(),
            with_this.as_ptr(),
            with_this.len(),
            out,
        )
    }
}

unsafe fn array_len(shape: &ArrayShape, obj: *mut u8, d: *mut vdynamic) -> Result<usize, u8> {
    let mut length = Value::null();
    match shape.length {
        Length::Field(offset, t) => match unsafe { read_field(d, offset, t) } {
            Some(v) => length = v,
            None => return Err(REPLY_UNSUPPORTED),
        },
        Length::Method(slot) => {
            let code = unsafe { send_array(slot, obj, &[], &mut length) };
            if code != REPLY_OK {
                return Err(code);
            }
        }
    }
    Ok(length.as_int().unwrap_or(0).max(0) as usize)
}

unsafe extern "C-unwind" fn len(obj: *mut u8, out: *mut usize) -> u8 {
    let d = unsafe { inner(obj) };
    let Some(shape) = shape_of(d) else {
        return REPLY_UNSUPPORTED;
    };
    match unsafe { array_len(shape, obj, d) } {
        Ok(n) => {
            unsafe { *out = n };
            REPLY_OK
        }
        Err(code) => code,
    }
}

fn position(key: Value) -> Option<i32> {
    key.as_int().or_else(|| key.as_number().map(|n| n as i32))
}

unsafe extern "C-unwind" fn index(obj: *mut u8, key: Value, out: *mut Value) -> u8 {
    let d = unsafe { inner(obj) };
    let Some(shape) = shape_of(d) else {
        return REPLY_UNSUPPORTED;
    };
    let Some(pos) = position(key) else {
        return REPLY_MISSING;
    };
    let length = match unsafe { array_len(shape, obj, d) } {
        Ok(n) => n,
        Err(code) => return code,
    };
    if pos < 0 || pos as usize >= length {
        return REPLY_MISSING;
    }
    unsafe { send_array(shape.get_dyn, obj, &[Value::int(pos)], out) }
}

unsafe extern "C-unwind" fn set_index(obj: *mut u8, key: Value, value: Value) -> u8 {
    let d = unsafe { inner(obj) };
    let Some(shape) = shape_of(d) else {
        return REPLY_UNSUPPORTED;
    };
    let Some(pos) = position(key) else {
        return REPLY_MISSING;
    };
    if pos < 0 {
        return REPLY_MISSING;
    }
    let mut ignored = Value::null();
    unsafe { send_array(shape.set_dyn, obj, &[Value::int(pos), value], &mut ignored) }
}

/// The elements in order: the state is the next position.
unsafe extern "C-unwind" fn iterate(obj: *mut u8, state: *mut Value, out: *mut Value) -> u8 {
    let d = unsafe { inner(obj) };
    let Some(shape) = shape_of(d) else {
        return REPLY_UNSUPPORTED;
    };
    let pos = unsafe { *state }.as_int().unwrap_or(0).max(0);
    let length = match unsafe { array_len(shape, obj, d) } {
        Ok(n) => n,
        Err(code) => return code,
    };
    if pos as usize >= length {
        return REPLY_MISSING;
    }
    let code = unsafe { send_array(shape.get_dyn, obj, &[Value::int(pos)], out) };
    if code == REPLY_OK {
        unsafe { *state = Value::int(pos + 1) };
    }
    code
}

static HAXE_PROTO: Protocol = Protocol {
    get_member: Some(get_member),
    set_member: Some(set_member),
    invoke: Some(invoke),
    get_member_at: Some(get_member_at),
    set_member_at: Some(set_member_at),
    invoke_at: Some(invoke_at),
    call: Some(call),
    arity: Some(arity),
    index: Some(index),
    set_index: Some(set_index),
    len: Some(len),
    iterate: Some(iterate),
    to_string: Some(to_string),
    hash: Some(hash),
    equals: Some(equals),
    unwrap_native: Some(unwrap_native),
    type_name: Some(type_name),
    ..Protocol::NONE
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wrapper_is_two_words_with_the_descriptor_first() {
        assert_eq!(size_of::<HaxeRef>(), 16);
        assert_eq!(std::mem::offset_of!(HaxeRef, desc), 0);
        assert_eq!(std::mem::offset_of!(HaxeRef, obj), 8);
    }

    #[test]
    fn runtime_messages_map_to_kinds() {
        assert_eq!(kind_of_message("Null access"), ErrorKind::NullAccess);
        assert_eq!(kind_of_message("Out of bounds"), ErrorKind::Index);
        assert_eq!(
            kind_of_message("Array index out of bounds"),
            ErrorKind::Index
        );
        assert_eq!(kind_of_message("Divide by zero"), ErrorKind::Arithmetic);
        assert_eq!(kind_of_message("Stack overflow"), ErrorKind::StackOverflow);
        assert_eq!(kind_of_message("Can't cast"), ErrorKind::Runtime);
    }

    #[test]
    fn non_objects_never_unwrap() {
        assert_eq!(unwrap(Value::null()), None);
        assert_eq!(unwrap(Value::int(3)), None);
        assert_eq!(unwrap(Value::number(1.5)), None);
        assert_eq!(unwrap(Value::bool(true)), None);
    }
}
