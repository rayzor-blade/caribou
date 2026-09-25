//! The world as a Zyntax program sees it: other languages' modules,
//! classes, objects and plugins as foreign objects
//! (`zyntax_embed::foreign`). Lua's `require("haxe.ScaleValues")` and
//! Python's `from haxe.ScaleValues import ScaleValues` import the world's
//! module `haxe:ScaleValues`; what the program then reads, writes and
//! calls on it goes through the bridge.
//!
//! A foreign object's word is a [`Held`]: an object of the core, rooted
//! by a handle while the program holds its box, or one of the registry's
//! modules, classes and functions, or a method as a value. None, booleans,
//! numbers and strings cross as the program's own values; every other
//! value of the core crosses as a foreign object.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Arc;

use caribou::bridge;
use caribou::error::{Error, Int64, Str};
use caribou::heap::{self, Handle};
use caribou::protocol::Callable;
use caribou::registry::{self, Interface, MethodIface, MethodKind};
use caribou::symbol::Symbol;
use caribou::world::LANG_CORE;
use caribou_abi::{ErrorKind, LangId, Value};
use zyntax_embed::foreign::{self, Any, Foreign, ForeignError};

/// What a foreign object's word points at.
enum Held {
    /// An object of the core.
    Object(Handle),
    /// A module: its classes and functions are its members.
    Module(Arc<Interface>),
    /// A class: its statics and static methods are its members, and
    /// calling it constructs one.
    Class(Arc<Interface>, usize),
    /// A module's function, or a class's static method.
    Function(MethodIface),
    /// A method as a value: called with its receiver first, as a Lua
    /// method is.
    Method(Symbol),
}

impl Held {
    fn value(&self) -> Option<Value> {
        match self {
            Held::Object(h) => Some(Value::object(heap::handle_get(*h) as *const c_void)),
            Held::Class(iface, i) => {
                let object = iface.classes[*i].class_object;
                (!object.is_null()).then_some(object)
            }
            _ => None,
        }
    }

    fn describe(&self) -> String {
        match self {
            Held::Object(_) => self.value().map_or_else(String::new, bridge::describe),
            Held::Module(iface) => format!("module {}", iface.module),
            Held::Class(iface, i) => format!("class {}", iface.classes[*i].name),
            Held::Function(m) => format!("function {}", m.name),
            Held::Method(name) => format!("method {}", name.name()),
        }
    }
}

thread_local! {
    /// The Zyntax language whose code is running, for the bridge's traces.
    static CALLER: Cell<LangId> = const { Cell::new(LANG_CORE) };
}

