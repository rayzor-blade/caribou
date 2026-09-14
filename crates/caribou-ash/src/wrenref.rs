//! A foreign object as something Haxe can hold: the `WrenRef`.
//!
//! A Wren object reaching Haxe must be an object Haxe can keep, that
//! Haxe's collector sees, and that stays alive in Wren for as long as
//! Haxe keeps it. A `WrenRef` is that: a traced core object under a
//! static descriptor whose field is the foreign object's bridge value.
//! Its trace hook marks the object, and the core's mark is what a Wren
//! cycle ends with, so a live ref is what keeps its object across that
//! cycle. Haxe holds the ref as a raw pointer in an
//! `hl.Abstract<"caribou_obj">` field of the class the build macro emits
//! for the Wren class: a word the conservative scan sees and HashLink
//! never reads.
//!
//! The ref's protocol forwards every message to the object it stands for,
//! so a Haxe caller reaching it through `Dynamic` gets Wren semantics. It
//! answers `unwrap_native` with the object, which is how the Wren adapter
//! recognises one of its own objects coming back and restores identity.
//!
//! One ref per object. The object's own language keeps it, as the
//! protocol's shadow, when it keeps one: a Wren object has a word for it.
//! For an object whose language keeps none, a map from the object's
//! address to the ref's. Neither roots the ref, so a ref is reachable
//! only from Haxe and dies when Haxe drops it. Its drop hook, run by the
//! core's sweep before the ref's lines can be reused, forgets it on the
//! object or in the map. The invariant: a ref kept on an object or named
//! in the map is alive. Presence means alive, because the only way out is
//! the drop of the ref itself, and the object cannot die before its ref,
//! whose trace marks it.
//!
//! Whatever is not Haxe's is wrapped the same way: a core `Str` or
//! `Error`, an object of a language registered later.

use std::ffi::c_void;
use std::ptr;
use std::sync::{LazyLock, Mutex, MutexGuard};

use caribou::bridge;
use caribou::hash::AddressMap;
use caribou::heap::{self, Handle, Tracer, TypeDesc};
use caribou::protocol::{
    CallSite, Fault, Protocol, REPLY_MISSING, REPLY_OK, REPLY_RAISED, REPLY_UNSUPPORTED, Reply,
    Send, Symbol, desc_of,
};
use caribou_abi::hl::{hl_type, vdynamic};
use caribou_abi::mem::{KIND_DYNAMIC, TRACED};
use caribou_abi::{ErrorKind, LangId, Value};

use crate::proto::{haxe_type, lang};

#[repr(C)]
struct WrenRef {
    desc: *const TypeDesc,
    obj: Value,
    /// The Haxe object standing for the foreign one (`import.rs`), or
    /// null: it holds the ref, and the ref holds it, so the two die
    /// together when Haxe lets go.
    face: *mut vdynamic,
}

unsafe extern "C" fn trace_ref(obj: *mut u8, tracer: *mut Tracer) {
    let r = unsafe { &*(obj as *const WrenRef) };
    unsafe { (*tracer).mark_value(r.obj.to_bits()) };
    if !r.face.is_null() {
        unsafe { (*tracer).mark(r.face as *const u8) };
    }
}

/// The ref is dead: the object forgets it, or its entry goes. An object
/// already forgotten by its own language's cycle, which is the common
/// end of a ref and its object, has nothing to forget.
unsafe extern "C" fn drop_ref(obj: *mut u8) {
    let r = unsafe { &*(obj as *const WrenRef) };
    if let Some(key) = address_of(r.obj)
        && heap::is_allocation_start(key as *const c_void)
        && unsafe { Send::drop_shadow(key as *mut u8, obj) }.is_err()
    {
        let mut map = refs();
        if map.get(&key) == Some(&(obj as usize)) {
            map.remove(&key);
        }
    }
}

/// Word zero of every ref. Mutable for one field: `lang` is Haxe's id,
/// written by `set_lang` at registration.
static mut WRENREF_DESC: TypeDesc = {
    let mut d = TypeDesc::new(haxe_type());
    d.trace = Some(trace_ref);
    d.drop = Some(drop_ref);
    d.protocol = &WRENREF_PROTO;
    d.name = "foreign object".as_ptr();
    d.name_len = "foreign object".len();
    d
};

