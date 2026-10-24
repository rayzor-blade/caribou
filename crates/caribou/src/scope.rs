//! Task scopes as a class every language imports, `core:Scope`: a host
//! opens one, starts tasks in it, and cancels them together, as an editor
//! stops a game it previews (`sched::cancel_scope`).

use std::sync::Once;

use caribou_abi::{ErrorKind, Value};

use crate::error::Error;
use crate::protocol::Callable;
use crate::registry::{self, ClassIface, Interface, MethodIface, TypeRef};
use crate::sched;
use crate::world::LANG_CORE;

fn scope_arg(args: &[Value], member: &str) -> Result<u64, Value> {
    args.first()
        .and_then(|v| v.as_int())
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| {
            Error::value(Error::new(
                ErrorKind::Type,
                &format!("Scope.{member} takes a scope, as Scope.open() gave it"),
                LANG_CORE,
            ))
        })
}

fn id(scope: u64) -> Value {
    Value::int(i32::try_from(scope).unwrap_or(i32::MAX))
}

fn open(_: &[Value]) -> Result<Value, Value> {
    Ok(id(sched::new_scope()))
}

/// Tasks the caller starts from now on join `scope`; returns the scope it
/// replaces.
fn enter(args: &[Value]) -> Result<Value, Value> {
    Ok(id(sched::enter_scope(scope_arg(args, "enter")?)))
}

fn current(_: &[Value]) -> Result<Value, Value> {
    Ok(id(sched::current_scope()))
}

/// Cancels the scope's tasks; returns how many it reached.
fn cancel(args: &[Value]) -> Result<Value, Value> {
    let reached = sched::cancel_scope(scope_arg(args, "cancel")?);
    Ok(Value::int(i32::try_from(reached).unwrap_or(i32::MAX)))
}

fn cancelled(_: &[Value]) -> Result<Value, Value> {
    Ok(Value::bool(sched::cancelled()))
}

pub(crate) fn publish() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
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
            module: "Scope".to_owned(),
            classes: vec![ClassIface {
                name: "Scope".to_owned(),
                type_name: "caribou.Scope".to_owned(),
                superclass: None,
                fields: Vec::new(),
                statics: Vec::new(),
                methods: vec![
                    method("open", Vec::new(), TypeRef::Int, open),
                    method("enter", vec![TypeRef::Int], TypeRef::Int, enter),
                    method("current", Vec::new(), TypeRef::Int, current),
                    method("cancel", vec![TypeRef::Int], TypeRef::Int, cancel),
                    method("cancelled", Vec::new(), TypeRef::Bool, cancelled),
                ],
                ctor: None,
                class_object: Value::null(),
            }],
            functions: Vec::new(),
        })
        .expect("the core Scope interface is unique");
    });
}
