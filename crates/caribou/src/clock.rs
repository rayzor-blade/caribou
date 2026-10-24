//! The world's clock as a class every language imports, `core:Clock`: a
//! host steps it, moves it on by its fixed step and reads it, and every
//! timed wait of the world's tasks follows it (`sched::step_clock`).

use std::sync::{LazyLock, Once};
use std::time::{Duration, Instant};

use caribou_abi::{ErrorKind, Value};

use crate::error::Error;
use crate::protocol::Callable;
use crate::registry::{self, ClassIface, Interface, MethodIface, TypeRef};
use crate::sched;
use crate::world::LANG_CORE;

/// What `now()` counts from.
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

fn type_error(message: &str) -> Value {
    Error::value(Error::new(ErrorKind::Type, message, LANG_CORE))
}

fn step(_: &[Value]) -> Result<Value, Value> {
    sched::step_clock();
    Ok(Value::null())
}

/// Moves a stepped clock on by a count of seconds; false on wall time.
fn advance(args: &[Value]) -> Result<Value, Value> {
    let seconds = args
        .first()
        .and_then(|v| v.as_number().or_else(|| v.as_int().map(f64::from)))
        .filter(|s| s.is_finite() && *s >= 0.0)
        .ok_or_else(|| type_error("Clock.advance takes a count of seconds, zero or more"))?;
    Ok(Value::bool(sched::advance_clock(Duration::from_secs_f64(
        seconds,
    ))))
}

/// The world's time in seconds.
fn now(_: &[Value]) -> Result<Value, Value> {
    Ok(Value::number(
        sched::now().saturating_duration_since(*EPOCH).as_secs_f64(),
    ))
}

fn wall(_: &[Value]) -> Result<Value, Value> {
    sched::wall_clock();
    Ok(Value::null())
}

fn stepped(_: &[Value]) -> Result<Value, Value> {
    Ok(Value::bool(sched::clock_is_stepped()))
}

pub(crate) fn publish() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        LazyLock::force(&EPOCH);
        let method = |name: &str,
                      params: Vec<TypeRef>,
                      ret: TypeRef,
                      f: fn(&[Value]) -> Result<Value, Value>| MethodIface {
            name: name.to_owned(),
            is_static: true,
            params,
            ret,
            target: Callable::Core(f),
            native: None,
        };
        registry::publish(Interface {
            lang: LANG_CORE,
            module: "Clock".to_owned(),
            classes: vec![ClassIface {
                name: "Clock".to_owned(),
                type_name: "caribou.Clock".to_owned(),
                superclass: None,
                fields: Vec::new(),
                statics: Vec::new(),
                methods: vec![
                    method("step", Vec::new(), TypeRef::Void, step),
                    method("advance", vec![TypeRef::Float], TypeRef::Bool, advance),
                    method("now", Vec::new(), TypeRef::Float, now),
                    method("wall", Vec::new(), TypeRef::Void, wall),
                    method("stepped", Vec::new(), TypeRef::Bool, stepped),
                ],
                ctor: None,
                class_object: Value::null(),
            }],
            functions: Vec::new(),
        })
        .expect("the core Clock interface is unique");
    });
}