fn wrenref_desc() -> *const TypeDesc {
    &raw const WRENREF_DESC
}

pub(crate) fn set_lang(lang: LangId) {
    unsafe { WRENREF_DESC.lang = lang };
}

/// Object address to ref address, for objects whose language keeps no
/// shadow; see the module doc for what an entry means. Never held across
/// an allocation: a collection's drop hooks take it.
static REFS: LazyLock<Mutex<AddressMap<usize>>> =
    LazyLock::new(|| Mutex::new(AddressMap::default()));

fn refs() -> MutexGuard<'static, AddressMap<usize>> {
    REFS.lock().unwrap_or_else(|e| e.into_inner())
}

fn address_of(v: Value) -> Option<usize> {
    match v.as_object() {
        Some(p) if !p.is_null() => Some(p as usize),
        _ => None,
    }
}

/// The ref behind `v`, if `v` is one.
fn as_ref(v: Value) -> Option<*mut WrenRef> {
    let p = address_of(v)? as *mut u8;
    ptr::eq(unsafe { desc_of(p) }, wrenref_desc()).then_some(p as *mut WrenRef)
}

/// `v` as something Haxe can hold: `v` itself when it is Haxe's or not an
/// object, else the one ref for its object, made on first need. A fresh
/// ref is not rooted; store it or root it before allocating.
pub fn wrap_foreign(v: Value) -> Value {
    let Some(obj) = address_of(v) else {
        return v;
    };
    if bridge::language_of(v) == Some(lang()) {
        return v;
    }
    let kept = match unsafe { Send::shadow(obj as *mut u8, lang()) } {
        Ok(r) => return Value::object(r as *const c_void),
        Err(Fault::Unsupported) => false,
        Err(_) => true,
    };
    if !kept && let Some(&r) = refs().get(&obj) {
        return Value::object(r as *const c_void);
    }
    // An object its language keeps a shadow on is retained through the
    // allocation by its own heap record; any other is rooted here.
    let root = if kept {
        Handle::NULL
    } else {
        heap::handle_new(obj as *mut u8)
    };
    let p = unsafe {
        heap::alloc_gen(
            wrenref_desc() as *mut hl_type,
            size_of::<WrenRef>(),
            KIND_DYNAMIC | TRACED,
        )
    } as *mut WrenRef;
    if p.is_null() {
        heap::out_of_memory("a foreign object's ref");
    }
    unsafe {
        (*p).obj = v;
        (*p).face = ptr::null_mut();
    }
    heap::handle_release(root);
    // Another thread may have made one meanwhile. Ours is then garbage,
    // and its drop forgets nothing, not being the one kept.
    let r = if kept {
        let mut other = ptr::null_mut();
        match unsafe { Send::keep_shadow(obj as *mut u8, p as *mut u8, &mut other) } {
            Ok(()) => p as usize,
            Err(Fault::Missing) => other as usize,
            Err(_) => *refs().entry(obj).or_insert(p as usize),
        }
    } else {
        *refs().entry(obj).or_insert(p as usize)
    };
    Value::object(r as *const c_void)
}

/// The object behind `v` when `v` is a ref, else `v`.
pub fn unwrap_foreign(v: Value) -> Value {
    as_ref(v).map_or(v, |r| unsafe { (*r).obj })
}

/// The live ref for `v`'s object, if Haxe holds one.
pub fn foreign_ref(v: Value) -> Option<Value> {
    let obj = address_of(v)?;
    match unsafe { Send::shadow(obj as *mut u8, lang()) } {
        Ok(r) => Some(Value::object(r as *const c_void)),
        Err(Fault::Unsupported) => refs().get(&obj).map(|&r| Value::object(r as *const c_void)),
        Err(_) => None,
    }
}

/// The Haxe object standing for the ref's object, if one was bound.
pub(crate) fn face(r: Value) -> Option<*mut vdynamic> {
    let face = unsafe { (*as_ref(r)?).face };
    (!face.is_null()).then_some(face)
}