/// Run `f` as code of `lang`: what calls out of it name as their caller.
/// The caller before is back once `f` returns or a throw leaves it.
pub(crate) fn as_caller<T>(lang: LangId, f: impl FnOnce() -> T) -> T {
    struct Restore(LangId);
    impl Drop for Restore {
        fn drop(&mut self) {
            CALLER.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(CALLER.with(|c| c.replace(lang)));
    f()
}

fn caller() -> LangId {
    CALLER.with(Cell::get)
}

fn hold(held: Held) -> Any {
    foreign::boxed(Box::into_raw(Box::new(held)) as usize)
}

/// # Safety
/// `word` is a live foreign object's.
unsafe fn held<'a>(word: usize) -> &'a Held {
    unsafe { &*(word as *const Held) }
}

/// A value of the core as the program's.
pub(crate) fn any_of(v: Value) -> Any {
    if v.is_null() || v.is_undefined() {
        return foreign::none();
    }
    if let Some(b) = v.as_bool() {
        return foreign::boolean(b);
    }
    if let Some(i) = v.as_int() {
        return foreign::int(i as i64);
    }
    if let Some(n) = v.as_number() {
        return foreign::float(n);
    }
    if let Some(text) = unsafe { Str::text(v) } {
        return foreign::string(text);
    }
    if let Some(n) = Int64::of(v) {
        return foreign::int(n);
    }
    match v.as_object() {
        Some(p) => hold(Held::Object(heap::handle_new(p as *mut u8))),
        None => foreign::none(),
    }
}

/// The program's value as a value of the core, and the object made for
/// it, which the caller keeps where the collector sees it.
///
/// # Safety
/// `any` is null or a live box.
pub(crate) unsafe fn value_of(any: Any) -> Result<(Value, *mut u8), ForeignError> {
    use zyntax_embed::foreign::Value as V;
    let made = |v: Value| {
        (
            v,
            v.as_object().map_or(std::ptr::null_mut(), |p| p as *mut u8),
        )
    };
    Ok(match unsafe { foreign::read(any) } {
        V::None => (Value::null(), std::ptr::null_mut()),
        V::Bool(b) => (Value::bool(b), std::ptr::null_mut()),
        V::Int(i) => made(Int64::value(i)),
        V::Float(f) => (Value::number(f), std::ptr::null_mut()),
        V::Str(s) => made(Str::value(Str::new(s))),
        V::Foreign(word) => {
            let held = unsafe { held(word) };
            match held.value() {
                Some(v) => (v, std::ptr::null_mut()),
                None => {
                    return Err(ForeignError::new(
                        "TypeError",
                        format!("{} cannot be passed", held.describe()),
                    ));
                }
            }
        }
        V::Other(_) => {
            return Err(ForeignError::new(
                "TypeError",
                "a value of the program's own cannot be passed yet",
            ));
        }
    })
}

/// The widest call the program makes of the world.
const WIDEST: usize = 16;

/// `f` over `args` as values of the core. What conversion made stays on
/// this frame, where the conservative scan sees it, until `f` returns.
fn with_values<T>(
    args: &[Any],
    f: impl FnOnce(&[Value]) -> Result<T, ForeignError>,
) -> Result<T, ForeignError> {
    if args.len() > WIDEST {
        return Err(ForeignError::new(
            "TypeError",
            format!("a call of more than {WIDEST} arguments"),
        ));
    }
    let mut values = [Value::null(); WIDEST];
    let mut keep = [std::ptr::null_mut::<u8>(); WIDEST];
    for (i, &a) in args.iter().enumerate() {
        let (v, made) = unsafe { value_of(a)? };
        values[i] = v;
        keep[i] = made;
    }
    let out = f(&values[..args.len()]);
    std::hint::black_box(&keep);
    out
}

/// An error the bridge returned, as the library raises it.
fn error_of(err: Value) -> ForeignError {
    match unsafe { Error::from_value(err) } {
        Some(e) => {
            let kind = match unsafe { Error::kind(e) } {
                ErrorKind::Type => "TypeError",
                ErrorKind::Index => "IndexError",
                ErrorKind::NullAccess => "AttributeError",
                _ => "RuntimeError",
            };
            ForeignError::new(kind, unsafe { Error::message_str(e) })
        }
        None => match unsafe { Str::text(err) } {
            Some(text) => ForeignError::new("RuntimeError", text),
            None => ForeignError::new("RuntimeError", bridge::describe(err)),
        },
    }
}

fn no_member(held: &Held, name: &str) -> ForeignError {
    ForeignError::new(
        "AttributeError",
        format!("{} has no member '{name}'", held.describe()),
    )
}

fn result(r: Result<Value, Value>) -> Result<Any, ForeignError> {
    r.map(any_of).map_err(error_of)
}

fn call_target(target: &Callable, args: &[Any]) -> Result<Any, ForeignError> {
    with_values(args, |values| {
        result(bridge::call(*target, values, caller()))
    })
}

/// The method `name` of `v`'s published class, rather than a field or a
/// getter: its target takes the receiver first.
fn method_of(v: Value, name: &str) -> Option<Callable> {
    let lang = bridge::language_of(v)?;
    let (iface, i) = registry::class_for_type(lang, &bridge::type_name(v)?)?;
    iface.classes[i]
        .methods
        .iter()
        .find(|m| !m.is_static && m.name == name && m.kind() == MethodKind::Method)
        .map(|m| m.target)
}

/// Call the method `name` of `receiver`: its class's target when the
/// class publishes it, else the receiver's protocol.
fn send(receiver: Value, name: &str, args: &[Any]) -> Result<Any, ForeignError> {
    let Some(target) = method_of(receiver, name) else {
        return with_values(args, |values| {
            result(bridge::invoke(
                receiver,
                Symbol::intern(name),
                values,
                caller(),
            ))
        });
    };
    with_values(args, |values| {
        let mut all = [Value::null(); WIDEST + 1];
        all[0] = receiver;
        all[1..=values.len()].copy_from_slice(values);
        result(bridge::call(target, &all[..=values.len()], caller()))
    })
}

/// A member of a module: a class or a function.
fn module_member(iface: &Arc<Interface>, name: &str) -> Option<Any> {
    if let Some(i) = iface.classes.iter().position(|c| c.name == name) {
        return Some(hold(Held::Class(Arc::clone(iface), i)));
    }
    iface
        .functions
        .iter()
        .find(|f| f.name == name)
        .map(|f| hold(Held::Function(f.clone())))
}

/// The world's modules, classes and objects for the Zyntax languages.
pub(crate) struct World;

impl Foreign for World {
    fn get(&self, word: usize, name: &str) -> Result<Any, ForeignError> {
        let held = unsafe { held(word) };
        match held {
            Held::Module(iface) => module_member(iface, name).ok_or_else(|| no_member(held, name)),
            Held::Class(iface, i) => {
                let class = &iface.classes[*i];
                if let Some(m) = class.methods.iter().find(|m| m.name == name) {
                    return Ok(if m.is_static {
                        hold(Held::Function(m.clone()))
                    } else {
                        hold(Held::Method(Symbol::intern(name)))
                    });
                }
                match held.value() {
                    Some(object) => result(bridge::get(object, Symbol::intern(name), caller())),
                    None => Err(no_member(held, name)),
                }
            }
            Held::Object(_) => {
                let v = held.value().expect("an object");
                if method_of(v, name).is_some() {
                    return Ok(hold(Held::Method(Symbol::intern(name))));
                }
                result(bridge::get(v, Symbol::intern(name), caller()))
            }
            Held::Function(_) | Held::Method(_) => Err(no_member(held, name)),
        }
    }

    fn set(&self, word: usize, name: &str, value: Any) -> Result<(), ForeignError> {
        let held = unsafe { held(word) };
        let Some(object) = held.value() else {
            return Err(no_member(held, name));
        };
        with_values(&[value], |values| {
            bridge::set(object, Symbol::intern(name), values[0], caller()).map_err(error_of)
        })
    }

    fn call(&self, word: usize, args: &[Any]) -> Result<Any, ForeignError> {
        let held = unsafe { held(word) };
        match held {
            Held::Class(iface, i) => match &iface.classes[*i].ctor {
                Some(ctor) => call_target(&ctor.target, args),
                None => Err(ForeignError::new(
                    "TypeError",
                    format!("{} has no constructor", held.describe()),
                )),
            },
            Held::Function(f) => call_target(&f.target, args),
            Held::Method(name) => {
                let Some((&receiver, rest)) = args.split_first() else {
                    return Err(ForeignError::new(
                        "TypeError",
                        format!("{} is called with its receiver first", held.describe()),
                    ));
                };
                let name = name.name();
                with_values(&[receiver], |recv| send(recv[0], name, rest))
            }
            Held::Object(_) => {
                let v = held.value().expect("an object");
                call_target(&Callable::Dynamic(v), args)
            }
            Held::Module(_) => Err(ForeignError::new(
                "TypeError",
                format!("{} is not callable", held.describe()),
            )),
        }
    }

    fn invoke(&self, word: usize, name: &str, args: &[Any]) -> Result<Any, ForeignError> {
        let held = unsafe { held(word) };
        match held {
            Held::Object(_) => send(held.value().expect("an object"), name, args),
            Held::Class(iface, i) => {
                let class = &iface.classes[*i];
                if let Some(m) = class.methods.iter().find(|m| m.is_static && m.name == name) {
                    return call_target(&m.target, args);
                }
                match held.value() {
                    Some(object) => with_values(args, |values| {
                        result(bridge::invoke(
                            object,
                            Symbol::intern(name),
                            values,
                            caller(),
                        ))
                    }),
                    None => Err(no_member(held, name)),
                }
            }
            // A module's function, or its class constructed.
            Held::Module(iface) => {
                if let Some(f) = iface.functions.iter().find(|f| f.name == name) {
                    return call_target(&f.target, args);
                }
                match iface.classes.iter().find(|c| c.name == name) {
                    Some(class) => match &class.ctor {
                        Some(ctor) => call_target(&ctor.target, args),
                        None => Err(ForeignError::new(
                            "TypeError",
                            format!("class {} has no constructor", class.name),
                        )),
                    },
                    None => Err(no_member(held, name)),
                }
            }
            Held::Function(_) | Held::Method(_) => Err(no_member(held, name)),
        }
    }

    fn text(&self, word: usize) -> String {
        unsafe { held(word) }.describe()
    }

    fn type_name(&self, word: usize) -> String {
        match unsafe { held(word) } {
            held @ Held::Object(_) => held
                .value()
                .and_then(bridge::type_name)
                .unwrap_or_else(|| "object".to_owned()),
            Held::Module(_) => "module".to_owned(),
            Held::Class(..) => "class".to_owned(),
            Held::Function(_) => "function".to_owned(),
            Held::Method(_) => "method".to_owned(),
        }
    }

    fn equals(&self, a: usize, b: usize) -> bool {
        match unsafe { (held(a), held(b)) } {
            (Held::Object(_), Held::Object(_)) => unsafe { held(a).value() == held(b).value() },
            (Held::Module(x), Held::Module(y)) => x.lang == y.lang && x.module == y.module,
            (Held::Class(x, i), Held::Class(y, j)) => {
                x.lang == y.lang && x.module == y.module && i == j
            }
            (Held::Method(x), Held::Method(y)) => x == y,
            _ => a == b,
        }
    }

    fn hash(&self, word: usize) -> i64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match unsafe { held(word) } {
            held @ Held::Object(_) => held.value().map(Value::to_bits).hash(&mut h),
            Held::Module(iface) => iface.module.hash(&mut h),
            Held::Class(iface, i) => (&iface.module, i).hash(&mut h),
            Held::Method(name) => name.name().hash(&mut h),
            Held::Function(_) => word.hash(&mut h),
        }
        h.finish() as i64
    }

    /// `ns.Module` is the world's `ns:Module`; a module path under the
    /// namespace keeps its dots, as Haxe's do (`haxe.pkg.Class`).
    fn import(&self, name: &str) -> Result<Option<Any>, ForeignError> {
        let Some((namespace, module)) = name.split_once('.') else {
            return Ok(None);
        };
        let found = registry::lookup_or_load(namespace, module)
            .map_err(|e| ForeignError::new("ImportError", e))?;
        Ok(found.map(|iface| hold(Held::Module(iface))))
    }

    fn release(&self, word: usize) {
        let held = unsafe { Box::from_raw(word as *mut Held) };
        if let Held::Object(h) = *held {
            heap::handle_release(h);
        }
    }
}
