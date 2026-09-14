//! A cell: the one core object standing for an object of another
//! language, in the terms of the languages that hold it.
//!
//! A language holding another's object needs something its own compiled
//! code can read where it reads its own objects, on every send: Haxe
//! reads an `hl_type` at word zero and dispatches, casts and tests types
//! through it; wren_lift reads an instance header at the address it
//! holds. A cell is a core object laid out so that each holder finds its
//! own header at the offset its code expects:
//!
//! | offset | what |
//! |---|---|
//! | 0 | the descriptor: a `TypeDesc` starts with an `hl_type`, which the holder's adapter fills as its own type, mirroring one of its classes |
//! | 8 | the bridge word a hosted collector marks in, as it marks its own objects' prefix |
//! | 16 | a header for a holder whose code reads more than a word: wren_lift's instance, 40 bytes, at the offset its prefix puts it |
//! | 56 | the object the cell stands for, as the bridge value it crossed as |
//! | 64 | the holder's own object in front of the cell, or null |
//! | 72 | flags |
//!
//! So a cell is an instance of a Haxe class to HashLink, an instance of
//! a Wren class to wren_lift (its address plus 16), and a core object to
//! the bridge, which unwraps it. The cell's protocol forwards every
//! message to the object it stands for, so a caller reaching it through
//! `Dynamic` gets the object's own semantics, and it answers
//! `unwrap_native` with the object, which is how the object's adapter
//! recognises it coming home. A cell is known by that protocol.
//!
//! One cell per object. The object's own language keeps it, as the
//! protocol's shadow, when it keeps one; for an object whose language
//! keeps none, a map from the object's address to the cell's. Neither
//! roots the cell, so a cell is reachable only from its holders and dies
//! when they drop it. Its trace marks the object, and the core's mark is
//! what the object's own cycle ends with, so a live cell keeps its
//! object. Its drop hook, run by the sweep before the cell's lines can be
//! reused, forgets it on the object or in the map: a cell kept on an
//! object or named in the map is alive, because the only way out is the
//! drop of the cell itself, and the object cannot die before a cell whose
//! trace marks it.
//!
//! A holder may make an object of its own to stand in front of the cell,
//! a class instance its code constructed, and the cell keeps it, so the
//! object always comes back as that one.

use core::ffi::c_void;
use core::ptr;
use std::sync::{LazyLock, Mutex, MutexGuard};

use caribou_abi::hl::hl_type;
use caribou_abi::mem::{KIND_DYNAMIC, TRACED};
use caribou_abi::{ErrorKind, LangId, Value};

use crate::hash::AddressMap;
use crate::heap::{self, Handle, Tracer, TypeDesc};
use crate::protocol::{
    CallSite, Fault, Protocol, REPLY_MISSING, REPLY_OK, REPLY_RAISED, REPLY_UNSUPPORTED, Reply,
    Send, Symbol,
};

#[repr(C)]
pub struct Cell {
    desc: *const TypeDesc,
    bridge: usize,
    view: [u64; VIEW_WORDS],
    /// The object it stands for, as the bridge value it crossed as.
    obj: Value,
    /// The holder's own object in front of the cell, or null: it holds
    /// the cell, and the cell holds it, so the two die together.
    front: *mut c_void,
    flags: usize,
}

/// Where the second holder's header lies, and how many words it has.
pub const VIEW: usize = 16;
const VIEW_WORDS: usize = 5;

/// The cell is named in the map, not kept as a shadow.
const MAPPED: usize = 1;

/// A cell descriptor for holder `lang`: `prefix` is what the holder's
/// code reads at word zero, `name` what the object is described as.
/// Kept for the process; one per view a holder has.
pub fn descriptor(prefix: hl_type, lang: LangId, name: &str) -> &'static TypeDesc {
    let name: &'static str = String::leak(name.to_owned());
    let mut d = TypeDesc::new(prefix);
    d.trace = Some(trace);
    d.drop = Some(drop);
    d.protocol = &PROTO;
    d.name = name.as_ptr();
    d.name_len = name.len();
    d.lang = lang;
    Box::leak(Box::new(d))
}

/// Give `desc` its holder's language: once, at the holder's
/// registration, before a cell is made under it.
///
/// # Safety
/// `desc` came from `descriptor`, and no cell under it exists yet.
pub unsafe fn set_lang(desc: &'static TypeDesc, lang: LangId) {
    unsafe { (*(desc as *const TypeDesc as *mut TypeDesc)).lang = lang };
}

