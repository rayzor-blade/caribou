//! Language-neutral eventual values backed by the shared scheduler.
//!
//! A future is a core heap object. Its settled value stays in that object and
//! is traced there; waiters are scheduler tokens, so waiting parks one task
//! without blocking its world. Plugins see only the one-word ABI carrier.

use std::ptr;
use std::sync::{Mutex, Once};

use caribou_abi::Value;

use crate::error::{Error, Rooted, Str};
use crate::heap::{self, Tracer, TypeDesc};
use crate::protocol::{self, Callable, Protocol, REPLY_MISSING, REPLY_OK, REPLY_RAISED};
use crate::registry::{ClassIface, Interface, MethodIface, TypeRef};
use crate::sched::{self, Waiter};
use crate::symbol::{self, Symbol};
use crate::world::LANG_CORE;

#[derive(Clone, Copy)]
enum Settlement {
    Resolved(Value),
    Rejected(Value),
}

struct State {
    settlement: Option<Settlement>,
    waiters: Vec<Waiter>,
}

#[repr(C)]
pub struct FutureData {
    desc: *const TypeDesc,
    state: Mutex<State>,
}

const fn descriptor() -> TypeDesc {
    let name = "caribou.Future";
    let mut d = TypeDesc::new(crate::error::core_type());
    d.name = name.as_ptr();
    d.name_len = name.len();
    d.trace = Some(trace_future);
    d.drop = Some(drop_future);
    d.protocol = &FUTURE_PROTO;
    d
}

pub static FUTURE_DESC: TypeDesc = descriptor();

pub fn new() -> *mut FutureData {
    let root = Rooted::alloc(&FUTURE_DESC, size_of::<FutureData>());
    let future = root.ptr().cast::<FutureData>();
    unsafe {
        ptr::addr_of_mut!((*future).state).write(Mutex::new(State {
            settlement: None,
            waiters: Vec::new(),
        }));
    }
    future
}

pub fn of(value: Value) -> Option<*mut FutureData> {
    let object = crate::cell::unwrap(value).as_object()?.cast::<u8>();
    (!object.is_null() && ptr::eq(unsafe { protocol::desc_of(object) }, &FUTURE_DESC))
        .then_some(object.cast())
}

/// Whether the future has settled.
///
/// # Safety
/// `future` is null or a future from [`new`] or [`of`] that is still alive.
pub unsafe fn ready(future: *const FutureData) -> bool {
    if future.is_null() {
        return false;
    }
    unsafe {
        (*future)
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .settlement
            .is_some()
    }
}

/// Complete once and wake all tasks that observed the pending state.
///
/// # Safety
/// `future` is null or a future from [`new`] or [`of`] that is still alive.
pub unsafe fn settle(future: *mut FutureData, value: Value, rejected: bool) -> bool {
    if future.is_null() {
        return false;
    }
    // A rejection reason becomes a Caribou error once, at settlement. A
    // language-owned object remains its native payload and is unwrapped when
    // awaited back in that language; a core string becomes the error message.
    let rejection = rejected.then(|| {
        if unsafe { Error::from_value(value) }.is_some() {
            Rooted::of(value)
        } else if let Some(text) = unsafe { Str::text(value) } {
            Error::new_rooted(caribou_abi::ErrorKind::User, text, LANG_CORE)
        } else if let Some(origin) = crate::bridge::language_of(value) {
            Error::with_native_rooted(value, origin)
        } else {
            Error::new_rooted(
                caribou_abi::ErrorKind::User,
                &crate::bridge::describe(value),
                LANG_CORE,
            )
        }
    });
    let value = rejection.as_ref().map_or(value, Rooted::value);
    // Resolution may come from a backend callback. Serialising it with the
    // collector keeps the traced value and the object's lifetime coherent.
    let _gc = heap::gc_guard();
    let mut state = unsafe { (*future).state.lock().unwrap_or_else(|e| e.into_inner()) };
    if state.settlement.is_some() {
        return false;
    }
    state.settlement = Some(if rejected {
        Settlement::Rejected(value)
    } else {
        Settlement::Resolved(value)
    });
    let waiters = std::mem::take(&mut state.waiters);
    drop(state);
    drop(_gc);
    for waiter in waiters {
        sched::wake(waiter);
    }
    true
}

fn await_settlement(future: *mut FutureData) -> Settlement {
    loop {
        let waiter = sched::new_waiter();
        {
            let mut state = unsafe { (*future).state.lock().unwrap_or_else(|e| e.into_inner()) };
            if let Some(settlement) = state.settlement {
                sched::discard(waiter.token());
                return settlement;
            }
            state.waiters.push(waiter);
        }
        let _ = sched::park(waiter, None);
    }
}

