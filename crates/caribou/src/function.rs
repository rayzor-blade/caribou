//! A function of any language as a core value: a small object of the
//! core's own language that holds a `Callable` and answers `call` and
//! `arity`. It is what a module-level function is to a language that
//! imports it as a value (a Wren module variable), and what a plugin or
//! a Zyntax function is when it crosses as a value at all, since a
//! `Callable::Typed` is code and a signature, not an object.

use core::ffi::c_void;
use core::mem::size_of;
use core::ptr;

use caribou_abi::Value;
use caribou_abi::mem::{KIND_DYNAMIC, TRACED};

use crate::bridge;
use crate::error::{Str, core_type};
use crate::heap::{self, Tracer, TypeDesc};
use crate::protocol::{Callable, Protocol, REPLY_OK, REPLY_UNSUPPORTED};
use crate::symbol::{Symbol, intern};
use crate::world::LANG_CORE;

#[repr(C)]
struct Function {
    desc: *const TypeDesc,
    callable: Callable,
    /// The name the trace shows for a call.
    name: Symbol,
    /// How many arguments `call` takes, when the callable says.
    arity: Option<usize>,
}

pub static FUNCTION_DESC: TypeDesc = {
    let mut d = TypeDesc::new(core_type());
    d.trace = Some(trace);
    d.protocol = &FUNCTION_PROTO;
    d.name = "caribou.Function".as_ptr();
    d.name_len = "caribou.Function".len();
    d.lang = LANG_CORE;
    d
};

/// A `Callable` as a value, unrooted: the caller stores or roots it
/// before allocating again. `arity` is the parameter count the
/// callable's publisher declared, which a typed signature carries and
/// a dynamic callable may not.
pub fn new(callable: Callable, name: &str, arity: Option<usize>) -> Value {
    let _lock = heap::gc_guard();
    let p = unsafe {
        heap::alloc_gen(
            &FUNCTION_DESC as *const TypeDesc as *mut caribou_abi::hl::hl_type,
            size_of::<Function>(),
            KIND_DYNAMIC | TRACED,
        )
    } as *mut Function;
    if p.is_null() {
        heap::out_of_memory("a function");
    }
    unsafe {
        (*p).desc = &FUNCTION_DESC;
        ptr::addr_of_mut!((*p).callable).write(callable);
        (*p).name = intern(name);
        (*p).arity = arity;
    }
    Value::object(p as *const c_void)
}

/// The callable behind `v`, when `v` is a function object.
pub fn callable_of(v: Value) -> Option<Callable> {
    let obj = v.as_object()?;
    if obj.is_null() || !ptr::eq(unsafe { crate::protocol::desc_of(obj as *const u8) }, &FUNCTION_DESC) {
        return None;
    }
    Some(unsafe { (*(obj as *const Function)).callable })
}

/// The values a callable holds: a dynamic callable's object, a Wren
/// method's class.
unsafe extern "C" fn trace(obj: *mut u8, tracer: *mut Tracer) {
    let f = unsafe { &*(obj as *const Function) };
    let held = match f.callable {
        Callable::Dynamic(v) => Some(v),
        Callable::WrenMethod { class, .. } => Some(class),
        Callable::Typed { .. } | Callable::Cell { .. } => None,
    };
    if let Some(v) = held {
        unsafe { (*tracer).mark_value(v.to_bits()) };
    }
}

unsafe extern "C-unwind" fn call(obj: *mut u8, args: *const Value, n: usize, out: *mut Value) -> u8 {
    let f = unsafe { &*(obj as *const Function) };
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(args, n) }
    };
    match bridge::call_named(f.callable, args, LANG_CORE, f.name.name()) {
        Ok(v) => {
            unsafe { *out = v };
            REPLY_OK
        }
        Err(e) => {
            bridge::set_pending(e);
            crate::protocol::REPLY_RAISED
        }
    }
}

unsafe extern "C-unwind" fn arity(obj: *mut u8, out: *mut usize) -> u8 {
    let f = unsafe { &*(obj as *const Function) };
    match f.arity {
        Some(n) => {
            unsafe { *out = n };
            REPLY_OK
        }
        None => REPLY_UNSUPPORTED,
    }
}

unsafe extern "C-unwind" fn type_name(_obj: *mut u8, out: *mut Symbol) -> u8 {
    unsafe { *out = intern("caribou.Function") };
    REPLY_OK
}

unsafe extern "C-unwind" fn to_string(obj: *mut u8, out: *mut Value) -> u8 {
    let f = unsafe { &*(obj as *const Function) };
    let text = format!("<function {}>", f.name.name());
    unsafe { *out = Str::value(Str::new(&text)) };
    REPLY_OK
}

unsafe extern "C-unwind" fn equals(obj: *mut u8, other: Value, out: *mut bool) -> u8 {
    unsafe { *out = other.as_object() == Some(obj as *mut c_void) };
    REPLY_OK
}

unsafe extern "C-unwind" fn hash(obj: *mut u8, out: *mut u64) -> u8 {
    unsafe { *out = obj as u64 };
    REPLY_OK
}

static FUNCTION_PROTO: Protocol = Protocol {
    call: Some(call),
    arity: Some(arity),
    type_name: Some(type_name),
    to_string: Some(to_string),
    equals: Some(equals),
    hash: Some(hash),
    ..Protocol::NONE
};