/// Whether the object at `p` is a cell, by its word zero: a descriptor
/// whose protocol is the cell's.
///
/// # Safety
/// `p` must be a live object whose word zero is an `hl_type*`.
#[inline]
pub unsafe fn is_cell(p: *const u8) -> bool {
    let t = unsafe { *(p as *const *const hl_type) };
    let descriptor = unsafe { heap::is_descriptor(t) };
    descriptor && ptr::eq(unsafe { (*(t as *const TypeDesc)).protocol }, &PROTO)
}

/// The cell `v` is, if it is one.
fn as_cell(v: Value) -> Option<*mut Cell> {
    let p = address_of(v)? as *mut u8;
    unsafe { is_cell(p) }.then_some(p as *mut Cell)
}

fn address_of(v: Value) -> Option<usize> {
    match v.as_object() {
        Some(p) if !p.is_null() => Some(p as usize),
        _ => None,
    }
}

/// Object address to cell address, for objects whose language keeps no
/// shadow. Never held across an allocation: a collection's drop hooks
/// take it.
static CELLS: LazyLock<Mutex<AddressMap<usize>>> =
    LazyLock::new(|| Mutex::new(AddressMap::default()));

fn cells() -> MutexGuard<'static, AddressMap<usize>> {
    CELLS.lock().unwrap_or_else(|e| e.into_inner())
}

fn alloc(desc: &'static TypeDesc, v: Value, flags: usize) -> *mut Cell {
    let p = unsafe {
        heap::alloc_gen(
            desc as *const TypeDesc as *mut hl_type,
            size_of::<Cell>(),
            KIND_DYNAMIC | TRACED,
        )
    } as *mut Cell;
    if p.is_null() {
        heap::out_of_memory("a cell");
    }
    unsafe {
        (*p).obj = v;
        (*p).flags = flags;
    }
    p
}

/// The cell for `v`'s object under the holder `desc` is for, made on
/// first need; `v` itself when it is not an object. A fresh cell is not
/// rooted: store it or root it before allocating. A cell that exists
/// already is answered as it is, whatever descriptor it was made under;
/// `view` changes that.
pub fn wrap(v: Value, desc: &'static TypeDesc) -> Value {
    let Some(obj) = address_of(v) else {
        return v;
    };
    let lang = desc.lang;
    let kept = match unsafe { Send::shadow(obj as *mut u8, lang) } {
        Ok(c) => return Value::object(c as *const c_void),
        Err(Fault::Unsupported) => false,
        Err(_) => true,
    };
    if !kept && let Some(&c) = cells().get(&obj) {
        return Value::object(c as *const c_void);
    }
    // An object its language keeps a shadow on is retained through the
    // allocation by its own heap record; any other is rooted here.
    let root = if kept {
        Handle::NULL
    } else {
        heap::handle_new(obj as *mut u8)
    };
    let p = alloc(desc, v, if kept { 0 } else { MAPPED });
    heap::handle_release(root);
    // Another thread may have made one meanwhile. Ours is then garbage,
    // and its drop forgets nothing, not being the one kept.
    let c = if kept {
        let mut other = ptr::null_mut();
        match unsafe { Send::keep_shadow(obj as *mut u8, p as *mut u8, &mut other) } {
            Ok(()) => p as usize,
            Err(Fault::Missing) => other as usize,
            Err(_) => {
                unsafe { (*p).flags |= MAPPED };
                *cells().entry(obj).or_insert(p as usize)
            }
        }
    } else {
        *cells().entry(obj).or_insert(p as usize)
    };
    Value::object(c as *const c_void)
}

/// The live cell for `v`'s object under holder `lang`, if there is one.
pub fn of(v: Value, lang: LangId) -> Option<Value> {
    let obj = address_of(v)?;
    match unsafe { Send::shadow(obj as *mut u8, lang) } {
        Ok(c) => Some(Value::object(c as *const c_void)),
        Err(Fault::Unsupported) => cells()
            .get(&obj)
            .map(|&c| Value::object(c as *const c_void)),
        Err(_) => None,
    }
}

/// The object behind `v` when `v` is a cell, else `v`.
pub fn unwrap(v: Value) -> Value {
    as_cell(v).map_or(v, |c| unsafe { (*c).obj })
}

