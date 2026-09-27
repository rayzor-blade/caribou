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
use caribou_abi::Value;
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

/// A `caribou` native's member as the link rule names it: the native
/// `game:hud.Hud.add(_)` is the method `add` of arity 1 of class `Hud` in
/// module `hud` of namespace `game`. `None` for a sequence operation or a
/// static field, which do not link.
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
    let (kind, sig) = if let Some(sig) = member.strip_prefix("static:") {
        if sig.ends_with("=(_)") || !sig.contains('(') {
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
        assert!(member_of("game:hud.Hud.static:count").is_none());
    }
}