/// Bind `face` as the Haxe object standing for the ref's object.
pub(crate) fn set_face(r: Value, face: *mut vdynamic) {
    if let Some(r) = as_ref(r) {
        unsafe { (*r).face = face };
    }
}

/// The pointer Haxe keeps in its `hl.Abstract<"caribou_obj">` field: the
/// ref itself. Null for anything that is not a ref.
pub fn wrenref_as_abstract(v: Value) -> *mut c_void {
    as_ref(v).map_or(ptr::null_mut(), |r| r as *mut c_void)
}

/// The ref a Haxe abstract field holds, as a value; null for null.
///
/// # Safety
/// `p` must be null or a pointer `wrenref_as_abstract` gave, still held
/// by Haxe.
pub unsafe fn wrenref_from_abstract(p: *mut c_void) -> Value {
    if p.is_null() {
        return Value::null();
    }
    debug_assert!(ptr::eq(unsafe { desc_of(p as *const u8) }, wrenref_desc()));
    Value::object(p)
}

// ---------------------------------------------------------------------------
// The protocol: every message goes to the object
// ---------------------------------------------------------------------------

/// The object's address, for `Send`.
unsafe fn inner(obj: *mut u8) -> *mut u8 {
    let r = unsafe { &*(obj as *const WrenRef) };
    address_of(r.obj).map_or(ptr::null_mut(), |p| p as *mut u8)
}

fn code(reply: Reply, out: *mut Value) -> u8 {
    match reply {
        Ok(v) => {
            unsafe { *out = v };
            REPLY_OK
        }
        Err(fault) => code_of(fault),
    }
}

fn code_of(fault: Fault) -> u8 {
    match fault {
        Fault::Missing => REPLY_MISSING,
        Fault::Raised => REPLY_RAISED,
        Fault::Unsupported => REPLY_UNSUPPORTED,
    }
}

unsafe extern "C-unwind" fn get_member(obj: *mut u8, name: Symbol, out: *mut Value) -> u8 {
    code(unsafe { Send::get_member(inner(obj), name) }, out)
}

unsafe extern "C-unwind" fn set_member(obj: *mut u8, name: Symbol, value: Value) -> u8 {
    match unsafe { Send::set_member(inner(obj), name, value) } {
        Ok(()) => REPLY_OK,
        Err(fault) => code_of(fault),
    }
}

unsafe extern "C-unwind" fn invoke(
    obj: *mut u8,
    name: Symbol,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, n) }
    };
    code(unsafe { Send::invoke(inner(obj), name, args) }, out)
}

// The site is the caller's; the Wren object's protocol fills it. A direct
// send it leaves there would be called on the ref, not the object, so
// none is kept: a call through a ref takes the plain path every time.
unsafe extern "C-unwind" fn get_member_at(
    obj: *mut u8,
    name: Symbol,
    site: *mut CallSite,
    out: *mut Value,
) -> u8 {
    let r = unsafe { Send::get_member_at(inner(obj), name, &*site) };
    unsafe { &*site }.clear_direct();
    code(r, out)
}

unsafe extern "C-unwind" fn set_member_at(
    obj: *mut u8,
    name: Symbol,
    site: *mut CallSite,
    value: Value,
) -> u8 {
    let r = unsafe { Send::set_member_at(inner(obj), name, &*site, value) };
    unsafe { &*site }.clear_direct();
    match r {
        Ok(()) => REPLY_OK,
        Err(fault) => code_of(fault),
    }
}

unsafe extern "C-unwind" fn invoke_at(
    obj: *mut u8,
    name: Symbol,
    site: *mut CallSite,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, n) }
    };
    let r = unsafe { Send::invoke_at(inner(obj), name, &*site, args) };
    unsafe { &*site }.clear_direct();
    code(r, out)
}

unsafe extern "C-unwind" fn call(
    obj: *mut u8,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let args = if n == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, n) }
    };
    code(unsafe { Send::call(inner(obj), args) }, out)
}

unsafe extern "C-unwind" fn index(obj: *mut u8, key: Value, out: *mut Value) -> u8 {
    code(unsafe { Send::index(inner(obj), key) }, out)
}

