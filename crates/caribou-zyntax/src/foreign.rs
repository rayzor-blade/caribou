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
use std::sync::{Arc, RwLock};

use caribou::bridge;
use caribou::error::{Error, Int64, Str};
use caribou::heap::{self, Handle};
use caribou::protocol::{CallSite, Callable};
use caribou::registry::{self, Interface, MethodIface, MethodKind};
use caribou::symbol::Symbol;
use caribou::world::LANG_CORE;
use caribou_abi::{ErrorKind, LangId, Value};
use zyntax_embed::foreign::{self, Any, Foreign, ForeignError};

/// What a foreign object's word points at.
enum Held {
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
            Held::Class(iface, i) => {
                let object = iface.classes[*i].class_object;
                (!object.is_null()).then_some(object)
            }
            _ => None,
        }
    }

    fn describe(&self) -> String {
        match self {
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

struct HostSlot {
    name: Symbol,
    get: CallSite,
    set: CallSite,
    invoke: CallSite,
    target: Option<Callable>,
}

/// A process-local schema key carrying the already interned member, its call
/// sites, and an optional published target. Zyntax treats this as opaque data.
pub fn host_key(name: &str, target: Option<Callable>) -> u64 {
    Box::into_raw(Box::new(HostSlot {
        name: Symbol::intern(name),
        get: CallSite::new(),
        set: CallSite::new(),
        invoke: CallSite::new(),
        target,
    })) as u64
}

fn host_slot(key: u64) -> Result<&'static HostSlot, ForeignError> {
    if key == 0 {
        return Err(ForeignError::new("TypeError", "invalid host member key"));
    }
    Ok(unsafe { &*(key as *const HostSlot) })
}

/// Run `f` as code of `lang`: what calls out of it name as their caller.
/// The caller before is back once `f` returns or a throw leaves it.
pub fn as_caller<T>(lang: LangId, f: impl FnOnce() -> T) -> T {
    struct Restore<'a>(&'a Cell<LangId>, LangId);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            self.0.set(self.1);
        }
    }
    CALLER.with(|caller| {
        let _restore = Restore(caller, caller.replace(lang));
        f()
    })
}

/// The Zyntax language whose code is running now.
pub fn caller() -> LangId {
    CALLER.with(Cell::get)
}

/// Object words use their low alignment bit to carry a core handle directly.
/// Metadata values remain aligned pointers to [`Held`].
const OBJECT_WORD: usize = 1;

fn hold(held: Held) -> Any {
    let word = Box::into_raw(Box::new(held)) as usize;
    debug_assert_eq!(word & OBJECT_WORD, 0);
    foreign::boxed(word)
}

fn root_object(object: *mut u8) -> usize {
    let handle = heap::handle_new(object);
    ((handle.as_raw() as usize) << 1) | OBJECT_WORD
}

fn hold_object(object: *mut u8) -> Any {
    foreign::boxed(root_object(object))
}

fn object_handle(word: usize) -> Option<Handle> {
    (word & OBJECT_WORD != 0).then(|| Handle::from_raw((word >> 1) as u32))
}

fn direct_object(word: usize) -> Option<Value> {
    object_handle(word).map(|handle| Value::object(heap::handle_get(handle) as *const c_void))
}

fn object_of(word: usize) -> Option<Value> {
    direct_object(word).or_else(|| unsafe { held(word) }.value())
}

/// # Safety
/// `word` is a live foreign object's.
unsafe fn held<'a>(word: usize) -> &'a Held {
    unsafe { &*(word as *const Held) }
}

/// How a language's own values cross, for a language whose values other
/// languages hold as objects of the core (a Lua table or function).
pub trait Crossing: Send + Sync {
    /// A value of the program's own that this layer has no reading for,
    /// as a value of the core; `None` for one that is not the language's.
    fn value_of(&self, any: Any) -> Option<Value>;

    /// The program's own value that `v` stands for, when it stands for
    /// one: what crossed out comes back as itself.
    fn own(&self, v: Value) -> Option<Any>;
}

static CROSSINGS: RwLock<Vec<&'static dyn Crossing>> = RwLock::new(Vec::new());

/// Cross a language's own values with `crossing`, for the process.
pub fn add_crossing(crossing: &'static dyn Crossing) {
    CROSSINGS.write().unwrap().push(crossing);
}

