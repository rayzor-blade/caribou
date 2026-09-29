//! What compiled Haxe calls at a linked boundary (docs/architecture/
//! linking.md): the casts between Haxe's own values and the core's, and
//! the check after a callee returns. Each cast takes the value and the
//! program's `hl_type` for the Haxe side of it, which is how a cast
//! producing a Haxe object allocates one: a compiled program has no
//! other way to name its types.

use ash_std::error::hlp_throw;
use caribou::bridge;
use caribou::error::Str;
use caribou::heap;
use caribou::protocol::{Fault, Send};
use caribou_abi::Value;
use caribou_abi::data::{BufferData, EnumData};
use caribou_abi::hl::{hl_type, vdynamic};

use crate::proto;

/// A Haxe `String` as a core string: what a plugin's `Text` is.
///
/// # Safety
/// `s` is null or a live `String` object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_string_to_str(s: *mut vdynamic, _t: *mut hl_type) -> *mut u8 {
    if s.is_null() {
        return core::ptr::null_mut();
    }
    let _kept = heap::keep(s.cast());
    Str::new(&unsafe { proto::string_text(s) }) as *mut u8
}

/// The text of a Haxe `String`, for another language's cast.
///
/// # Safety
/// `s` is a live `String` object.
pub unsafe fn string_text(s: *mut vdynamic) -> String {
    unsafe { proto::string_text(s) }
}

/// The name of the program's class type `t`, `math.Vec2`: what a cast
/// that makes another language's object for a face knows its class by.
///
/// # Safety
/// `t` is null or one of the program's types.
pub unsafe fn type_name(t: *const hl_type) -> Option<String> {
    unsafe { proto::obj_name(t) }
}

/// The running program's type `t` as a build describes it
/// (`program::describe_program`): a function type with its parameters and
/// result.
///
/// # Safety
/// `t` is null or one of the program's types.
pub unsafe fn type_ref(t: *const hl_type) -> caribou::registry::TypeRef {
    use caribou::registry::TypeRef;
    use caribou_abi::hl;
    let Some(ty) = (unsafe { t.as_ref() }) else {
        return TypeRef::Dyn;
    };
    match ty.kind {
        hl::HVOID => TypeRef::Void,
        hl::HUI8 | hl::HUI16 | hl::HI32 => TypeRef::Int,
        hl::HI64 => TypeRef::Int64,
        hl::HF32 | hl::HF64 => TypeRef::Float,
        hl::HBOOL => TypeRef::Bool,
        // As the build describes it: one it cannot spell is any function.
        hl::HFUN => match unsafe { ty.detail.fun.as_ref() } {
            Some(fun)
                if (0..fun.nargs.max(0) as usize)
                    .map(|i| unsafe { *fun.args.add(i) })
                    .chain([fun.ret])
                    .all(|a| {
                        unsafe { a.as_ref() }
                            .is_some_and(|a| !matches!(a.kind, hl::HUI8 | hl::HUI16))
                    }) =>
            {
                TypeRef::Function {
                    params: (0..fun.nargs.max(0) as usize)
                        .map(|i| unsafe { type_ref(*fun.args.add(i)) })
                        .collect(),
                    ret: Box::new(unsafe { type_ref(fun.ret) }),
                }
            }
            _ => TypeRef::Fun,
        },
        hl::HMETHOD => TypeRef::Fun,
        hl::HARRAY => TypeRef::Array(Box::new(TypeRef::Dyn)),
        hl::HNULL => unsafe { type_ref(ty.detail.tparam) },
        hl::HOBJ | hl::HSTRUCT => match unsafe { proto::obj_name(t) } {
            Some(name) if name == "String" => TypeRef::Str,
            Some(name) => TypeRef::Object(name),
            None => TypeRef::Dyn,
        },
        _ => TypeRef::Dyn,
    }
}

