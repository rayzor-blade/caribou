//! The messages every heap object answers, whichever language made it, and
//! the callable the bridge invokes. Adapters implement `Protocol` once per
//! runtime; the core dispatches through the `TypeDesc` an object's word zero
//! names.

use core::ffi::c_void;

use std::sync::atomic::{AtomicUsize, Ordering};

use caribou_abi::{ErrorKind, LangId, Value};

use crate::heap::TypeDesc;

pub use crate::symbol::Symbol;

/// Why a message did not produce a value.
#[derive(Debug, PartialEq, Eq)]
pub enum Fault {
    /// The object does not answer this message.
    Unsupported,
    /// No such member, index or key.
    Missing,
    /// The callee raised; the error value is pending on the current task.
    Raised,
}

pub type Reply = Result<Value, Fault>;

/// The vtable a `TypeDesc` points at. Every entry has a default that
/// answers `Unsupported`, so an adapter implements only what its type does.
///
/// Entries are C-ABI so a plugin can supply one; they are `C-unwind` so a
/// Rust entry's panic reaches the bridge's protected boundary instead of
/// aborting inside the entry. `obj` is the object's address; `args` are
/// `Value`s. A reply of `Err(Fault::Raised)` means the entry raised through
/// the bridge and the error is pending on the current task.
#[repr(C)]
pub struct Protocol {
    pub get_member:
        Option<unsafe extern "C-unwind" fn(obj: *mut u8, name: Symbol, out: *mut Value) -> u8>,
    pub set_member:
        Option<unsafe extern "C-unwind" fn(obj: *mut u8, name: Symbol, value: Value) -> u8>,
    pub invoke: Option<
        unsafe extern "C-unwind" fn(
            obj: *mut u8,
            name: Symbol,
            args: *const Value,
            n: usize,
            out: *mut Value,
        ) -> u8,
    >,
    /// `get_member`, `set_member` and `invoke` with a [`CallSite`] the
    /// caller keeps: what the entry derived last time, keyed as it chose,
    /// so a site that sees the same shape again does no lookup. Optional;
    /// the bridge falls back to the plain entries.
    pub get_member_at: Option<
        unsafe extern "C-unwind" fn(
            obj: *mut u8,
            name: Symbol,
            site: *mut CallSite,
            out: *mut Value,
        ) -> u8,
    >,
    pub set_member_at: Option<
        unsafe extern "C-unwind" fn(
            obj: *mut u8,
            name: Symbol,
            site: *mut CallSite,
            value: Value,
        ) -> u8,
    >,
    pub invoke_at: Option<
        unsafe extern "C-unwind" fn(
            obj: *mut u8,
            name: Symbol,
            site: *mut CallSite,
            args: *const Value,
            n: usize,
            out: *mut Value,
        ) -> u8,
    >,
    pub call: Option<
        unsafe extern "C-unwind" fn(
            obj: *mut u8,
            args: *const Value,
            n: usize,
            out: *mut Value,
        ) -> u8,
    >,
    pub index: Option<unsafe extern "C-unwind" fn(obj: *mut u8, key: Value, out: *mut Value) -> u8>,
    pub set_index:
        Option<unsafe extern "C-unwind" fn(obj: *mut u8, key: Value, value: Value) -> u8>,
    pub len: Option<unsafe extern "C-unwind" fn(obj: *mut u8, out: *mut usize) -> u8>,
    /// How many arguments `call` takes; `Unsupported` for what is not a
    /// function. The mark of a callable value.
    pub arity: Option<unsafe extern "C-unwind" fn(obj: *mut u8, out: *mut usize) -> u8>,
    /// Steps an iterator: `state` starts as `Value::null()`; returns `Missing`
    /// when exhausted.
    pub iterate:
        Option<unsafe extern "C-unwind" fn(obj: *mut u8, state: *mut Value, out: *mut Value) -> u8>,
    pub to_string: Option<unsafe extern "C-unwind" fn(obj: *mut u8, out: *mut Value) -> u8>,
    pub hash: Option<unsafe extern "C-unwind" fn(obj: *mut u8, out: *mut u64) -> u8>,
    pub equals:
        Option<unsafe extern "C-unwind" fn(obj: *mut u8, other: Value, out: *mut bool) -> u8>,
    pub unwrap_native:
        Option<unsafe extern "C-unwind" fn(obj: *mut u8, out: *mut *mut c_void) -> u8>,
    pub is_error: Option<unsafe extern "C-unwind" fn(obj: *mut u8) -> bool>,
    pub error_message: Option<unsafe extern "C-unwind" fn(obj: *mut u8, out: *mut Value) -> u8>,
    pub error_kind: Option<unsafe extern "C-unwind" fn(obj: *mut u8) -> ErrorKind>,
    pub error_cause: Option<unsafe extern "C-unwind" fn(obj: *mut u8, out: *mut Value) -> u8>,
    pub error_trace: Option<unsafe extern "C-unwind" fn(obj: *mut u8, out: *mut Value) -> u8>,
    /// The object's type in its own language's terms, as a core `Str`:
    /// what the registry's `ClassIface::type_name` holds for its class.
    pub type_name: Option<unsafe extern "C-unwind" fn(obj: *mut u8, out: *mut Value) -> u8>,
    /// The object of language `lang` standing for this one, which this
    /// object's language keeps on it so the same object crossing twice is
    /// the same object on the other side; `Missing` when none is kept.
    /// Optional: a language that keeps none answers `Unsupported`, and the
    /// other language remembers its own.
    pub shadow:
        Option<unsafe extern "C-unwind" fn(obj: *mut u8, lang: LangId, out: *mut *mut u8) -> u8>,
    /// Keep `shadow` as the object of its language standing for this one,
    /// when none of that language is kept yet; else `Missing`, with the one
    /// kept in `out`.
    pub keep_shadow:
        Option<unsafe extern "C-unwind" fn(obj: *mut u8, shadow: *mut u8, out: *mut *mut u8) -> u8>,
    /// Forget `shadow`, if it is the one kept.
    pub drop_shadow: Option<unsafe extern "C-unwind" fn(obj: *mut u8, shadow: *mut u8) -> u8>,
}

