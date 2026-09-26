//! What compiled Haxe calls at a linked boundary (docs/architecture/
//! linking.md): the casts between Haxe's own values and the core's, and
//! the check after a callee returns. Each cast takes the value and the
//! program's `hl_type` for the Haxe side of it, which is how a cast
//! producing a Haxe object allocates one: a compiled program has no
//! other way to name its types.

use ash_std::error::hlp_throw;
use caribou_abi::hl::{hl_type, vdynamic};
use caribou::bridge;
use caribou::error::Str;
use caribou::heap;
use caribou_abi::Value;

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
    Str::new(&unsafe { proto::string_text(s) }) as *mut u8
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
    let text = unsafe { Str::text(Value::object(s.cast())) }.unwrap_or("").to_owned();
    unsafe { proto::alloc_string_typed(t, &text) }
}

/// After a linked callee returns: what it left pending, thrown into Haxe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_haxe_raise_pending() {
    let Some(e) = bridge::take_pending() else {
        return;
    };
    let _kept = e.as_object().map(|p| heap::keep(p as *const u8));
    let thrown = proto::throwable(e);
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
            (name, if params.is_empty() { 0 } else { params.split(',').count() })
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
        assert_eq!((m.kind, m.name.as_str(), m.arity), (Kind::Static, "hypot", 2));
        let m = member_of("game:ui/hud.Hud.add(_)").unwrap();
        assert_eq!((m.module.as_str(), m.kind, m.arity), ("ui/hud", Kind::Method, 1));
        let m = member_of("game:hud.Hud.construct:new()").unwrap();
        assert_eq!((m.kind, m.name.as_str(), m.arity), (Kind::Constructor, "new", 0));
        let m = member_of("game:hud.Hud.score=(_)").unwrap();
        assert_eq!((m.kind, m.name.as_str(), m.arity), (Kind::Setter, "score", 1));
        let m = member_of("game:hud.Hud.score").unwrap();
        assert_eq!((m.kind, m.arity), (Kind::Getter, 0));
        assert!(member_of("game:hud.Hud.static:count").is_none());
    }
}