/// A type spelled as Haxe spells it, `(Float, String)->Bool` for a function
/// type: how a build names a function type to Ash and to another
/// language's module for it, the same from the bytecode and at run time.
/// `None` for a type Haxe code cannot spell, which has no such module.
pub fn spell(ty: &caribou::registry::TypeRef) -> Option<String> {
    use caribou::registry::TypeRef;
    Some(match ty {
        TypeRef::Void => "Void".to_owned(),
        TypeRef::Int => "Int".to_owned(),
        TypeRef::Float => "Float".to_owned(),
        TypeRef::Bool => "Bool".to_owned(),
        TypeRef::Str => "String".to_owned(),
        TypeRef::Int64 => "haxe.Int64".to_owned(),
        TypeRef::Dyn => "Dynamic".to_owned(),
        TypeRef::Object(name) => name.clone(),
        TypeRef::Function { params, ret } => {
            let params: Option<Vec<String>> = params.iter().map(spell).collect();
            format!("({})->{}", params?.join(", "), spell(ret)?)
        }
        _ => return None,
    })
}

/// A Haxe `String` of the program's type `t` holding `text`, for another
/// language's cast. Unrooted, like every fresh Haxe object.
///
/// # Safety
/// `t` is the program's `String` type.
pub unsafe fn string(t: *mut hl_type, text: &str) -> *mut vdynamic {
    unsafe { proto::alloc_string_typed(t, text) }
}

/// The core object a Haxe face stands for, for another language's cast;
/// `None` for null or a face whose constructor has not bound it.
///
/// # Safety
/// `face` is null or a live Haxe object.
pub unsafe fn behind(face: *mut vdynamic) -> Option<Value> {
    unsafe { crate::import::behind(face) }.ok()
}

/// Bind `face`, which the program just constructed, to the core object
/// `v`: what a linked constructor's `init` does.
///
/// # Safety
/// `face` is a live instance of a face class.
pub unsafe fn bind(face: *mut vdynamic, v: Value) {
    let _face_kept = (!face.is_null()).then(|| heap::keep(face.cast()));
    let _value_kept = heap::keep_value(v);
    unsafe { crate::import::bind_face(face, crate::wrenref::wrap_foreign(v)) }
}

/// Bind a face directly to `v`, with the face as `v`'s weak shadow. This is
/// for an AOT face class whose allocation carries a host drop policy.
///
/// # Safety
/// `face` is a live instance of that face class and `v` is its live core
/// object.
pub unsafe fn bind_attachment(face: *mut vdynamic, v: Value) {
    let Some(object) = v.as_object().map(|p| p as *mut u8) else {
        return;
    };
    unsafe { crate::import::set_direct_attachment(face, object) };
    let mut kept = core::ptr::null_mut();
    let result = unsafe { Send::keep_shadow(object, face.cast(), &mut kept) };
    debug_assert!(
        result.is_ok() || matches!(result, Err(Fault::Missing)) && kept == face.cast(),
        "a freshly constructed object already has another Haxe face"
    );
}

/// The directly attached face of type `t` for `v`, allocating and binding it
/// when the weak edge is empty.
///
/// # Safety
/// `v` is null or a live core object and `t` is the program's corresponding
/// face class with the attachment drop policy.
pub unsafe fn attachment_face(v: Value, t: *mut hl_type) -> *mut vdynamic {
    let Some(object) = v.as_object().map(|p| p as *mut u8) else {
        return core::ptr::null_mut();
    };
    let _value_kept = heap::keep_value(v);
    if let Ok(shadow) = unsafe { Send::shadow(object, proto::lang()) } {
        if unsafe { caribou::cell::is_cell(shadow) } {
            if let Some(front) = caribou::cell::front(Value::object(shadow.cast())) {
                return front.cast();
            }
        } else {
            return shadow.cast();
        }
    }
    let face = unsafe { ash_std::obj::hlp_alloc_obj(t.cast()) } as *mut vdynamic;
    unsafe { bind_attachment(face, v) };
    face
}

/// The face of type `t` for the core object `v`: the one already in front
/// of it, else a new one bound to it. Null for null.
///
/// # Safety
/// `t` is the program's type of a face class.
pub unsafe fn face(v: Value, t: *mut hl_type) -> *mut vdynamic {
    if v.is_null() {
        return core::ptr::null_mut();
    }
    let _value_kept = heap::keep_value(v);
    let cell = crate::wrenref::wrap_foreign(v);
    if let Some(front) = caribou::cell::front(cell) {
        return front.cast();
    }
    let _cell_kept = heap::keep_value(cell);
    let face = unsafe { ash_std::obj::hlp_alloc_obj(t.cast()) } as *mut vdynamic;
    unsafe { crate::import::bind_face(face, cell) };
    face
}