/// The object the cell at `p` stands for.
///
/// # Safety
/// `p` must be a live cell.
pub unsafe fn object_at(p: *const u8) -> Value {
    unsafe { (*(p as *const Cell)).obj }
}

/// The second holder's header in the cell at `p`, `VIEW` in: zero until
/// that holder fills it.
///
/// # Safety
/// `p` must be a live cell.
pub unsafe fn view_at(p: *mut u8) -> *mut u8 {
    unsafe { p.add(VIEW) }
}

/// The descriptor the cell `v` is read under.
pub fn descriptor_of(v: Value) -> Option<&'static TypeDesc> {
    as_cell(v).map(|c| unsafe { &*(*c).desc })
}

/// The descriptor the cell `v` is read under and the holder's object in
/// front of it, null for none: what a holder asks on every crossing, in
/// one read of the cell.
pub fn view_of(v: Value) -> Option<(&'static TypeDesc, *mut c_void)> {
    let c = as_cell(v)?;
    Some(unsafe { (&*(*c).desc, (*c).front) })
}

/// Read the cell `v` under `desc` from now on: word zero, which the
/// holder's code has not seen while the cell was held only as a pointer
/// it never reads. Nothing for a value that is not a cell.
pub fn view(v: Value, desc: &'static TypeDesc) {
    if let Some(c) = as_cell(v) {
        unsafe { (*c).desc = desc };
    }
}

/// The holder's object in front of the cell `v`, if it made one.
pub fn front(v: Value) -> Option<*mut c_void> {
    let p = unsafe { (*as_cell(v)?).front };
    (!p.is_null()).then_some(p)
}

/// Put `p`, the holder's own object, in front of the cell `v`.
pub fn set_front(v: Value, p: *mut c_void) {
    if let Some(c) = as_cell(v) {
        unsafe { (*c).front = p };
    }
}

/// The cell at `p` as a value; null for null.
///
/// # Safety
/// `p` must be null or a cell's address a holder kept.
pub unsafe fn from_pointer(p: *mut c_void) -> Value {
    if p.is_null() {
        return Value::null();
    }
    debug_assert!(unsafe { is_cell(p as *const u8) });
    Value::object(p)
}

unsafe extern "C" fn trace(obj: *mut u8, tracer: *mut Tracer) {
    let c = unsafe { &*(obj as *const Cell) };
    unsafe { (*tracer).mark_value(c.obj.to_bits()) };
    if !c.front.is_null() {
        unsafe { (*tracer).mark(c.front as *const u8) };
    }
}

/// The cell is dead: its entry goes, or the object forgets it. An object
/// already forgotten by its own language's cycle, which is the common
/// end of a cell and its object, has nothing to forget.
unsafe extern "C" fn drop(obj: *mut u8) {
    let c = unsafe { &*(obj as *const Cell) };
    let Some(key) = address_of(c.obj) else {
        return;
    };
    if c.flags & MAPPED != 0 {
        let mut map = cells();
        if map.get(&key) == Some(&(obj as usize)) {
            map.remove(&key);
        }
    } else if heap::is_allocation_start(key as *const c_void) {
        let _ = unsafe { Send::drop_shadow(key as *mut u8, obj) };
    }
}

// ---------------------------------------------------------------------------
// The protocol: every message goes to the object
// ---------------------------------------------------------------------------