unsafe extern "C-unwind" fn set_index(obj: *mut u8, key: Value, value: Value) -> u8 {
    match unsafe { Send::set_index(inner(obj), key, value) } {
        Ok(()) => REPLY_OK,
        Err(fault) => code_of(fault),
    }
}

unsafe extern "C-unwind" fn len(obj: *mut u8, out: *mut usize) -> u8 {
    match unsafe { Send::len(inner(obj)) } {
        Ok(n) => {
            unsafe { *out = n };
            REPLY_OK
        }
        Err(fault) => code_of(fault),
    }
}

unsafe extern "C-unwind" fn arity(obj: *mut u8, out: *mut usize) -> u8 {
    match unsafe { Send::arity(inner(obj)) } {
        Ok(n) => {
            unsafe { *out = n };
            REPLY_OK
        }
        Err(fault) => code_of(fault),
    }
}

unsafe extern "C-unwind" fn iterate(obj: *mut u8, state: *mut Value, out: *mut Value) -> u8 {
    code(unsafe { Send::iterate(inner(obj), &mut *state) }, out)
}

unsafe extern "C-unwind" fn to_string(obj: *mut u8, out: *mut Value) -> u8 {
    code(unsafe { Send::to_string(inner(obj)) }, out)
}

unsafe extern "C-unwind" fn hash(obj: *mut u8, out: *mut u64) -> u8 {
    match unsafe { Send::hash(inner(obj)) } {
        Ok(h) => {
            unsafe { *out = h };
            REPLY_OK
        }
        Err(fault) => code_of(fault),
    }
}

/// The object's own equality, against the object behind `other` when
/// that is a ref too.
unsafe extern "C-unwind" fn equals(obj: *mut u8, other: Value, out: *mut bool) -> u8 {
    match unsafe { Send::equals(inner(obj), unwrap_foreign(other)) } {
        Ok(same) => {
            unsafe { *out = same };
            REPLY_OK
        }
        Err(fault) => code_of(fault),
    }
}

/// The object itself: what a proxy stands for.
unsafe extern "C-unwind" fn unwrap_native(obj: *mut u8, out: *mut *mut c_void) -> u8 {
    unsafe { *out = inner(obj).cast() };
    REPLY_OK
}

unsafe extern "C-unwind" fn is_error(obj: *mut u8) -> bool {
    unsafe { Send::is_error(inner(obj)) }
}

unsafe extern "C-unwind" fn error_kind(obj: *mut u8) -> ErrorKind {
    unsafe { Send::error_kind(inner(obj)) }.unwrap_or(ErrorKind::Runtime)
}

unsafe extern "C-unwind" fn error_message(obj: *mut u8, out: *mut Value) -> u8 {
    code(unsafe { Send::error_message(inner(obj)) }, out)
}

unsafe extern "C-unwind" fn error_cause(obj: *mut u8, out: *mut Value) -> u8 {
    code(unsafe { Send::error_cause(inner(obj)) }, out)
}

unsafe extern "C-unwind" fn error_trace(obj: *mut u8, out: *mut Value) -> u8 {
    code(unsafe { Send::error_trace(inner(obj)) }, out)
}

unsafe extern "C-unwind" fn type_name(obj: *mut u8, out: *mut Value) -> u8 {
    code(unsafe { Send::type_name(inner(obj)) }, out)
}

// A shadow is the object's, so a ref forwards it: whoever stands for the
// ref stands for the object, and the ref is what stands for it in Haxe.
unsafe extern "C-unwind" fn shadow(obj: *mut u8, lang: LangId, out: *mut *mut u8) -> u8 {
    match unsafe { Send::shadow(inner(obj), lang) } {
        Ok(p) => {
            unsafe { *out = p };
            REPLY_OK
        }
        Err(fault) => code_of(fault),
    }
}

unsafe extern "C-unwind" fn keep_shadow(obj: *mut u8, shadow: *mut u8, out: *mut *mut u8) -> u8 {
    match unsafe { Send::keep_shadow(inner(obj), shadow, &mut *out) } {
        Ok(()) => REPLY_OK,
        Err(fault) => code_of(fault),
    }
}