/// The Haxe closure of the program's function type `t` for the function
/// `v` of another language (`callback.rs`). Null for null.
///
/// # Safety
/// `t` is one of the program's function types.
pub unsafe fn function(v: Value, t: *mut hl_type) -> *mut vdynamic {
    if v.is_null() {
        return core::ptr::null_mut();
    }
    // A Haxe function that went to another language comes back as itself.
    if bridge::language_of(v) == Some(proto::lang()) {
        return v.as_object().map_or(core::ptr::null_mut(), |p| p.cast());
    }
    let _value_kept = heap::keep_value(v);
    crate::callback::function_for_typed(v, t)
}

/// Another language's value as a heap object Haxe can keep as a closure's
/// bound value: its cell. Null for null.
pub fn hold(v: Value) -> *mut core::ffi::c_void {
    if v.is_null() {
        return core::ptr::null_mut();
    }
    crate::wrenref::wrap_foreign(v)
        .as_object()
        .map_or(core::ptr::null_mut(), |p| p.cast())
}

/// The value [`hold`] kept in `bound`.
///
/// # Safety
/// `bound` is null or what `hold` gave.
pub unsafe fn held(bound: *mut core::ffi::c_void) -> Value {
    if bound.is_null() {
        return Value::null();
    }
    crate::wrenref::unwrap_foreign(Value::object(bound.cast_const()))
}

/// Whether the Haxe closure `c` is one Ash made over a value [`hold`]
/// kept: its bound value is a cell.
///
/// # Safety
/// `c` is null or a live Haxe closure.
pub unsafe fn holds(c: *mut vdynamic) -> Option<*mut core::ffi::c_void> {
    let closure = c.cast::<caribou_abi::hl::vclosure>();
    let closure = unsafe { closure.as_ref() }?;
    (closure.hasValue == 1 && unsafe { caribou::cell::is_cell(closure.value.cast()) })
        .then_some(closure.value)
}

/// A Haxe value as the core value another language holds it by: a
/// function or an object that came from one as itself, any other object
/// as the object.
///
/// # Safety
/// `d` is null or a live Haxe value.
pub unsafe fn value(d: *mut vdynamic) -> Value {
    unsafe { proto::dyn_to_value(d) }
}

/// What a Haxe exception says, read without running Haxe code.
///
/// # Safety
/// `exc` is null or a live Haxe value.
pub unsafe fn exception_text(exc: *mut vdynamic) -> String {
    unsafe { proto::exception_text(exc) }
}

/// Raise `message` into the Haxe code that made a linked call, as the
/// error of the callee's language `origin`.
pub fn raise(message: &str, origin: caribou_abi::LangId) {
    {
        let e = caribou::error::Error::new(caribou_abi::ErrorKind::Runtime, message, origin);
        let _kept = heap::keep(e.cast());
        // Rooted by the pending slot from here.
        bridge::set_pending(caribou::error::Error::value(e));
    }
    unsafe { caribou_haxe_raise_pending() };
}

/// A core string as a Haxe `String` of the program's type `t`.
///
/// # Safety
/// `s` is null or a live core string; `t` is the program's `String` type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_str_to_string(s: *mut u8, t: *mut hl_type) -> *mut vdynamic {
    proto::set_string_type(t);
    if s.is_null() {
        return core::ptr::null_mut();
    }
    let _kept = heap::keep(s);
    let text = unsafe { Str::text(Value::object(s.cast())) }
        .unwrap_or("")
        .to_owned();
    unsafe { proto::alloc_string_typed(t, &text) }
}

/// After a linked callee returns: what it left pending, thrown into Haxe.
///
/// # Safety
/// The caller is inside a Haxe invocation with an active exception trap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_raise_pending() {
    let Some(e) = bridge::take_pending() else {
        return;
    };
    // The guard ends before the throw, which leaves this frame without
    // running its drops; nothing allocates between the two.
    let thrown = {
        let _kept = e.as_object().map(|p| heap::keep(p as *const u8));
        proto::throwable(e)
    };
    unsafe { hlp_throw(thrown.cast()) };
}

