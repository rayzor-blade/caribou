//! Module lifecycle as a class every language imports, `core:Modules`:
//! `onReload(handler)` hears each module's reload, before its new body
//! runs and after, so a host can drop what the module registered with it
//! and let the new body register again.

use std::rc::Rc;
use std::sync::Once;

use caribou_abi::{ErrorKind, Value};

use crate::bridge;
use crate::error::{Error, Str};
use crate::heap;
use crate::protocol::Callable;
use crate::registry::{self, ClassIface, Interface, MethodIface, TypeRef};
use crate::world::{Event, EventKind, LANG_CORE, World};

fn text(s: &str) -> Value {
    Str::value(Str::new(s))
}

/// Subscribe `handler(namespace, module, before, error)`: `before` true
/// once a module's new source compiled and before its body runs, false
/// after the reload, with `error` the reason it failed (null when it did
/// not). A source that does not compile is heard only after, with its
/// error, and the module is as it was.
fn on_reload(args: &[Value]) -> Result<Value, Value> {
    let handler = args
        .first()
        .copied()
        .and_then(Value::as_object)
        .filter(|p| !p.is_null())
        .ok_or_else(|| {
            Error::value(Error::new(
                ErrorKind::Type,
                "Modules.onReload takes a function",
                LANG_CORE,
            ))
        })?;
    let world = World::current().ok_or_else(|| {
        Error::value(Error::new(
            ErrorKind::Runtime,
            "Modules.onReload: no world on this thread",
            LANG_CORE,
        ))
    })?;
    // Rooted for the world's life, as the subscription is.
    let handler = heap::handle_new(handler.cast());
    let call = Rc::new(
        move |lang, module: &str, before: bool, error: Option<&str>| {
            let (namespace, module) = registry::import_name(lang, module);
            let args = [
                text(&namespace),
                text(&module),
                Value::bool(before),
                error.map_or_else(Value::null, text),
            ];
            let handler = Value::object(heap::handle_get(handler).cast());
            if let Err(raised) = bridge::call(Callable::Dynamic(handler), &args, LANG_CORE) {
                eprintln!(
                    "[caribou] a reload handler raised: {}",
                    crate::diag::render_string(
                        &crate::diag::report(raised),
                        &crate::diag::NoSources,
                        false
                    )
                );
            }
        },
    );
    let before = Rc::clone(&call);
    world.on(EventKind::Reloading, move |event| {
        if let Event::Reloading { lang, module } = event {
            before(*lang, module, true, None);
        }
    });
    world.on(EventKind::Reload, move |event| {
        if let Event::Reload {
            lang,
            module,
            error,
        } = event
        {
            call(*lang, module, false, error.as_deref());
        }
    });
    Ok(Value::null())
}

pub(crate) fn publish() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        registry::publish(Interface {
            lang: LANG_CORE,
            module: "Modules".to_owned(),
            classes: vec![ClassIface {
                name: "Modules".to_owned(),
                type_name: "caribou.Modules".to_owned(),
                superclass: None,
                fields: Vec::new(),
                statics: Vec::new(),
                methods: vec![MethodIface {
                    name: "onReload".to_owned(),
                    is_static: true,
                    params: vec![TypeRef::Fun],
                    ret: TypeRef::Void,
                    target: Callable::Core(on_reload),
                    native: None,
                }],
                ctor: None,
                class_object: Value::null(),
            }],
            functions: Vec::new(),
        })
        .expect("the core Modules interface is unique");
    });
}
