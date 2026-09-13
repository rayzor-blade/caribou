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
//! hash `hlp_hash_gen` gives a name.
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

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{LazyLock, Mutex};

use ash_std::bytes::hlp_alloc_bytes;
use ash_std::error::{
    hlp_clear_exc_value, hlp_get_exc_value, hlp_remove_trap_jit, hlp_setup_trap_jit,
};
use ash_std::fun::hlp_dyn_call;
use ash_std::obj::{
    hl_get_obj_proto, hlp_alloc_dynamic, hlp_alloc_dynbool, hlp_alloc_obj, hlp_dyn_getp,
    hlp_dyn_setp, hlp_hash_gen, hlp_lookup_find, hlp_obj_has_field,
};
use ash_std::strings::hlp_value_to_string;
use ash_std::types::{hlt_bytes, hlt_dyn, hlt_f64, hlt_i32, hlt_i64};
use caribou::bridge;
use caribou::error::{Error, Str};
use caribou::heap::{self, Handle, Tracer, TypeDesc};
use caribou::protocol::{
    Callable, Protocol, REPLY_MISSING, REPLY_OK, REPLY_UNSUPPORTED, Symbol, desc_of,
};
use caribou::registry::ClassIface;
use caribou_abi::hl::{
    self, hl_field_lookup, hl_runtime_obj, hl_type, hl_type_detail, hl_type_fun, hl_type_kind,
    uchar, vclosure, vdynamic,
};
use caribou_abi::mem::{KIND_DYNAMIC, KIND_NOPTR, TRACED};
use caribou_abi::{ErrorKind, LangId, Value};

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
    let (v, root) = wrap_rooted(obj);
    heap::handle_release(root);
    v
}

/// [`wrap`], with a handle the caller releases.
fn wrap_rooted(obj: *mut vdynamic) -> (Value, Handle) {
    let _lock = heap::gc_guard();
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
        setup: unsafe extern "C" fn() -> *mut c_void,
        remove: unsafe extern "C" fn(),
        callback: unsafe extern "C" fn(*mut c_void),
        context: *mut c_void,
    ) -> i32;
}

/// Run `f` under a HashLink trap; `Err` is what it threw. A throw abandons
/// the frames inside `f` without running their drops, so `f` owns nothing
/// that needs one: it reads and writes slots the caller prepared.
fn trapped<F: FnMut()>(mut f: F) -> Result<(), *mut vdynamic> {
    unsafe extern "C" fn thunk<F: FnMut()>(context: *mut c_void) {
        unsafe { (*(context as *mut F))() }
    }
    let threw = unsafe {
        caribou_ash_run_with_hl_trap(
            hlp_setup_trap_jit,
            hlp_remove_trap_jit,
            thunk::<F>,
            &mut f as *mut F as *mut c_void,
        )
    };
    if threw == 0 {
        return Ok(());
    }
    let exception = unsafe { hlp_get_exc_value() };
    unsafe { hlp_clear_exc_value() };
    Err(exception.cast())
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
                // Its bytes are the first field, one pointer past the type.
                let bytes = unsafe {
                    *((exc as *const u8).add(size_of::<*mut hl_type>()) as *const *const uchar)
                };
                (ErrorKind::User, unsafe { utf16z(bytes) })
            } else {
                (ErrorKind::User, name)
            }
        }
        kind => (ErrorKind::User, format!("a thrown value of kind {kind}")),
    }
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
    unsafe { obj_name((*d).t) }.as_deref() == Some("String")
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

/// The typed dispatcher for Haxe: `func` under `sig`, as a closure without
/// a bound value, through `hlp_dyn_call`. A code pointer that is one of the
/// interpreter's stubs takes the closure runner ash registered, as every
/// dynamic call in ash does.
pub(crate) unsafe extern "C-unwind" fn dispatch(
    func: *const c_void,
    sig: *const hl_type,
    args: *const Value,
    nargs: usize,
    out: *mut Value,
) -> u8 {
    let args = if nargs == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, nargs) }
    };
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
    func: *const c_void,
    sig: *const hl_type,
}