/// A Haxe `Bytes` as a core buffer over the same bytes: what a plugin's
/// `Buffer` or `BufferMut` is. `t` is the program's `haxe.io.Bytes`.
///
/// # Safety
/// `b` is null or a live `Bytes` of type `t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_bytes_to_buffer(
    b: *mut vdynamic,
    t: *mut hl_type,
) -> *mut u8 {
    if b.is_null() {
        return core::ptr::null_mut();
    }
    if unsafe { (*b).t } != t || unsafe { crate::data::learn(t) }.is_err() {
        raise("the value is not a haxe.io.Bytes", proto::lang());
    }
    let _kept = heap::keep(b.cast());
    unsafe { crate::data::buffer_from_haxe(b) }
        .as_object()
        .map_or(core::ptr::null_mut(), |p| p.cast())
}

/// A core buffer as a Haxe `Bytes` of the program's type `t`, over the
/// same bytes unless they are read-only.
///
/// # Safety
/// `p` is null or a live core buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_buffer_to_bytes(
    p: *mut BufferData,
    t: *mut hl_type,
) -> *mut vdynamic {
    if p.is_null() || unsafe { crate::data::learn(t) }.is_err() {
        return core::ptr::null_mut();
    }
    let _kept = heap::keep(p.cast());
    unsafe { crate::data::buffer_to_haxe(p) }.unwrap_or(core::ptr::null_mut())
}

/// A Haxe `Float` as a plugin's `f32`.
#[unsafe(no_mangle)]
pub extern "C" fn caribou_haxe_f64_to_f32(v: f64, _t: *mut hl_type) -> f32 {
    v as f32
}

/// A plugin's `f32` as a Haxe `Float`.
#[unsafe(no_mangle)]
pub extern "C" fn caribou_haxe_f32_to_f64(v: f32, _t: *mut hl_type) -> f64 {
    f64::from(v)
}

/// A Haxe dynamic as a core value: what a plugin's `Value` is.
///
/// # Safety
/// `d` is null or a live Haxe value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_dyn_to_value(d: *mut vdynamic, _t: *mut hl_type) -> u64 {
    unsafe { proto::dyn_to_value(d) }.to_bits()
}

/// A core value as a Haxe dynamic.
///
/// # Safety
/// `v` is the bit representation of a valid live core value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_value_to_dyn(v: u64, _t: *mut hl_type) -> *mut vdynamic {
    let v = Value::from_bits(v);
    let _kept = heap::keep_value(v);
    unsafe { proto::value_to_dyn(v, caribou_abi::hl::HDYN) }.unwrap_or(core::ptr::null_mut())
}

/// A plugin's future as the Haxe dynamic a native returns it as: a
/// `caribou.Future` face.
///
/// # Safety
/// `p` is null or a live core future.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_future_to_haxe(
    p: *mut u8,
    _t: *mut hl_type,
) -> *mut vdynamic {
    if p.is_null() {
        return core::ptr::null_mut();
    }
    unsafe { caribou_haxe_value_to_dyn(Value::object(p.cast()).to_bits(), _t) }
}

/// A `caribou.Future` a plugin takes, as the core future behind it.
///
/// # Safety
/// `d` is null or a live Haxe value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_future_from_haxe(
    d: *mut vdynamic,
    _t: *mut hl_type,
) -> *mut u8 {
    if d.is_null() {
        return core::ptr::null_mut();
    }
    let v = unsafe { proto::dyn_to_value(d) };
    match caribou::future::of(v) {
        Some(future) => future.cast(),
        None => {
            raise("the value is not a caribou.Future", proto::lang());
            core::ptr::null_mut()
        }
    }
}

// The caribou library's own natives, which `caribou.Future` and
// `caribou.Sequence` declare, as a compiled program links them: the
// operations a hosted run's natives run, over Haxe's words. What one
// raises is left pending for the check after the call.

