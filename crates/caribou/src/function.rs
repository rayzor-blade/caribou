//! A function of any language as a core value: a small object of the
//! core's own language that holds a `Callable` and answers `call` and
//! `arity`. It is what a module-level function is to a language that
//! imports it as a value (a Wren module variable), and what a plugin or
//! a Zyntax function is when it crosses as a value at all, since a
//! `Callable::Typed` is code and a signature, not an object.

use core::ffi::c_void;
use core::mem::size_of;
use core::ptr;

use caribou_abi::mem::{KIND_DYNAMIC, TRACED};
use caribou_abi::{LangId, Value};

use crate::bridge;
use crate::error::{Str, core_type};
use crate::heap::{self, Tracer, TypeDesc};
use crate::protocol::{self, Callable, Protocol, REPLY_OK, REPLY_UNSUPPORTED};
use crate::registry;
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
    /// The published function the callable was read from, when it was
    /// one: what a reload of its module publishes again.
    origin: Option<Origin>,
    /// The epoch the callable was read in.
    epoch: usize,
}

/// A module's function in the registry: the interface's language and
/// module, and the function's name there.
#[derive(Clone, Copy)]
struct Origin {
    lang: LangId,
    module: Symbol,
    member: Symbol,
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
    make(callable, name, arity, None)
}

/// The function `member` of the interface `lang:module`, as a value that
/// follows its module: after a reload publishes the interface again, a
/// call reaches what it publishes for the name. Named `module.member`
/// in the trace.
pub fn of_module(
    lang: LangId,
    module: &str,
    member: &str,
    callable: Callable,
    arity: Option<usize>,
) -> Value {
    let origin = Origin {
        lang,
        module: intern(module),
        member: intern(member),
    };
    make(callable, &format!("{module}.{member}"), arity, Some(origin))
}

fn make(callable: Callable, name: &str, arity: Option<usize>, origin: Option<Origin>) -> Value {
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
        ptr::addr_of_mut!((*p).origin).write(origin);
        (*p).epoch = protocol::epoch();
    }
    Value::object(p as *const c_void)
}

/// The callable behind `v`, when `v` is a function object: for a
/// module's function, what its module publishes now.
pub fn callable_of(v: Value) -> Option<Callable> {
    let obj = v.as_object()?;
    if obj.is_null()
        || !ptr::eq(
            unsafe { crate::protocol::desc_of(obj as *const u8) },
            &FUNCTION_DESC,
        )
    {
        return None;
    }
    Some(unsafe { current(obj as *mut Function) })
}

/// The function's callable as of this epoch. A module function whose
/// epoch has passed is read again from its interface, which a reload
/// published again; a module withdrawn since keeps the callable it had.
/// Every reader of one function in one epoch writes the same words.
unsafe fn current(f: *mut Function) -> Callable {
    let epoch = protocol::epoch();
    let f = unsafe { &mut *f };
    if f.epoch != epoch {
        if let Some(origin) = f.origin
            && let Some(iface) = registry::interface(origin.lang, origin.module.name())
            && let Some(m) = iface
                .functions
                .iter()
                .find(|m| m.name == origin.member.name())
        {
            f.callable = m.target;
        }
        f.epoch = epoch;
    }
    f.callable
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

unsafe extern "C-unwind" fn call(
    obj: *mut u8,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let callable = unsafe { current(obj as *mut Function) };
    let f = unsafe { &*(obj as *const Function) };
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(args, n) }
    };
    match bridge::call_named(callable, args, LANG_CORE, f.name.name()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Interface, MethodIface};
    use crate::world::{Adapter, Config, World};

    struct Fake;

    impl Adapter for Fake {
        fn languages(&self) -> Vec<String> {
            vec!["flang".to_owned()]
        }
        fn assign_languages(&mut self, _ids: &[LangId]) {}
    }

    fn iface(lang: LangId, target: Callable) -> Interface {
        Interface {
            lang,
            module: "fgame/mod".to_owned(),
            classes: Vec::new(),
            functions: vec![MethodIface {
                name: "f".to_owned(),
                is_static: true,
                params: vec![],
                ret: crate::registry::TypeRef::Int,
                target,
            }],
        }
    }

    fn dynamic_int(c: Callable) -> Option<i32> {
        match c {
            Callable::Dynamic(v) => v.as_int(),
            _ => None,
        }
    }

    #[test]
    fn a_module_function_follows_a_reload_of_its_module() {
        let _serial = crate::world::SERIAL.lock().unwrap();
        let world = World::new(Config::default());
        let lang = world.register(Box::new(Fake)).unwrap()[0];
        registry::publish(iface(lang, Callable::Dynamic(Value::int(1)))).unwrap();

        let f = of_module(
            lang,
            "fgame/mod",
            "f",
            Callable::Dynamic(Value::int(1)),
            Some(0),
        );
        let plain = new(Callable::Dynamic(Value::int(1)), "plain", Some(0));
        assert_eq!(callable_of(f).and_then(dynamic_int), Some(1));

        // Published again, as a reload does, then the epoch moves on.
        registry::publish(iface(lang, Callable::Dynamic(Value::int(2)))).unwrap();
        assert_eq!(
            callable_of(f).and_then(dynamic_int),
            Some(1),
            "not before the epoch"
        );
        protocol::bump_epoch();
        assert_eq!(callable_of(f).and_then(dynamic_int), Some(2));
        assert_eq!(
            callable_of(plain).and_then(dynamic_int),
            Some(1),
            "a bare callable stays"
        );

        // The module gone, the function keeps what it had.
        registry::withdraw(lang, "fgame/mod");
        protocol::bump_epoch();
        assert_eq!(callable_of(f).and_then(dynamic_int), Some(2));
        assert!(callable_of(Value::int(3)).is_none());
    }
}