unsafe extern "C-unwind" fn drop_shadow(obj: *mut u8, shadow: *mut u8) -> u8 {
    match unsafe { Send::drop_shadow(inner(obj), shadow) } {
        Ok(()) => REPLY_OK,
        Err(fault) => code_of(fault),
    }
}

static WRENREF_PROTO: Protocol = Protocol {
    get_member: Some(get_member),
    set_member: Some(set_member),
    invoke: Some(invoke),
    get_member_at: Some(get_member_at),
    set_member_at: Some(set_member_at),
    invoke_at: Some(invoke_at),
    call: Some(call),
    index: Some(index),
    set_index: Some(set_index),
    len: Some(len),
    arity: Some(arity),
    iterate: Some(iterate),
    to_string: Some(to_string),
    hash: Some(hash),
    equals: Some(equals),
    unwrap_native: Some(unwrap_native),
    is_error: Some(is_error),
    error_message: Some(error_message),
    error_kind: Some(error_kind),
    error_cause: Some(error_cause),
    error_trace: Some(error_trace),
    type_name: Some(type_name),
    shadow: Some(shadow),
    keep_shadow: Some(keep_shadow),
    drop_shadow: Some(drop_shadow),
};

#[cfg(test)]
mod tests {
    use super::*;
    use caribou::error::Str;
    use caribou::symbol::intern;

    /// A core string, wrapped twice, the ref rooted by a handle. Answers
    /// the handle and the two addresses inverted: this frame is gone and
    /// the caller's holds no word the conservative scan takes for either.
    #[inline(never)]
    fn held() -> (Handle, usize, usize) {
        let _lock = heap::gc_guard();
        let s = Str::new("held");
        let sv = Str::value(s);
        let r = wrap_foreign(sv);
        assert_eq!(
            wrap_foreign(sv).to_bits(),
            r.to_bits(),
            "one ref per object"
        );
        assert_ne!(r.to_bits(), sv.to_bits());
        assert_eq!(unwrap_foreign(r).to_bits(), sv.to_bits());
        assert_eq!(foreign_ref(sv).map(Value::to_bits), Some(r.to_bits()));
        let rp = r.as_object().unwrap() as *mut u8;
        assert_eq!(wrenref_as_abstract(r), rp.cast());
        assert_eq!(
            unsafe { wrenref_from_abstract(rp.cast()) }.to_bits(),
            r.to_bits()
        );
        assert!(wrenref_as_abstract(sv).is_null());
        // Messages go to the string.
        unsafe {
            assert_eq!(Str::text(Send::to_string(rp).unwrap()), Some("held"));
            assert_eq!(Send::equals(rp, sv), Ok(true));
            assert_eq!(Send::equals(rp, r), Ok(true));
            assert_eq!(Send::hash(rp), Send::hash(s as *mut u8));
            assert_eq!(Send::unwrap_native(rp), Ok(s as *mut c_void));
            assert_eq!(
                Send::get_member(rp, intern("x")),
                Send::get_member(s as *mut u8, intern("x"))
            );
            assert!(!Send::is_error(rp));
        }
        (heap::handle_new(rp), !(rp as usize), !(s as usize))
    }

    #[test]
    fn one_ref_per_object_while_haxe_holds_it() {
        heap::init();
        heap::gc_register_current_os_thread();
        // What registration does: Haxe's id, so a core string is foreign.
        crate::proto::set_lang(41);
        set_lang(41);
        let (root, ref_hidden, obj_hidden) = held();
        let obj = Value::object(!obj_hidden as *const c_void);

        heap::major();
        assert_eq!(
            foreign_ref(obj).map(Value::to_bits),
            Some(Value::object(!ref_hidden as *const c_void).to_bits()),
            "the ref lives while Haxe's handle does"
        );

        heap::handle_release(root);
        scrub_stack();
        heap::major();
        assert_eq!(foreign_ref(obj), None, "dropped, the ref left the map");
        heap::gc_unregister_current_os_thread();
    }

    /// Overwrite the stack below this frame, where `foreign_ref` left the
    /// ref's address for the conservative scan to find.
    #[inline(never)]
    fn scrub_stack() {
        let buf = [0u8; 1 << 14];
        std::hint::black_box(&buf);
    }

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