/// Run the operation `name` on `receiver` with `args`, both kept.
unsafe fn library(name: &str, receiver: *mut vdynamic, args: &[Value]) -> Value {
    let _receiver_kept = (!receiver.is_null()).then(|| heap::keep(receiver.cast()));
    let _args_kept: [Option<heap::Kept>; 2] =
        std::array::from_fn(|i| args.get(i).and_then(|&a| heap::keep_value(a)));
    match unsafe { crate::import::library_operation(name, receiver, args) } {
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            bridge::set_pending(e);
            Value::null()
        }
        None => unreachable!("`{name}` is one of the library's operations"),
    }
}

/// A result as the Haxe dynamic the native returns.
fn dynamic(v: Value) -> *mut vdynamic {
    let _kept = heap::keep_value(v);
    unsafe { proto::value_to_dyn(v, caribou_abi::hl::HDYN) }.unwrap_or(core::ptr::null_mut())
}

/// A Haxe dynamic argument as a core value, converted while `receiver`
/// is kept.
unsafe fn argument(receiver: *mut vdynamic, d: *mut vdynamic) -> Value {
    let _receiver_kept = (!receiver.is_null()).then(|| heap::keep(receiver.cast()));
    unsafe { proto::dyn_to_value(d) }
}

/// A face class naming its type as the program starts: `namespace`,
/// `module` and `class` as its natives name it, each a core string.
///
/// # Safety
/// The names are live core strings; `t` is the class's type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_face(
    namespace: *mut u8,
    module: *mut u8,
    class: *mut u8,
    t: *mut hl_type,
) {
    let text = |p: *mut u8| {
        (!p.is_null())
            .then(|| unsafe { Str::text(Value::object(p.cast())) })
            .flatten()
            .unwrap_or("")
            .to_owned()
    };
    crate::import::register_face(&text(namespace), &text(module), &text(class), t);
}

/// # Safety
/// `face` is the fresh `caribou.Future`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_future_new(face: *mut vdynamic) {
    unsafe { library("future_new", face, &[]) };
}

/// # Safety
/// `future` is a live Haxe value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_future_ready(future: *mut vdynamic) -> bool {
    unsafe { library("future_ready", future, &[]) }
        .as_bool()
        .unwrap_or(false)
}

/// # Safety
/// `future` is a live Haxe value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_future_await(future: *mut vdynamic) -> *mut vdynamic {
    dynamic(unsafe { library("future_await", future, &[]) })
}

/// # Safety
/// `future` and `value` are live Haxe values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_future_resolve(
    future: *mut vdynamic,
    value: *mut vdynamic,
) -> bool {
    let value = unsafe { argument(future, value) };
    unsafe { library("future_resolve", future, &[value]) }
        .as_bool()
        .unwrap_or(false)
}

/// # Safety
/// `future` and `error` are live Haxe values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_future_reject(
    future: *mut vdynamic,
    error: *mut vdynamic,
) -> bool {
    let error = unsafe { argument(future, error) };
    unsafe { library("future_reject", future, &[error]) }
        .as_bool()
        .unwrap_or(false)
}

/// # Safety
/// `seq` is a live Haxe value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_len(seq: *mut vdynamic) -> i32 {
    unsafe { library("len", seq, &[]) }.as_int().unwrap_or(0)
}

/// # Safety
/// `seq` is a live Haxe value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_index(seq: *mut vdynamic, i: i32) -> *mut vdynamic {
    dynamic(unsafe { library("index", seq, &[Value::int(i)]) })
}

/// # Safety
/// `seq` and `value` are live Haxe values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_set_index(seq: *mut vdynamic, i: i32, value: *mut vdynamic) {
    let value = unsafe { argument(seq, value) };
    unsafe { library("set_index", seq, &[Value::int(i), value]) };
}

/// A Haxe enum value of the program's enum type `t` as the core's, once
/// the enum it declares is registered; null when it is not.
///
/// # Safety
/// `e` is null or a live value of type `t`.
pub unsafe fn enum_to_core(e: *mut vdynamic, t: *mut hl_type) -> *mut u8 {
    if e.is_null() {
        return core::ptr::null_mut();
    }
    if unsafe { (*e).t } != t
        || unsafe { crate::data::learn(t) }.is_err()
        || !crate::data::is_enum(t)
    {
        raise(
            "the value is not a value of the declared enum",
            proto::lang(),
        );
    }
    unsafe { crate::data::enum_from_haxe(e) }
        .as_object()
        .map_or(core::ptr::null_mut(), |p| p.cast())
}