/// Reply codes an entry returns.
pub const REPLY_OK: u8 = 0;
pub const REPLY_UNSUPPORTED: u8 = 1;
pub const REPLY_MISSING: u8 = 2;
pub const REPLY_RAISED: u8 = 3;

impl Protocol {
    /// Answers `Unsupported` to everything.
    pub const NONE: Protocol = Protocol {
        get_member: None,
        set_member: None,
        invoke: None,
        get_member_at: None,
        set_member_at: None,
        invoke_at: None,
        call: None,
        index: None,
        set_index: None,
        len: None,
        arity: None,
        iterate: None,
        to_string: None,
        hash: None,
        equals: None,
        unwrap_native: None,
        is_error: None,
        error_message: None,
        error_kind: None,
        error_cause: None,
        error_trace: None,
        type_name: None,
        shadow: None,
        keep_shadow: None,
        drop_shadow: None,
    };
}

/// A foreign call site's cache: three words the callee's protocol fills
/// with what it derived for the site, `key` naming what the rest is good
/// for (a class, a type, a VM), and reads back when `key` matches. Zero is
/// empty. Owned by the caller for the life of the site; shared, since a
/// site may be reached from several threads, and a stale read costs one
/// lookup.
///
/// A callee that can do the whole send for the site in one function
/// leaves that function in `direct`, with `a` and `b` as its own data:
/// the bridge then calls it before anything else, and takes the plain
/// path again when it answers `Missing` or `Unsupported`. `key`, `a` and
/// `b` belong to whichever the callee filled last. `plain` counts the
/// sends that took the plain path, for the run report. `reentrant` is
/// set once a send from the site has called back into the caller's
/// language (see `bridge::enter`).
#[repr(C)]
#[derive(Debug, Default)]
pub struct CallSite {
    pub key: AtomicUsize,
    pub a: AtomicUsize,
    pub b: AtomicUsize,
    pub direct: AtomicUsize,
    pub plain: AtomicUsize,
    pub reentrant: AtomicUsize,
}