/// A value of the core as the program's.
pub fn any_of(v: Value) -> Any {
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
    if let Some(own) = CROSSINGS.read().unwrap().iter().find_map(|c| c.own(v)) {
        return own;
    }
    if let Some(f) = crate::object::function_of(v) {
        return f;
    }
    match v.as_object() {
        Some(p) => hold_object(p as *mut u8),
        None => foreign::none(),
    }
}

/// The program's value as a value of the core, and the object made for
/// it, which the caller keeps where the collector sees it.
///
/// # Safety
/// `any` is null or a live box.
pub unsafe fn value_of(any: Any) -> Result<(Value, *mut u8), ForeignError> {
    unsafe { value_from(any, None) }
}

/// [`value_of`] for a value `origin`'s program made, which a function
/// value keeps to describe what its calls raise; without one, the
/// running language's last module.
///
/// # Safety
/// `any` is null or a live box.
pub unsafe fn value_from(
    any: Any,
    origin: Option<&crate::object::Origin>,
) -> Result<(Value, *mut u8), ForeignError> {
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
        // Several values as one: a tuple of the core, each named by its
        // place.
        V::Tuple(items) => made(unsafe { tuple_of(items)? }),
        V::Foreign(word) => {
            if let Some(value) = direct_object(word) {
                return Ok((value, std::ptr::null_mut()));
            }
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
            let crossed = CROSSINGS
                .read()
                .unwrap()
                .iter()
                .find_map(|c| c.value_of(any));
            let origin = origin.copied().or_else(|| crate::origin_of(caller()));
            match (crossed, origin) {
                (Some(v), _) => made(v),
                (None, Some(origin)) if unsafe { foreign::is_function(any) } => {
                    made(crate::object::function(any, origin))
                }
                (None, _) => {
                    return Err(ForeignError::new(
                        "TypeError",
                        "a value of the program's own cannot be passed yet",
                    ));
                }
            }
        }
    })
}