/// A core enum value as a Haxe one of the program's enum type `t`.
///
/// # Safety
/// `p` is null or a live core enum value.
pub unsafe fn core_to_enum(p: *mut EnumData, t: *mut hl_type) -> *mut vdynamic {
    if p.is_null() || unsafe { crate::data::learn(t) }.is_err() {
        return core::ptr::null_mut();
    }
    unsafe { crate::data::enum_to_haxe(p) }.unwrap_or(core::ptr::null_mut())
}

/// The name of the program's enum type `t`, `math.Event`.
///
/// # Safety
/// `t` is null or one of the program's types.
pub unsafe fn enum_name(t: *const hl_type) -> Option<String> {
    let t = unsafe { t.as_ref() }?;
    (t.kind == caribou_abi::hl::HENUM).then(|| unsafe { crate::data::enum_type_name(t) })
}

/// A `caribou` native's member as the link rule names it: the native
/// `game:hud.Hud.add(_)` is the method `add` of arity 1 of class `Hud` in
/// module `hud` of namespace `game`. `None` for a sequence operation or a
/// static setter, which do not link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub namespace: String,
    pub module: String,
    pub class: String,
    pub kind: caribou::link::Kind,
    pub name: String,
    pub arity: usize,
}

pub fn member_of(native: &str) -> Option<Member> {
    use caribou::link::Kind;
    let (namespace, module, class, member) = crate::import::parse(native)?;
    // A static getter is a static of no arguments, as a compiled module
    // exports it; a static setter does not link.
    let (kind, sig) = if let Some(sig) = member.strip_prefix("static:") {
        if sig.ends_with("=(_)") {
            return None;
        }
        (Kind::Static, sig)
    } else if let Some(sig) = member.strip_prefix("construct:") {
        (Kind::Constructor, sig)
    } else if let Some(field) = member.strip_suffix("=(_)") {
        (Kind::Setter, field)
    } else if member.contains('(') {
        (Kind::Method, member.as_str())
    } else {
        (Kind::Getter, member.as_str())
    };
    let (name, arity) = match sig.split_once('(') {
        Some((name, params)) => {
            let params = params.trim_end_matches(')');
            (
                name,
                if params.is_empty() {
                    0
                } else {
                    params.split(',').count()
                },
            )
        }
        None if kind == Kind::Setter => (sig, 1),
        None => (sig, 0),
    };
    Some(Member {
        namespace,
        module,
        class,
        kind,
        name: name.to_owned(),
        arity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use caribou::link::Kind;

    #[test]
    fn a_native_names_its_member_as_the_link_rule_does() {
        let m = member_of("math:Math.Math.static:hypot(_,_)").unwrap();
        assert_eq!(
            (m.namespace.as_str(), m.module.as_str(), m.class.as_str()),
            ("math", "Math", "Math")
        );
        assert_eq!(
            (m.kind, m.name.as_str(), m.arity),
            (Kind::Static, "hypot", 2)
        );
        let m = member_of("game:ui/hud.Hud.add(_)").unwrap();
        assert_eq!(
            (m.module.as_str(), m.kind, m.arity),
            ("ui/hud", Kind::Method, 1)
        );
        let m = member_of("game:hud.Hud.construct:new()").unwrap();
        assert_eq!(
            (m.kind, m.name.as_str(), m.arity),
            (Kind::Constructor, "new", 0)
        );
        let m = member_of("game:hud.Hud.score=(_)").unwrap();
        assert_eq!(
            (m.kind, m.name.as_str(), m.arity),
            (Kind::Setter, "score", 1)
        );
        let m = member_of("game:hud.Hud.score").unwrap();
        assert_eq!((m.kind, m.arity), (Kind::Getter, 0));
        let m = member_of("game:hud.Hud.static:count").unwrap();
        assert_eq!(
            (m.kind, m.name.as_str(), m.arity),
            (Kind::Static, "count", 0)
        );
        assert!(member_of("game:hud.Hud.static:count=(_)").is_none());
    }
}