/// The whole send for one site: `target` is what the bridge would have
/// sent to, the receiver's core address or a typed callable's function,
/// and the site holds what the callee left there for itself. Answers as a
/// protocol entry does; `Missing` and `Unsupported` mean the site no
/// longer fits and the plain path should decide.
pub type Direct = unsafe extern "C-unwind" fn(
    site: *const CallSite,
    target: usize,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8;

impl CallSite {
    pub const fn new() -> CallSite {
        CallSite {
            key: AtomicUsize::new(0),
            a: AtomicUsize::new(0),
            b: AtomicUsize::new(0),
            direct: AtomicUsize::new(0),
            plain: AtomicUsize::new(0),
            reentrant: AtomicUsize::new(0),
        }
    }

    /// `(a, b)` when the site was filled under `key` and holds no direct
    /// send.
    #[inline]
    pub fn get(&self, key: usize) -> Option<(usize, usize)> {
        (key != 0
            && self.direct.load(Ordering::Relaxed) == 0
            && self.key.load(Ordering::Acquire) == key)
            .then(|| {
                (
                    self.a.load(Ordering::Relaxed),
                    self.b.load(Ordering::Relaxed),
                )
            })
    }

    #[inline]
    pub fn set(&self, key: usize, a: usize, b: usize) {
        self.direct.store(0, Ordering::Relaxed);
        self.a.store(a, Ordering::Relaxed);
        self.b.store(b, Ordering::Relaxed);
        self.key.store(key, Ordering::Release);
    }

    /// The direct send, when the callee left one.
    #[inline]
    pub fn direct(&self) -> Option<Direct> {
        let f = self.direct.load(Ordering::Acquire);
        (f != 0).then(|| unsafe { std::mem::transmute::<usize, Direct>(f) })
    }

    /// Leave `f` as the direct send, with `key`, `a` and `b` for it.
    #[inline]
    pub fn set_direct(&self, f: Direct, key: usize, a: usize, b: usize) {
        self.key.store(key, Ordering::Relaxed);
        self.a.store(a, Ordering::Relaxed);
        self.b.store(b, Ordering::Relaxed);
        self.direct.store(f as usize, Ordering::Release);
    }

    /// Forget the direct send; the plain path fills the site again.
    #[inline]
    pub fn clear_direct(&self) {
        self.direct.store(0, Ordering::Release);
    }

    /// One more send took the plain path. A load and a store, not a
    /// read-modify-write: a count two threads race on may lose one, and
    /// the report can bear that.
    #[inline]
    pub fn note_plain(&self) {
        let n = self.plain.load(Ordering::Relaxed);
        self.plain.store(n.wrapping_add(1), Ordering::Relaxed);
    }

    /// How many sends took the plain path.
    pub fn plain(&self) -> usize {
        self.plain.load(Ordering::Relaxed)
    }

    /// Whether a send from this site has called back into the caller's
    /// language.
    #[inline]
    pub fn reentrant(&self) -> bool {
        self.reentrant.load(Ordering::Relaxed) != 0
    }

    #[inline]
    pub fn note_reentrant(&self) {
        self.reentrant.store(1, Ordering::Relaxed);
    }

    /// `key`, `a` and `b` as the direct send left them.
    #[inline]
    pub fn words(&self) -> (usize, usize, usize) {
        (
            self.key.load(Ordering::Relaxed),
            self.a.load(Ordering::Relaxed),
            self.b.load(Ordering::Relaxed),
        )
    }
}

/// A reply code and an out-value into a `Reply`.
#[inline]
pub(crate) fn reply(code: u8, out: Value) -> Reply {
    match code {
        REPLY_OK => Ok(out),
        REPLY_MISSING => Err(Fault::Missing),
        REPLY_RAISED => Err(Fault::Raised),
        _ => Err(Fault::Unsupported),
    }
}

/// The descriptor of a heap object, from its word zero.
///
/// # Safety
/// `obj` must be a live object whose word zero is a `*const TypeDesc`.
pub unsafe fn desc_of(obj: *const u8) -> *const TypeDesc {
    unsafe { *(obj as *const *const TypeDesc) }
}

/// Dispatch helpers: read the object's descriptor, find its protocol, send.
/// A descriptor with no protocol answers `Unsupported`.
///
/// Every method is unsafe under one contract: `obj` is a live object whose
/// word zero is a `*const TypeDesc`.
pub struct Send;

#[allow(clippy::missing_safety_doc)]
impl Send {
    #[inline]
    unsafe fn proto(obj: *mut u8) -> Option<&'static Protocol> {
        let desc = unsafe { desc_of(obj).as_ref()? };
        unsafe { desc.protocol.as_ref() }
    }

    #[inline]
    pub unsafe fn get_member(obj: *mut u8, name: Symbol) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.get_member) else {
            return Err(Fault::Unsupported);
        };
        let mut out = Value::null();
        reply(unsafe { f(obj, name, &mut out) }, out)
    }

    #[inline]
    pub unsafe fn set_member(obj: *mut u8, name: Symbol, value: Value) -> Result<(), Fault> {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.set_member) else {
            return Err(Fault::Unsupported);
        };
        reply(unsafe { f(obj, name, value) }, Value::null()).map(|_| ())
    }

    /// [`Self::get_member`] through `site`, when the protocol caches.
    #[inline]
    pub unsafe fn get_member_at(obj: *mut u8, name: Symbol, site: &CallSite) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.get_member_at) else {
            return unsafe { Self::get_member(obj, name) };
        };
        let mut out = Value::null();
        reply(
            unsafe {
                f(
                    obj,
                    name,
                    site as *const CallSite as *mut CallSite,
                    &mut out,
                )
            },
            out,
        )
    }

    /// [`Self::set_member`] through `site`, when the protocol caches.
    #[inline]
    pub unsafe fn set_member_at(
        obj: *mut u8,
        name: Symbol,
        site: &CallSite,
        value: Value,
    ) -> Result<(), Fault> {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.set_member_at) else {
            return unsafe { Self::set_member(obj, name, value) };
        };
        reply(
            unsafe { f(obj, name, site as *const CallSite as *mut CallSite, value) },
            Value::null(),
        )
        .map(|_| ())
    }

    /// [`Self::invoke`] through `site`, when the protocol caches.
    #[inline]
    pub unsafe fn invoke_at(obj: *mut u8, name: Symbol, site: &CallSite, args: &[Value]) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.invoke_at) else {
            return unsafe { Self::invoke(obj, name, args) };
        };
        let mut out = Value::null();
        reply(
            unsafe {
                f(
                    obj,
                    name,
                    site as *const CallSite as *mut CallSite,
                    args.as_ptr(),
                    args.len(),
                    &mut out,
                )
            },
            out,
        )
    }

    #[inline]
    pub unsafe fn invoke(obj: *mut u8, name: Symbol, args: &[Value]) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.invoke) else {
            return Err(Fault::Unsupported);
        };
        let mut out = Value::null();
        reply(
            unsafe { f(obj, name, args.as_ptr(), args.len(), &mut out) },
            out,
        )
    }

    #[inline]
    pub unsafe fn call(obj: *mut u8, args: &[Value]) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.call) else {
            return Err(Fault::Unsupported);
        };
        let mut out = Value::null();
        reply(unsafe { f(obj, args.as_ptr(), args.len(), &mut out) }, out)
    }

    pub unsafe fn index(obj: *mut u8, key: Value) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.index) else {
            return Err(Fault::Unsupported);
        };
        let mut out = Value::null();
        reply(unsafe { f(obj, key, &mut out) }, out)
    }

    pub unsafe fn set_index(obj: *mut u8, key: Value, value: Value) -> Result<(), Fault> {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.set_index) else {
            return Err(Fault::Unsupported);
        };
        reply(unsafe { f(obj, key, value) }, Value::null()).map(|_| ())
    }

    /// One step of iteration; `state` starts as `Value::null()`.
    /// `Err(Missing)` when exhausted.
    pub unsafe fn iterate(obj: *mut u8, state: &mut Value) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.iterate) else {
            return Err(Fault::Unsupported);
        };
        let mut out = Value::null();
        reply(unsafe { f(obj, state, &mut out) }, out)
    }

    pub unsafe fn len(obj: *mut u8) -> Result<usize, Fault> {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.len) else {
            return Err(Fault::Unsupported);
        };
        let mut out = 0usize;
        reply(unsafe { f(obj, &mut out) }, Value::null()).map(|_| out)
    }

    pub unsafe fn arity(obj: *mut u8) -> Result<usize, Fault> {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.arity) else {
            return Err(Fault::Unsupported);
        };
        let mut out = 0usize;
        reply(unsafe { f(obj, &mut out) }, Value::null()).map(|_| out)
    }

    pub unsafe fn to_string(obj: *mut u8) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.to_string) else {
            return Err(Fault::Unsupported);
        };
        let mut out = Value::null();
        reply(unsafe { f(obj, &mut out) }, out)
    }

    pub unsafe fn hash(obj: *mut u8) -> Result<u64, Fault> {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.hash) else {
            return Err(Fault::Unsupported);
        };
        let mut out = 0u64;
        reply(unsafe { f(obj, &mut out) }, Value::null()).map(|_| out)
    }

    pub unsafe fn equals(obj: *mut u8, other: Value) -> Result<bool, Fault> {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.equals) else {
            return Err(Fault::Unsupported);
        };
        let mut out = false;
        reply(unsafe { f(obj, other, &mut out) }, Value::null()).map(|_| out)
    }

    pub unsafe fn unwrap_native(obj: *mut u8) -> Result<*mut c_void, Fault> {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.unwrap_native) else {
            return Err(Fault::Unsupported);
        };
        let mut out = core::ptr::null_mut();
        reply(unsafe { f(obj, &mut out) }, Value::null()).map(|_| out)
    }

    pub unsafe fn is_error(obj: *mut u8) -> bool {
        unsafe { Self::proto(obj) }
            .and_then(|p| p.is_error)
            .is_some_and(|f| unsafe { f(obj) })
    }

    pub unsafe fn error_kind(obj: *mut u8) -> Option<ErrorKind> {
        unsafe { Self::proto(obj) }
            .and_then(|p| p.error_kind)
            .map(|f| unsafe { f(obj) })
    }

    pub unsafe fn error_message(obj: *mut u8) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.error_message) else {
            return Err(Fault::Unsupported);
        };
        let mut out = Value::null();
        reply(unsafe { f(obj, &mut out) }, out)
    }

    pub unsafe fn error_cause(obj: *mut u8) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.error_cause) else {
            return Err(Fault::Unsupported);
        };
        let mut out = Value::null();
        reply(unsafe { f(obj, &mut out) }, out)
    }

    pub unsafe fn error_trace(obj: *mut u8) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.error_trace) else {
            return Err(Fault::Unsupported);
        };
        let mut out = Value::null();
        reply(unsafe { f(obj, &mut out) }, out)
    }

    pub unsafe fn type_name(obj: *mut u8) -> Reply {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.type_name) else {
            return Err(Fault::Unsupported);
        };
        let mut out = Value::null();
        reply(unsafe { f(obj, &mut out) }, out)
    }

    pub unsafe fn shadow(obj: *mut u8, lang: LangId) -> Result<*mut u8, Fault> {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.shadow) else {
            return Err(Fault::Unsupported);
        };
        let mut out = core::ptr::null_mut();
        reply(unsafe { f(obj, lang, &mut out) }, Value::null()).map(|_| out)
    }

    /// `Err(Fault::Missing)` comes with the shadow already kept, in `out`.
    pub unsafe fn keep_shadow(
        obj: *mut u8,
        shadow: *mut u8,
        out: &mut *mut u8,
    ) -> Result<(), Fault> {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.keep_shadow) else {
            return Err(Fault::Unsupported);
        };
        reply(unsafe { f(obj, shadow, out) }, Value::null()).map(|_| ())
    }

    pub unsafe fn drop_shadow(obj: *mut u8, shadow: *mut u8) -> Result<(), Fault> {
        let Some(f) = unsafe { Self::proto(obj) }.and_then(|p| p.drop_shadow) else {
            return Err(Fault::Unsupported);
        };
        reply(unsafe { f(obj, shadow) }, Value::null()).map(|_| ())
    }
}