/// The object's address, for `Send`.
unsafe fn inner(obj: *mut u8) -> *mut u8 {
    let c = unsafe { &*(obj as *const Cell) };
    address_of(c.obj).map_or(ptr::null_mut(), |p| p as *mut u8)
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

// The site is the caller's; the object's protocol fills it. A direct
// send it leaves there would be called on the cell, not the object, so
// none is kept: a call through a cell takes the plain path every time.
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
/// that is a cell too.
unsafe extern "C-unwind" fn equals(obj: *mut u8, other: Value, out: *mut bool) -> u8 {
    match unsafe { Send::equals(inner(obj), unwrap(other)) } {
        Ok(same) => {
            unsafe { *out = same };
            REPLY_OK
        }
        Err(fault) => code_of(fault),
    }
}

/// The object itself: what the cell stands for.
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

unsafe extern "C-unwind" fn type_name(obj: *mut u8, out: *mut Symbol) -> u8 {
    match unsafe { Send::type_name(inner(obj)) } {
        Ok(sym) => {
            unsafe { *out = sym };
            REPLY_OK
        }
        Err(fault) => code_of(fault),
    }
}

// A shadow is the object's, so a cell forwards it: whoever stands for
// the cell stands for the object.
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

static PROTO: Protocol = Protocol {
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
    use crate::error::Str;
    use crate::symbol::intern;
    use caribou_abi::hl::hl_type_detail;

    /// A holder's view: an abstract type, as a language with no class to
    /// mirror would read it.
    fn plain() -> &'static TypeDesc {
        static DESC: std::sync::OnceLock<&'static TypeDesc> = std::sync::OnceLock::new();
        DESC.get_or_init(|| {
            descriptor(
                hl_type {
                    kind: caribou_abi::hl::HABSTRACT,
                    detail: hl_type_detail {
                        abs_name: ptr::null(),
                    },
                    vobj_proto: ptr::null_mut(),
                    mark_bits: ptr::null_mut(),
                },
                41,
                "foreign object",
            )
        })
    }

    /// A core string, wrapped twice, the cell rooted by a handle. Answers
    /// the handle and the two addresses inverted: this frame is gone and
    /// the caller's holds no word the conservative scan takes for either.
    #[inline(never)]
    fn held() -> (Handle, usize, usize) {
        let _lock = heap::gc_guard();
        let s = Str::new("held");
        let sv = Str::value(s);
        let c = wrap(sv, plain());
        assert_eq!(
            wrap(sv, plain()).to_bits(),
            c.to_bits(),
            "one cell per object"
        );
        assert_ne!(c.to_bits(), sv.to_bits());
        assert_eq!(unwrap(c).to_bits(), sv.to_bits());
        assert_eq!(of(sv, 41).map(Value::to_bits), Some(c.to_bits()));
        let cp = c.as_object().unwrap() as *mut u8;
        assert!(unsafe { is_cell(cp) });
        assert!(!unsafe { is_cell(s as *const u8) });
        assert_eq!(unsafe { from_pointer(cp.cast()) }.to_bits(), c.to_bits());
        assert_eq!(front(c), None);
        assert!(ptr::eq(descriptor_of(c).unwrap(), plain()));
        // Messages go to the string.
        unsafe {
            assert_eq!(Str::text(Send::to_string(cp).unwrap()), Some("held"));
            assert_eq!(Send::equals(cp, sv), Ok(true));
            assert_eq!(Send::equals(cp, c), Ok(true));
            assert_eq!(Send::hash(cp), Send::hash(s as *mut u8));
            assert_eq!(Send::unwrap_native(cp), Ok(s as *mut c_void));
            assert_eq!(
                Send::get_member(cp, intern("x")),
                Send::get_member(s as *mut u8, intern("x"))
            );
            assert!(!Send::is_error(cp));
        }
        (heap::handle_new(cp), !(cp as usize), !(s as usize))
    }

    #[test]
    fn one_cell_per_object_while_the_holder_keeps_it() {
        heap::init();
        heap::gc_register_current_os_thread();
        let (root, cell_hidden, obj_hidden) = held();
        let obj = Value::object(!obj_hidden as *const c_void);

        heap::major();
        assert_eq!(
            of(obj, 41).map(Value::to_bits),
            Some(Value::object(!cell_hidden as *const c_void).to_bits()),
            "the cell lives while the holder's handle does"
        );

        heap::handle_release(root);
        scrub_stack();
        heap::major();
        assert_eq!(of(obj, 41), None, "dropped, the cell left the map");
        heap::gc_unregister_current_os_thread();
    }

    /// Overwrite the stack below this frame, where `of` left the cell's
    /// address for the conservative scan to find.
    #[inline(never)]
    fn scrub_stack() {
        let buf = [0u8; 1 << 14];
        std::hint::black_box(&buf);
    }

    #[test]
    fn scalars_pass_through() {
        assert_eq!(
            wrap(Value::null(), plain()).to_bits(),
            Value::null().to_bits()
        );
        assert_eq!(
            wrap(Value::int(3), plain()).to_bits(),
            Value::int(3).to_bits()
        );
        assert_eq!(unwrap(Value::int(3)).to_bits(), Value::int(3).to_bits());
        assert_eq!(of(Value::number(1.0), 41), None);
        assert!(unsafe { from_pointer(ptr::null_mut()) }.is_null());
    }
}
