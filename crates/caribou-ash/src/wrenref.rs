//! A foreign object as something Haxe can hold: a cell (`caribou::cell`)
//! Haxe reads as one of its own objects.
//!
//! A cell Haxe only keeps as a pointer it never reads, a callback's
//! target or the field of a face it constructed, is made under the plain
//! view here, an abstract type. One Haxe gets as an object is read under
//! the class the program declares for the object's type, its `hl_type`
//! mirrored in the cell's descriptor (`import::face_for`), so to Haxe the
//! cell is an instance of that class: it dispatches, casts and tests the
//! type through the mirror, and the mirror shares the class's runtime
//! data. Haxe never reads a cell's other words. Whatever is not Haxe's is
//! held the same way: a core `Str` or `Error`, an object of a language
//! registered later.

use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;

use caribou::bridge;
use caribou::cell;
use caribou::heap::TypeDesc;
use caribou_abi::Value;

use crate::proto::{haxe_type, lang};

/// The plain view, for a cell Haxe holds but never reads.
pub(crate) fn plain() -> &'static TypeDesc {
    static PLAIN: OnceLock<&'static TypeDesc> = OnceLock::new();
    PLAIN.get_or_init(|| cell::descriptor(haxe_type(), lang(), "foreign object"))
}

/// `v` as something Haxe can hold: `v` itself when it is Haxe's or not an
/// object, else the one cell for its object, made on first need under
/// the plain view. A fresh cell is not rooted; store it or root it
/// before allocating.
pub fn wrap_foreign(v: Value) -> Value {
    if v.as_object().is_none() || bridge::language_of(v) == Some(lang()) {
        return v;
    }
    cell::wrap(v, plain())
}

/// The object behind `v` when `v` is a cell, else `v`.
pub fn unwrap_foreign(v: Value) -> Value {
    cell::unwrap(v)
}

/// The live cell for `v`'s object, if Haxe holds one.
pub fn foreign_ref(v: Value) -> Option<Value> {
    cell::of(v, lang())
}

/// The pointer Haxe keeps in its `hl.Abstract<"caribou_obj">` field: the
/// cell itself. Null for anything that is not a cell.
pub fn wrenref_as_abstract(v: Value) -> *mut c_void {
    if cell::descriptor_of(v).is_none() {
        return ptr::null_mut();
    }
    v.as_object().unwrap_or(ptr::null_mut())
}

/// The cell a Haxe abstract field holds, as a value; null for null.
///
/// # Safety
/// `p` must be null or a pointer `wrenref_as_abstract` gave, still held
/// by Haxe.
pub unsafe fn wrenref_from_abstract(p: *mut c_void) -> Value {
    unsafe { cell::from_pointer(p) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haxes_own_values_and_scalars_pass_through() {
        assert_eq!(
            wrap_foreign(Value::null()).to_bits(),
            Value::null().to_bits()
        );
        assert_eq!(
            wrap_foreign(Value::int(3)).to_bits(),
            Value::int(3).to_bits()
        );
        assert_eq!(
            unwrap_foreign(Value::int(3)).to_bits(),
            Value::int(3).to_bits()
        );
        assert_eq!(foreign_ref(Value::number(1.0)), None);
        assert!(wrenref_as_abstract(Value::null()).is_null());
        assert!(unsafe { wrenref_from_abstract(ptr::null_mut()) }.is_null());
    }
}