/// What the bridge invokes.
#[derive(Clone, Copy, Debug)]
pub enum Callable {
    /// A C function with a HashLink-shaped signature: raw scalars and
    /// pointers by kind. The dispatcher registered for `lang` marshals
    /// `Value`s in and out by it; `lang` also names the segment in the trace.
    Typed {
        func: *const c_void,
        signature: *const caribou_abi::hl::hl_type,
        lang: LangId,
    },
    /// A typed function reached through a cell holding its current code
    /// pointer, read at each call: a function a runtime may replace, by
    /// promoting it to a tier or by reloading it. Dispatched as `Typed`
    /// with what the cell holds.
    Cell {
        cell: *const *const c_void,
        signature: *const caribou_abi::hl::hl_type,
        lang: LangId,
    },
    /// An object that answers `call`. Its language is its descriptor's.
    Dynamic(Value),
    /// A method of a Wren class, sent by its Wren signature (`hit(_)`,
    /// `hp`, `hp=(_)`, `new(_)`) through the receiver's protocol: the
    /// first argument for an instance method, `class` for a static or a
    /// constructor. `class` is rooted by its publisher.
    WrenMethod {
        class: Value,
        signature: Symbol,
        is_static: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use caribou_abi::hl::hl_type;
    use core::mem::MaybeUninit;

    #[repr(C)]
    struct Point {
        desc: *const TypeDesc,
        x: f64,
        y: f64,
    }

    unsafe extern "C-unwind" fn point_get(obj: *mut u8, name: Symbol, out: *mut Value) -> u8 {
        let p = unsafe { &*(obj as *const Point) };
        let v = match name.0 {
            1 => p.x,
            2 => p.y,
            _ => return REPLY_MISSING,
        };
        unsafe { *out = Value::number(v) };
        REPLY_OK
    }

    unsafe extern "C-unwind" fn point_len(_obj: *mut u8, out: *mut usize) -> u8 {
        unsafe { *out = 2 };
        REPLY_OK
    }

    static POINT_PROTO: Protocol = Protocol {
        get_member: Some(point_get),
        len: Some(point_len),
        ..Protocol::NONE
    };

    fn point_desc() -> &'static TypeDesc {
        static DESC: std::sync::OnceLock<Box<TypeDesc>> = std::sync::OnceLock::new();
        DESC.get_or_init(|| {
            let hl: hl_type = unsafe { MaybeUninit::zeroed().assume_init() };
            let mut d = TypeDesc::new(hl);
            d.protocol = &POINT_PROTO;
            Box::new(d)
        })
    }