unsafe extern "C" fn trace_future(object: *mut u8, tracer: *mut Tracer) {
    let state = unsafe {
        (*object.cast::<FutureData>())
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    };
    let value = match state.settlement {
        Some(Settlement::Resolved(value) | Settlement::Rejected(value)) => value,
        None => return,
    };
    unsafe { (*tracer).mark_value(value.to_bits()) };
}

unsafe extern "C" fn drop_future(object: *mut u8) {
    unsafe { ptr::drop_in_place(ptr::addr_of_mut!((*object.cast::<FutureData>()).state)) };
}

unsafe extern "C-unwind" fn future_type_name(_object: *mut u8, out: *mut Symbol) -> u8 {
    unsafe { *out = symbol::intern("caribou.Future") };
    REPLY_OK
}

unsafe extern "C-unwind" fn future_invoke(
    object: *mut u8,
    name: Symbol,
    _args: *const Value,
    count: usize,
    out: *mut Value,
) -> u8 {
    let args = if count == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(_args, count) }
    };
    match name.name() {
        "ready" if args.is_empty() => {
            unsafe { *out = Value::bool(ready(object.cast())) };
            REPLY_OK
        }
        "await" if args.is_empty() => match await_settlement(object.cast()) {
            Settlement::Resolved(value) => {
                unsafe { *out = value };
                REPLY_OK
            }
            Settlement::Rejected(error) => {
                crate::bridge::set_pending(error);
                REPLY_RAISED
            }
        },
        "resolve" if args.len() == 1 => {
            unsafe { *out = Value::bool(settle(object.cast(), args[0], false)) };
            REPLY_OK
        }
        "reject" if args.len() == 1 => {
            unsafe { *out = Value::bool(settle(object.cast(), args[0], true)) };
            REPLY_OK
        }
        _ => REPLY_MISSING,
    }
}

static FUTURE_PROTO: Protocol = Protocol {
    invoke: Some(future_invoke),
    type_name: Some(future_type_name),
    ..Protocol::NONE
};

/// Publish the shared class once so proxy-based frontends can install it.
pub(crate) fn publish() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let method = |name: &str, ret| MethodIface {
            name: name.to_owned(),
            is_static: false,
            params: Vec::new(),
            ret,
            target: Callable::ProtocolMethod {
                name: symbol::intern(name),
            },
        };
        fn construct(args: &[Value]) -> Result<Value, Value> {
            debug_assert!(args.is_empty());
            Ok(Value::object(new().cast()))
        }
        crate::registry::publish(Interface {
            lang: LANG_CORE,
            module: "Future".to_owned(),
            classes: vec![ClassIface {
                name: "Future".to_owned(),
                type_name: "caribou.Future".to_owned(),
                superclass: None,
                fields: Vec::new(),
                statics: Vec::new(),
                methods: vec![
                    method("ready", TypeRef::Bool),
                    method("await", TypeRef::Dyn),
                    MethodIface {
                        name: "resolve".to_owned(),
                        is_static: false,
                        params: vec![TypeRef::Dyn],
                        ret: TypeRef::Bool,
                        target: Callable::ProtocolMethod {
                            name: symbol::intern("resolve"),
                        },
                    },
                    MethodIface {
                        name: "reject".to_owned(),
                        is_static: false,
                        params: vec![TypeRef::Dyn],
                        ret: TypeRef::Bool,
                        target: Callable::ProtocolMethod {
                            name: symbol::intern("reject"),
                        },
                    },
                ],
                ctor: Some(MethodIface {
                    name: "new".to_owned(),
                    is_static: true,
                    params: Vec::new(),
                    ret: TypeRef::Object("caribou.Future".to_owned()),
                    target: Callable::Core(construct),
                }),
                class_object: Value::null(),
            }],
            functions: Vec::new(),
        })
        .expect("the core Future interface is unique");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sched::{DEFAULT_STACK_SIZE, spawn_fiber, tick};
    use std::sync::{Arc, Mutex};

    #[test]
    #[cfg_attr(
        target_family = "wasm",
        ignore = "a parked fiber needs the host's suspension on wasm"
    )]
    fn completion_wakes_all_waiters_and_keeps_one_result() {
        let _heap = heap::gc_guard();
        let future = new();
        let root = Rooted::of(Value::object(future.cast()));
        drop(_heap);
        let seen = Arc::new(Mutex::new(Vec::new()));
        for _ in 0..2 {
            let seen = Arc::clone(&seen);
            let future = future as usize;
            spawn_fiber(DEFAULT_STACK_SIZE, move || {
                let Settlement::Resolved(value) = await_settlement(future as *mut FutureData)
                else {
                    panic!("rejected")
                };
                seen.lock().unwrap().push(value.as_int().unwrap());
            });
        }
        tick(None);
        assert!(unsafe { settle(future, Value::int(42), false) });
        assert!(!unsafe { settle(future, Value::int(7), false) });
        tick(None);
        let mut values = seen.lock().unwrap().clone();
        values.sort();
        assert_eq!(values, [42, 42]);
        drop(root);
    }
}