static mut CTOR_DESC: TypeDesc = {
    let mut d = TypeDesc::new(haxe_type());
    d.protocol = &CTOR_PROTO;
    d.name = "haxe constructor".as_ptr();
    d.name_len = "haxe constructor".len();
    d
};

/// The constructor of the class whose instance type is `t`, as a value
/// the registry can hold: `func` and `sig` are `__constructor__`'s code
/// pointer and full type, `this` first.
pub(crate) fn constructor(t: *mut hl_type, func: *const c_void, sig: *const hl_type) -> Value {
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
        (*p).func = func;
        (*p).sig = sig;
    }
    // Kept for the process, as the interface that names it is.
    let _keep = heap::handle_new(p as *mut u8);
    Value::object(p as *const c_void)
}

/// Allocate, wrap, construct; the wrapper is the result.
unsafe extern "C-unwind" fn ctor_call(
    obj: *mut u8,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let ctor = unsafe { &*(obj as *const HaxeCtor) };
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, n) }
    };
    let instance = unsafe { hlp_alloc_obj(ctor.t.cast()) } as *mut vdynamic;
    let (this, root) = wrap_rooted(instance);
    let mut with_this = Vec::with_capacity(n + 1);
    with_this.push(this);
    with_this.extend_from_slice(args);
    let mut ignored = Value::null();
    let code = unsafe {
        dispatch(
            ctor.func,
            ctor.sig,
            with_this.as_ptr(),
            with_this.len(),
            &mut ignored,
        )
    };
    heap::handle_release(root);
    if code == REPLY_OK {
        unsafe { *out = this };
    }
    code
}

static CTOR_PROTO: Protocol = Protocol {
    call: Some(ctor_call),
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

/// HashLink's hash of a symbol's name, through ash's own table so a
/// collision resolves as the loader resolved it. Computed once per symbol.
fn field_hash(name: Symbol) -> i32 {
    static HASHES: LazyLock<Mutex<HashMap<Symbol, i32>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    if let Some(&h) = HASHES.lock().unwrap().get(&name) {
        return h;
    }
    let mut units: Vec<uchar> = name.name().encode_utf16().collect();
    units.push(0);
    let h = unsafe { hlp_hash_gen(units.as_ptr(), true) };
    HASHES.lock().unwrap().insert(name, h);
    h
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
/// its code pointer, from the instance's own method table so an override
/// wins, and its full type with `this` first. `None` for a field or an
/// unknown name.
unsafe fn find_method(d: *mut vdynamic, hfield: i32) -> Option<(*const c_void, *const hl_type)> {
    let t = unsafe { (*d).t };
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
            let func = unsafe { *(*leaf).methods.add(index) };
            return Some((func, unsafe { (*f).t }));
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
    let d = unsafe { inner(obj) };
    if !has_members(unsafe { kind_of(d) }) {
        return REPLY_UNSUPPORTED;
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
    let d = unsafe { inner(obj) };
    let kind = unsafe { kind_of(d) };
    if !has_members(kind) {
        return REPLY_UNSUPPORTED;
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
    let d = unsafe { inner(obj) };
    if !has_members(unsafe { kind_of(d) }) {
        return REPLY_UNSUPPORTED;
    }
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, n) }
    };
    let hfield = field_hash(name);
    if let Some((func, sig)) = unsafe { find_method(d, hfield) } {
        let mut with_this = Vec::with_capacity(n + 1);
        with_this.push(Value::object(obj as *const c_void));
        with_this.extend_from_slice(args);
        return unsafe { dispatch(func, sig, with_this.as_ptr(), with_this.len(), out) };
    }
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

/// The class name of an object or struct: what the registry publishes it
/// under.
unsafe extern "C-unwind" fn type_name(obj: *mut u8, out: *mut Value) -> u8 {
    let d = unsafe { inner(obj) };
    match unsafe { obj_name((*d).t) } {
        Some(name) => {
            unsafe { *out = Str::value(Str::new(&name)) };
            REPLY_OK
        }
        None => REPLY_UNSUPPORTED,
    }
}

/// Sequence access is not answered yet: a Haxe array is a class instance
/// whose element storage differs by element type.
static HAXE_PROTO: Protocol = Protocol {
    get_member: Some(get_member),
    set_member: Some(set_member),
    invoke: Some(invoke),
    call: Some(call),
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