    #[test]
    fn messages_dispatch_through_the_descriptor() {
        let mut p = Point {
            desc: point_desc(),
            x: 3.0,
            y: 4.0,
        };
        let obj = &mut p as *mut Point as *mut u8;
        unsafe {
            assert_eq!(
                Send::get_member(obj, Symbol(1)).unwrap().as_number(),
                Some(3.0)
            );
            assert_eq!(
                Send::get_member(obj, Symbol(2)).unwrap().as_number(),
                Some(4.0)
            );
            assert_eq!(Send::get_member(obj, Symbol(9)), Err(Fault::Missing));
            assert_eq!(Send::len(obj), Ok(2));
            assert_eq!(Send::call(obj, &[]), Err(Fault::Unsupported));
            assert_eq!(Send::to_string(obj), Err(Fault::Unsupported));
            assert_eq!(Send::type_name(obj), Err(Fault::Unsupported));
            assert!(!Send::is_error(obj));
            assert_eq!(Send::error_kind(obj), None);
        }
    }

    #[test]
    fn a_descriptor_without_a_protocol_answers_unsupported() {
        let hl: hl_type = unsafe { MaybeUninit::zeroed().assume_init() };
        let d = Box::leak(Box::new(TypeDesc::new(hl)));
        let mut p = Point {
            desc: d,
            x: 0.0,
            y: 0.0,
        };
        let obj = &mut p as *mut Point as *mut u8;
        assert_eq!(
            unsafe { Send::get_member(obj, Symbol(1)) },
            Err(Fault::Unsupported)
        );
    }
}