/// The program's tuple `items` as a tuple of the core. Each value is
/// rooted until the tuple holds it: making the next may collect.
unsafe fn tuple_of(items: &[Any]) -> Result<Value, ForeignError> {
    let mut values = Vec::with_capacity(items.len());
    let mut handles = Vec::new();
    let mut failed = None;
    for &item in items {
        match unsafe { value_of(item) } {
            Ok((v, _)) => {
                if let Some(p) = v.as_object().filter(|p| !p.is_null()) {
                    handles.push(heap::handle_new(p as *mut u8));
                }
                values.push(v);
            }
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    let out = match failed {
        None => Ok(Value::object(
            caribou::data::tuple_new(caribou::data::tuple_positions(values.len()), &values).cast(),
        )),
        Some(e) => Err(e),
    };
    for h in handles {
        heap::handle_release(h);
    }
    out
}

/// The widest call the program makes of the world.
const WIDEST: usize = 16;

/// `f` over `args` as values of the core. What conversion made stays rooted
/// until `f` returns, including when this Rust frame is a wasm engine frame
/// the collector cannot scan.
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
    let mut _kept: [Option<heap::Kept>; WIDEST] = std::array::from_fn(|_| None);
    for (i, &a) in args.iter().enumerate() {
        let (v, made) = unsafe { value_of(a)? };
        values[i] = v;
        if !made.is_null() {
            _kept[i] = Some(heap::keep(made));
        }
    }
    f(&values[..args.len()])
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

fn object_result(r: Result<Value, Value>) -> Result<Any, ForeignError> {
    object_word_result(r).map(foreign::boxed)
}

fn object_word_result(r: Result<Value, Value>) -> Result<usize, ForeignError> {
    let value = r.map_err(error_of)?;
    let object = value
        .as_object()
        .filter(|object| !object.is_null())
        .ok_or_else(|| ForeignError::new("TypeError", "a host constructor returned no object"))?;
    Ok(root_object(object as *mut u8))
}

fn float_result(r: Result<Value, Value>) -> Result<f64, ForeignError> {
    let value = r.map_err(error_of)?;
    value
        .as_number()
        .or_else(|| value.as_int().map(|n| n as f64))
        .ok_or_else(|| ForeignError::new("TypeError", "a numeric host result was expected"))
}

fn with_float_values<T>(
    args: &[f64],
    f: impl FnOnce(&[Value]) -> Result<T, ForeignError>,
) -> Result<T, ForeignError> {
    if args.len() > WIDEST {
        return Err(ForeignError::new(
            "TypeError",
            format!("a call of more than {WIDEST} arguments"),
        ));
    }
    let mut values = [Value::null(); WIDEST];
    for (value, &number) in values.iter_mut().zip(args) {
        *value = Value::number(number);
    }
    f(&values[..args.len()])
}

fn call_target(target: &Callable, args: &[Any]) -> Result<Any, ForeignError> {
    with_values(args, |values| {
        result(bridge::call(*target, values, caller()))
    })
}

fn call_float_target(target: &Callable, args: &[f64]) -> Result<f64, ForeignError> {
    with_float_values(args, |values| {
        float_result(bridge::call(*target, values, caller()))
    })
}

fn construct_target(target: &Callable, args: &[Any]) -> Result<Any, ForeignError> {
    with_values(args, |values| {
        object_result(bridge::call(*target, values, caller()))
    })
}

fn construct_word_target(target: &Callable, args: &[Any]) -> Result<usize, ForeignError> {
    with_values(args, |values| {
        object_word_result(bridge::call(*target, values, caller()))
    })
}

fn construct_float_target(target: &Callable, args: &[f64]) -> Result<Any, ForeignError> {
    with_float_values(args, |values| {
        object_result(bridge::call(*target, values, caller()))
    })
}

fn construct_float_word_target(target: &Callable, args: &[f64]) -> Result<usize, ForeignError> {
    with_float_values(args, |values| {
        object_word_result(bridge::call(*target, values, caller()))
    })
}

fn call_float_target_at(
    target: &Callable,
    site: &CallSite,
    name: &str,
    args: &[f64],
) -> Result<f64, ForeignError> {
    with_float_values(args, |values| {
        float_result(bridge::call_at(*target, site, values, caller(), name))
    })
}

fn call_float_method_at(
    receiver: Value,
    target: &Callable,
    site: &CallSite,
    name: &str,
    args: &[f64],
) -> Result<f64, ForeignError> {
    with_float_values(args, |values| {
        let mut all = [Value::null(); WIDEST + 1];
        all[0] = receiver;
        all[1..=values.len()].copy_from_slice(values);
        float_result(bridge::call_at(
            *target,
            site,
            &all[..=values.len()],
            caller(),
            name,
        ))
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

fn send_float(receiver: Value, name: &str, args: &[f64]) -> Result<f64, ForeignError> {
    let Some(target) = method_of(receiver, name) else {
        return with_float_values(args, |values| {
            float_result(bridge::invoke(
                receiver,
                Symbol::intern(name),
                values,
                caller(),
            ))
        });
    };
    with_float_values(args, |values| {
        let mut all = [Value::null(); WIDEST + 1];
        all[0] = receiver;
        all[1..=values.len()].copy_from_slice(values);
        float_result(bridge::call(target, &all[..=values.len()], caller()))
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
        if let Some(object) = direct_object(word) {
            if method_of(object, name).is_some() {
                return Ok(hold(Held::Method(Symbol::intern(name))));
            }
            return result(bridge::get(object, Symbol::intern(name), caller()));
        }
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
            Held::Function(_) | Held::Method(_) => Err(no_member(held, name)),
        }
    }

    fn set(&self, word: usize, name: &str, value: Any) -> Result<(), ForeignError> {
        let Some(object) = object_of(word) else {
            let held = unsafe { held(word) };
            return Err(no_member(held, name));
        };
        with_values(&[value], |values| {
            bridge::set(object, Symbol::intern(name), values[0], caller()).map_err(error_of)
        })
    }

    fn call(&self, word: usize, args: &[Any]) -> Result<Any, ForeignError> {
        if let Some(object) = direct_object(word) {
            return call_target(&Callable::Dynamic(object), args);
        }
        let held = unsafe { held(word) };
        match held {
            Held::Class(iface, i) => match &iface.classes[*i].ctor {
                Some(ctor) => construct_target(&ctor.target, args),
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
            Held::Module(_) => Err(ForeignError::new(
                "TypeError",
                format!("{} is not callable", held.describe()),
            )),
        }
    }

    fn construct(&self, word: usize, args: &[Any]) -> Result<Any, ForeignError> {
        let held = unsafe { held(word) };
        match held {
            Held::Class(iface, i) => match &iface.classes[*i].ctor {
                Some(ctor) => construct_target(&ctor.target, args),
                None => Err(ForeignError::new(
                    "TypeError",
                    format!("{} has no constructor", held.describe()),
                )),
            },
            _ => Err(ForeignError::new(
                "TypeError",
                format!("{} is not a class", held.describe()),
            )),
        }
    }

    fn construct_word(&self, word: usize, args: &[Any]) -> Result<usize, ForeignError> {
        let held = unsafe { held(word) };
        match held {
            Held::Class(iface, i) => match &iface.classes[*i].ctor {
                Some(ctor) => construct_word_target(&ctor.target, args),
                None => Err(ForeignError::new(
                    "TypeError",
                    format!("{} has no constructor", held.describe()),
                )),
            },
            _ => Err(ForeignError::new(
                "TypeError",
                format!("{} is not a class", held.describe()),
            )),
        }
    }

    fn construct_float(&self, word: usize, args: &[f64]) -> Result<Any, ForeignError> {
        let held = unsafe { held(word) };
        match held {
            Held::Class(iface, i) => match &iface.classes[*i].ctor {
                Some(ctor) => construct_float_target(&ctor.target, args),
                None => Err(ForeignError::new(
                    "TypeError",
                    format!("{} has no constructor", held.describe()),
                )),
            },
            _ => Err(ForeignError::new(
                "TypeError",
                format!("{} is not a class", held.describe()),
            )),
        }
    }

    fn construct_float_word(&self, word: usize, args: &[f64]) -> Result<usize, ForeignError> {
        let held = unsafe { held(word) };
        match held {
            Held::Class(iface, i) => match &iface.classes[*i].ctor {
                Some(ctor) => construct_float_word_target(&ctor.target, args),
                None => Err(ForeignError::new(
                    "TypeError",
                    format!("{} has no constructor", held.describe()),
                )),
            },
            _ => Err(ForeignError::new(
                "TypeError",
                format!("{} is not a class", held.describe()),
            )),
        }
    }

    fn retain(&self, word: usize) -> Result<usize, ForeignError> {
        let Some(handle) = object_handle(word) else {
            return Err(ForeignError::new(
                "TypeError",
                "only host objects can be retained",
            ));
        };
        heap::handle_retain(handle);
        Ok(word)
    }

    fn invoke(&self, word: usize, name: &str, args: &[Any]) -> Result<Any, ForeignError> {
        if let Some(object) = direct_object(word) {
            return send(object, name, args);
        }
        let held = unsafe { held(word) };
        match held {
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
                        Some(ctor) => construct_target(&ctor.target, args),
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

    fn get_float(&self, word: usize, name: &str) -> Result<f64, ForeignError> {
        let Some(object) = object_of(word) else {
            let held = unsafe { held(word) };
            return Err(no_member(held, name));
        };
        float_result(bridge::get(object, Symbol::intern(name), caller()))
    }

    fn set_float(&self, word: usize, name: &str, value: f64) -> Result<(), ForeignError> {
        let Some(object) = object_of(word) else {
            let held = unsafe { held(word) };
            return Err(no_member(held, name));
        };
        bridge::set(object, Symbol::intern(name), Value::number(value), caller()).map_err(error_of)
    }

    fn call_float(&self, word: usize, args: &[f64]) -> Result<f64, ForeignError> {
        if let Some(object) = direct_object(word) {
            return call_float_target(&Callable::Dynamic(object), args);
        }
        let held = unsafe { held(word) };
        match held {
            Held::Function(function) => call_float_target(&function.target, args),
            _ => Err(ForeignError::new(
                "TypeError",
                format!("{} does not return a number", held.describe()),
            )),
        }
    }

    fn invoke_float(&self, word: usize, name: &str, args: &[f64]) -> Result<f64, ForeignError> {
        if let Some(object) = direct_object(word) {
            return send_float(object, name, args);
        }
        let held = unsafe { held(word) };
        match held {
            Held::Class(iface, i) => {
                let class = &iface.classes[*i];
                if let Some(method) = class
                    .methods
                    .iter()
                    .find(|method| method.is_static && method.name == name)
                {
                    return call_float_target(&method.target, args);
                }
                match held.value() {
                    Some(object) => with_float_values(args, |values| {
                        float_result(bridge::invoke(
                            object,
                            Symbol::intern(name),
                            values,
                            caller(),
                        ))
                    }),
                    None => Err(no_member(held, name)),
                }
            }
            Held::Module(iface) => match iface.functions.iter().find(|f| f.name == name) {
                Some(function) => call_float_target(&function.target, args),
                None => Err(no_member(held, name)),
            },
            Held::Function(_) | Held::Method(_) => Err(no_member(held, name)),
        }
    }

    fn get_float_key(&self, word: usize, key: u64) -> Result<f64, ForeignError> {
        let Some(object) = object_of(word) else {
            let held = unsafe { held(word) };
            return Err(ForeignError::new(
                "AttributeError",
                format!("{} has no keyed field", held.describe()),
            ));
        };
        let slot = host_slot(key)?;
        float_result(bridge::get_at(object, slot.name, &slot.get, caller()))
    }

    fn set_float_key(&self, word: usize, key: u64, value: f64) -> Result<(), ForeignError> {
        let Some(object) = object_of(word) else {
            let held = unsafe { held(word) };
            return Err(ForeignError::new(
                "AttributeError",
                format!("{} has no keyed field", held.describe()),
            ));
        };
        let slot = host_slot(key)?;
        bridge::set_at(object, slot.name, &slot.set, Value::number(value), caller())
            .map_err(error_of)
    }

    fn invoke_float_key(&self, word: usize, key: u64, args: &[f64]) -> Result<f64, ForeignError> {
        let slot = host_slot(key)?;
        if let Some(receiver) = direct_object(word) {
            let target = slot.target.ok_or_else(|| {
                ForeignError::new(
                    "AttributeError",
                    format!("object has no member '{}'", slot.name.name()),
                )
            })?;
            return call_float_method_at(receiver, &target, &slot.invoke, slot.name.name(), args);
        }
        let held = unsafe { held(word) };
        match held {
            Held::Class(..) | Held::Module(_) => match slot.target {
                Some(target) => call_float_target_at(&target, &slot.invoke, slot.name.name(), args),
                None => Err(no_member(held, slot.name.name())),
            },
            Held::Function(_) | Held::Method(_) => Err(no_member(held, slot.name.name())),
        }
    }

    fn text(&self, word: usize) -> String {
        direct_object(word).map_or_else(|| unsafe { held(word) }.describe(), bridge::describe)
    }

    fn type_name(&self, word: usize) -> String {
        if let Some(object) = direct_object(word) {
            return bridge::type_name(object).unwrap_or_else(|| "object".to_owned());
        }
        match unsafe { held(word) } {
            Held::Module(_) => "module".to_owned(),
            Held::Class(..) => "class".to_owned(),
            Held::Function(_) => "function".to_owned(),
            Held::Method(_) => "method".to_owned(),
        }
    }

    fn equals(&self, a: usize, b: usize) -> bool {
        match (direct_object(a), direct_object(b)) {
            (Some(x), Some(y)) => return x == y,
            (Some(_), None) | (None, Some(_)) => return false,
            (None, None) => {}
        }
        match unsafe { (held(a), held(b)) } {
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
        if let Some(object) = direct_object(word) {
            object.to_bits().hash(&mut h);
            return h.finish() as i64;
        }
        match unsafe { held(word) } {
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
        if let Some(handle) = object_handle(word) {
            heap::handle_release(handle);
        } else {
            unsafe { drop(Box::from_raw(word as *mut Held)) };
        }
    }

    /// A core buffer is read in place: the program holds it rooted, and
    /// the heap does not move it.
    fn bytes(&self, word: usize) -> Option<(*const u8, usize)> {
        let buffer = caribou::data::buffer_of(object_of(word)?)?;
        let b = unsafe { &*buffer };
        Some((b.bytes, b.len))
    }

    /// A core tuple is several values given at once: a call that returns
    /// one gives the program that many.
    fn values(&self, word: usize) -> Option<usize> {
        let tuple = caribou::data::tuple_of(object_of(word)?)?;
        Some(unsafe { caribou::data::tuple_values(tuple) }.len())
    }

    fn value(&self, word: usize, index: usize) -> Any {
        let tuple = object_of(word).and_then(caribou::data::tuple_of);
        match tuple.and_then(|t| unsafe { caribou::data::tuple_values(t) }.get(index)) {
            Some(&v) => any_of(v),
            None => foreign::none(),
        }
    }
}
