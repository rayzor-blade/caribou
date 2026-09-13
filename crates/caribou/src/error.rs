//! The core's own heap objects, all under `LANG_CORE`: `Str`, a UTF-8
//! string; `Trace`, the segments an error crossed; and `Error`, the value
//! every error becomes at a language boundary.
//!
//! Each is a `KIND_DYNAMIC | TRACED` allocation whose word zero is a static
//! `TypeDesc` here, traced precisely through its hook and answering the
//! protocol through its vtable. Everything is reached through raw pointers
//! and `Value`s; nothing here is boxed.
//!
//! Rooting: a NaN-boxed `Value` on the stack is invisible to the
//! conservative scanner, so every object this module creates is held by a
//! handle from its allocation until it is stored into a rooted parent or
//! handed to the caller, and every object it receives and holds across an
//! allocation is rooted first. `Rooted` is that handle.

use core::ffi::c_void;
use core::ptr;
use core::slice;
use std::sync::OnceLock;

use caribou_abi::hl::{self, hl_type, hl_type_detail};
use caribou_abi::mem::{KIND_DYNAMIC, TRACED};
use caribou_abi::{ErrorKind, LangId, Value};

use crate::heap::{self, Handle, TraceFn, Tracer, TypeDesc};
use crate::protocol::{self, Protocol, REPLY_MISSING, REPLY_OK, Symbol};
use crate::world::{LANG_CORE, language_name};

// ---------------------------------------------------------------------------
// Descriptors and allocation
// ---------------------------------------------------------------------------

/// What C sees at word zero of a core object: an abstract with no name.
const fn core_type() -> hl_type {
    hl_type {
        kind: hl::HABSTRACT,
        detail: hl_type_detail {
            abs_name: ptr::null(),
        },
        vobj_proto: ptr::null_mut(),
        mark_bits: ptr::null_mut(),
    }
}

const fn desc(name: &'static str, trace: TraceFn, protocol: &'static Protocol) -> TypeDesc {
    let mut d = TypeDesc::new(core_type());
    d.trace = Some(trace);
    d.protocol = protocol;
    d.name = name.as_ptr();
    d.name_len = name.len();
    d.lang = LANG_CORE;
    d
}

pub static STR_DESC: TypeDesc = desc("caribou.Str", trace_nothing, &STR_PROTO);
pub static TRACE_DESC: TypeDesc = desc("caribou.Trace", trace_trace, &TRACE_PROTO);
pub static ERROR_DESC: TypeDesc = desc("caribou.Error", trace_error, &ERROR_PROTO);

fn desc_ptr(d: &'static TypeDesc) -> *mut hl_type {
    d as *const TypeDesc as *mut hl_type
}

unsafe fn desc_name<'a>(d: *const TypeDesc) -> &'a str {
    let Some(d) = (unsafe { d.as_ref() }) else {
        return "";
    };
    if d.name.is_null() || d.name_len == 0 {
        return "";
    }
    unsafe { core::str::from_utf8_unchecked(slice::from_raw_parts(d.name, d.name_len)) }
}

/// Whether `v` is an object whose word zero is `d`.
unsafe fn is_instance(v: Value, d: &'static TypeDesc) -> Option<*mut u8> {
    let p = v.as_object()?;
    if p.is_null() {
        return None;
    }
    let p = p as *mut u8;
    ptr::eq(unsafe { protocol::desc_of(p) }, d).then_some(p)
}

/// A heap object kept alive by a handle for the scope of this value.
pub(crate) struct Rooted {
    value: Value,
    handle: Handle,
}

impl Rooted {
    /// Root what `value` refers to; a non-object value needs no root.
    pub(crate) fn of(value: Value) -> Rooted {
        let handle = match value.as_object() {
            Some(p) if !p.is_null() => heap::handle_new(p as *mut u8),
            _ => Handle::NULL,
        };
        Rooted { value, handle }
    }

    /// `size` zeroed bytes under `desc`, rooted before the GC lock is
    /// released so no collection can run between the two steps.
    pub(crate) fn alloc(desc: &'static TypeDesc, size: usize) -> Rooted {
        let _lock = heap::gc_guard();
        let p = unsafe { heap::alloc_gen(desc_ptr(desc), size, KIND_DYNAMIC | TRACED) };
        if p.is_null() {
            heap::out_of_memory(unsafe { desc_name(desc) });
        }
        Rooted {
            value: Value::object(p),
            handle: heap::handle_new(p as *mut u8),
        }
    }

    pub(crate) fn value(&self) -> Value {
        self.value
    }

    pub(crate) fn ptr(&self) -> *mut u8 {
        self.value
            .as_object()
            .map_or(ptr::null_mut(), |p| p as *mut u8)
    }

    /// Take over a handle someone else made for `value`.
    pub(crate) fn from_parts(value: Value, handle: Handle) -> Rooted {
        Rooted { value, handle }
    }
}

impl Drop for Rooted {
    fn drop(&mut self) {
        heap::handle_release(self.handle);
    }
}

unsafe extern "C" fn trace_nothing(_obj: *mut u8, _tracer: *mut Tracer) {}

// ---------------------------------------------------------------------------
// Str
// ---------------------------------------------------------------------------

/// A UTF-8 string: the bytes follow the header. Traced with no children,
/// so the bytes are never scanned.
#[repr(C)]
pub struct Str {
    desc: *const TypeDesc,
    len: usize,
}

/// Every unsafe method is unsafe under one contract: a `*const Str` is a
/// live `Str`, an object `Value` refers to a live object with a `TypeDesc`
/// at word zero, and a returned borrow does not outlive the object.
#[allow(clippy::missing_safety_doc)]
impl Str {
    /// The caller must root the result before allocating again.
    pub fn new(s: &str) -> *mut Str {
        Self::new_rooted(s).ptr() as *mut Str
    }

    pub(crate) fn new_rooted(s: &str) -> Rooted {
        let rooted = Rooted::alloc(&STR_DESC, size_of::<Str>() + s.len());
        let p = rooted.ptr() as *mut Str;
        unsafe {
            (*p).len = s.len();
            ptr::copy_nonoverlapping(s.as_ptr(), Self::bytes_ptr(p), s.len());
        }
        rooted
    }

    unsafe fn bytes_ptr(s: *mut Str) -> *mut u8 {
        unsafe { s.add(1) as *mut u8 }
    }

    pub unsafe fn as_str<'a>(s: *const Str) -> &'a str {
        let len = unsafe { (*s).len };
        let bytes = unsafe { slice::from_raw_parts(Self::bytes_ptr(s as *mut Str), len) };
        unsafe { core::str::from_utf8_unchecked(bytes) }
    }

    pub unsafe fn len(s: *const Str) -> usize {
        unsafe { (*s).len }
    }

    pub unsafe fn from_value(v: Value) -> Option<*mut Str> {
        unsafe { is_instance(v, &STR_DESC) }.map(|p| p as *mut Str)
    }

    pub fn value(s: *mut Str) -> Value {
        Value::object(s as *const c_void)
    }

    /// The text of `v` if it is a core string, else `None`.
    pub unsafe fn text<'a>(v: Value) -> Option<&'a str> {
        unsafe { Self::from_value(v).map(|s| Self::as_str(s)) }
    }
}

unsafe extern "C-unwind" fn str_to_string(obj: *mut u8, out: *mut Value) -> u8 {
    unsafe { *out = Value::object(obj as *const c_void) };
    REPLY_OK
}

/// Length in bytes.
unsafe extern "C-unwind" fn str_len(obj: *mut u8, out: *mut usize) -> u8 {
    unsafe { *out = Str::len(obj as *const Str) };
    REPLY_OK
}

unsafe extern "C-unwind" fn str_equals(obj: *mut u8, other: Value, out: *mut bool) -> u8 {
    let mine = unsafe { Str::as_str(obj as *const Str) };
    unsafe { *out = Str::text(other) == Some(mine) };
    REPLY_OK
}

/// FNV-1a over the bytes.
unsafe extern "C-unwind" fn str_hash(obj: *mut u8, out: *mut u64) -> u8 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in unsafe { Str::as_str(obj as *const Str) }.as_bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    unsafe { *out = h };
    REPLY_OK
}

static STR_PROTO: Protocol = Protocol {
    to_string: Some(str_to_string),
    len: Some(str_len),
    equals: Some(str_equals),
    hash: Some(str_hash),
    ..Protocol::NONE
};

// ---------------------------------------------------------------------------
// Trace
// ---------------------------------------------------------------------------

/// One segment of an error's path: the language it crossed, the callable
/// it was in, and where in that callable's source. `name` and `source` are
/// core strings or null; `start..end` is a byte span into the source, valid
/// only when `spanned`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TraceFrame {
    pub lang: LangId,
    pub spanned: bool,
    pub name: Value,
    /// A file path or module name: the id a `diag::SourceLookup` resolves.
    pub source: Value,
    pub start: usize,
    pub end: usize,
}

/// Unsafe under the contract in [`Str`]: the strings are live objects.
#[allow(clippy::missing_safety_doc)]
impl TraceFrame {
    pub unsafe fn name_str<'a>(&self) -> &'a str {
        unsafe { Str::text(self.name) }.unwrap_or("")
    }

    pub unsafe fn source_str<'a>(&self) -> Option<&'a str> {
        unsafe { Str::text(self.source) }
    }

    pub fn span(&self) -> Option<(usize, usize)> {
        self.spanned.then_some((self.start, self.end))
    }
}

/// A list of frames with fixed capacity, oldest first; `cap` frames follow
/// the header. Growing allocates a larger one, so the owner stores whatever
/// `push` returns.
#[repr(C)]
pub struct Trace {
    desc: *const TypeDesc,
    len: usize,
    cap: usize,
}

/// Unsafe under the contract in [`Str`], with `*const Trace` a live
/// `Trace`; a borrow of the frames must not span a push either.
#[allow(clippy::missing_safety_doc)]
impl Trace {
    const FIRST_CAPACITY: usize = 4;

    /// The caller must root the result before allocating again.
    pub fn with_capacity(cap: usize) -> *mut Trace {
        Self::alloc_rooted(cap).ptr() as *mut Trace
    }

    fn alloc_rooted(cap: usize) -> Rooted {
        let cap = cap.max(1);
        let rooted = Rooted::alloc(
            &TRACE_DESC,
            size_of::<Trace>() + cap * size_of::<TraceFrame>(),
        );
        unsafe { (*(rooted.ptr() as *mut Trace)).cap = cap };
        rooted
    }

    unsafe fn frames_ptr(t: *mut Trace) -> *mut TraceFrame {
        unsafe { t.add(1) as *mut TraceFrame }
    }

    pub unsafe fn len(t: *const Trace) -> usize {
        unsafe { (*t).len }
    }

    pub unsafe fn frames<'a>(t: *const Trace) -> &'a [TraceFrame] {
        unsafe { slice::from_raw_parts(Self::frames_ptr(t as *mut Trace), (*t).len) }
    }

    /// Append a frame, returning the trace that now holds the frames: `t`
    /// itself, or a larger copy when `t` was full. `t` must be rooted by the
    /// caller, and the frame's strings too when a copy may be allocated.
    unsafe fn push(t: *mut Trace, frame: TraceFrame) -> Rooted {
        let (len, cap) = unsafe { ((*t).len, (*t).cap) };
        let target = if len < cap {
            // Already rooted by the caller; no handle of its own.
            Rooted::from_parts(Value::object(t as *const c_void), Handle::NULL)
        } else {
            let grown = Self::alloc_rooted(cap * 2);
            let g = grown.ptr() as *mut Trace;
            unsafe {
                ptr::copy_nonoverlapping(Self::frames_ptr(t), Self::frames_ptr(g), len);
                (*g).len = len;
            }
            grown
        };
        let g = target.ptr() as *mut Trace;
        unsafe {
            Self::frames_ptr(g).add(len).write(frame);
            (*g).len = len + 1;
        }
        target
    }

    pub unsafe fn from_value(v: Value) -> Option<*mut Trace> {
        unsafe { is_instance(v, &TRACE_DESC) }.map(|p| p as *mut Trace)
    }

    pub fn value(t: *mut Trace) -> Value {
        Value::object(t as *const c_void)
    }

    /// One line per frame, oldest first: `  at name (source:start..end)
    /// [language]`.
    pub unsafe fn render(t: *const Trace) -> String {
        let mut out = String::new();
        for frame in unsafe { Self::frames(t) } {
            let name = unsafe { frame.name_str() };
            out.push_str("  at ");
            out.push_str(if name.is_empty() { "<callable>" } else { name });
            if let Some(source) = unsafe { frame.source_str() } {
                out.push_str(" (");
                out.push_str(source);
                if let Some((start, end)) = frame.span() {
                    out.push_str(&format!(":{start}..{end}"));
                }
                out.push(')');
            }
            out.push_str(&format!(" [{}]\n", language_name(frame.lang)));
        }
        out
    }
}

unsafe extern "C" fn trace_trace(obj: *mut u8, tracer: *mut Tracer) {
    let tracer = unsafe { &mut *tracer };
    for frame in unsafe { Trace::frames(obj as *const Trace) } {
        tracer.mark_value(frame.name.to_bits());
        tracer.mark_value(frame.source.to_bits());
    }
}

unsafe extern "C-unwind" fn trace_len(obj: *mut u8, out: *mut usize) -> u8 {
    unsafe { *out = Trace::len(obj as *const Trace) };
    REPLY_OK
}

unsafe extern "C-unwind" fn trace_to_string(obj: *mut u8, out: *mut Value) -> u8 {
    let text = unsafe { Trace::render(obj as *const Trace) };
    unsafe { *out = Str::value(Str::new(&text)) };
    REPLY_OK
}

static TRACE_PROTO: Protocol = Protocol {
    len: Some(trace_len),
    to_string: Some(trace_to_string),
    ..Protocol::NONE
};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// The value an error is at a boundary. `native` is the originating
/// language's own error object, kept so a round trip unwraps to it; `trace`
/// gains one frame per boundary crossed.
#[repr(C)]
pub struct Error {
    desc: *const TypeDesc,
    kind: ErrorKind,
    origin: LangId,
    /// A core `Str`, or null.
    message: Value,
    /// Another error, or null.
    cause: Value,
    native: Value,
    /// A core `Trace`, or null before the first frame.
    trace: Value,
}

/// Unsafe under the contract in [`Str`], with `*const Error` a live
/// `Error`.
#[allow(clippy::missing_safety_doc)]
impl Error {
    /// The caller must root the result before allocating again.
    pub fn new(kind: ErrorKind, message: &str, origin: LangId) -> *mut Error {
        Self::new_rooted(kind, message, origin).ptr() as *mut Error
    }

    pub(crate) fn new_rooted(kind: ErrorKind, message: &str, origin: LangId) -> Rooted {
        let rooted = Rooted::alloc(&ERROR_DESC, size_of::<Error>());
        let e = rooted.ptr() as *mut Error;
        // Zeroed memory is `Value(0)`, a number: every field is made null
        // before anything else can allocate.
        unsafe {
            (*e).kind = kind;
            (*e).origin = origin;
            (*e).message = Value::null();
            (*e).cause = Value::null();
            (*e).native = Value::null();
            (*e).trace = Value::null();
        }
        if !message.is_empty() {
            let text = Str::new_rooted(message);
            unsafe { (*e).message = text.value() };
        }
        rooted
    }

    /// A `User` error carrying a language's own error object and nothing
    /// else; the caller keeps `native` alive across the call.
    pub fn with_native(native: Value, origin: LangId) -> *mut Error {
        Self::with_native_rooted(native, origin).ptr() as *mut Error
    }

    pub(crate) fn with_native_rooted(native: Value, origin: LangId) -> Rooted {
        let rooted = Self::new_rooted(ErrorKind::User, "", origin);
        unsafe { (*(rooted.ptr() as *mut Error)).native = native };
        rooted
    }

    pub unsafe fn with_cause(e: *mut Error, cause: Value) -> *mut Error {
        unsafe { (*e).cause = cause };
        e
    }

    pub unsafe fn set_native(e: *mut Error, native: Value) -> *mut Error {
        unsafe { (*e).native = native };
        e
    }

    /// Append the segment the error is leaving: the language, the callable's
    /// name, and if known the source it was in and a byte span into it. `e`
    /// stays rooted here for the allocations the frame needs.
    pub unsafe fn push_frame(
        e: *mut Error,
        lang: LangId,
        name: &str,
        source: Option<&str>,
        span: Option<(usize, usize)>,
    ) {
        let _root = Rooted::of(Value::object(e as *const c_void));
        let name = (!name.is_empty()).then(|| Str::new_rooted(name));
        let source = source.filter(|s| !s.is_empty()).map(Str::new_rooted);
        let (start, end) = span.unwrap_or((0, 0));
        let frame = TraceFrame {
            lang,
            spanned: span.is_some(),
            name: name.as_ref().map_or(Value::null(), Rooted::value),
            source: source.as_ref().map_or(Value::null(), Rooted::value),
            start,
            end,
        };
        let trace = match unsafe { Trace::from_value((*e).trace) } {
            Some(t) => t,
            None => {
                let fresh = Trace::alloc_rooted(Trace::FIRST_CAPACITY);
                unsafe { (*e).trace = fresh.value() };
                fresh.ptr() as *mut Trace
            }
        };
        let grown = unsafe { Trace::push(trace, frame) };
        unsafe { (*e).trace = grown.value() };
    }

    /// `push_frame` for a segment whose source is unknown: what the bridge
    /// records at a boundary.
    pub unsafe fn push_segment(e: *mut Error, lang: LangId, name: &str) {
        unsafe { Self::push_frame(e, lang, name, None, None) }
    }

    pub unsafe fn from_value(v: Value) -> Option<*mut Error> {
        unsafe { is_instance(v, &ERROR_DESC) }.map(|p| p as *mut Error)
    }

    pub fn value(e: *mut Error) -> Value {
        Value::object(e as *const c_void)
    }

    pub unsafe fn kind(e: *const Error) -> ErrorKind {
        unsafe { (*e).kind }
    }

    pub unsafe fn origin(e: *const Error) -> LangId {
        unsafe { (*e).origin }
    }

    pub unsafe fn message(e: *const Error) -> Value {
        unsafe { (*e).message }
    }

    /// The message text; empty when there is none.
    pub unsafe fn message_str<'a>(e: *const Error) -> &'a str {
        unsafe { Str::text((*e).message) }.unwrap_or("")
    }

    pub unsafe fn cause(e: *const Error) -> Value {
        unsafe { (*e).cause }
    }

    pub unsafe fn native(e: *const Error) -> Value {
        unsafe { (*e).native }
    }

    pub unsafe fn trace(e: *const Error) -> Value {
        unsafe { (*e).trace }
    }

    /// The frames so far, oldest first; empty before any boundary.
    pub unsafe fn frames<'a>(e: *const Error) -> &'a [TraceFrame] {
        match unsafe { Trace::from_value((*e).trace) } {
            Some(t) => unsafe { Trace::frames(t) },
            None => &[],
        }
    }

    /// `Kind: message`, then the trace one frame per line.
    pub unsafe fn describe(e: *const Error) -> String {
        let mut out = unsafe { Self::headline(e) };
        if let Some(t) = unsafe { Trace::from_value((*e).trace) } {
            let trace = unsafe { Trace::render(t) };
            if !trace.is_empty() {
                out.push('\n');
                out.push_str(trace.trim_end_matches('\n'));
            }
        }
        out
    }

    unsafe fn headline(e: *const Error) -> String {
        let kind = kind_name(unsafe { (*e).kind });
        let message = unsafe { Self::message_str(e) };
        if message.is_empty() {
            kind.to_owned()
        } else {
            format!("{kind}: {message}")
        }
    }
}

pub fn kind_name(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Runtime => "Runtime",
        ErrorKind::Type => "Type",
        ErrorKind::NullAccess => "NullAccess",
        ErrorKind::Index => "Index",
        ErrorKind::Arithmetic => "Arithmetic",
        ErrorKind::User => "User",
        ErrorKind::Cancelled => "Cancelled",
        ErrorKind::StackOverflow => "StackOverflow",
        ErrorKind::OutOfMemory => "OutOfMemory",
        ErrorKind::Internal => "Internal",
    }
}

unsafe extern "C" fn trace_error(obj: *mut u8, tracer: *mut Tracer) {
    let e = obj as *const Error;
    let tracer = unsafe { &mut *tracer };
    unsafe {
        tracer.mark_value((*e).message.to_bits());
        tracer.mark_value((*e).cause.to_bits());
        tracer.mark_value((*e).native.to_bits());
        tracer.mark_value((*e).trace.to_bits());
    }
}

unsafe extern "C-unwind" fn error_is_error(_obj: *mut u8) -> bool {
    true
}

unsafe extern "C-unwind" fn error_kind(obj: *mut u8) -> ErrorKind {
    unsafe { (*(obj as *const Error)).kind }
}

unsafe extern "C-unwind" fn error_message(obj: *mut u8, out: *mut Value) -> u8 {
    unsafe { *out = (*(obj as *const Error)).message };
    REPLY_OK
}

unsafe extern "C-unwind" fn error_cause(obj: *mut u8, out: *mut Value) -> u8 {
    unsafe { *out = (*(obj as *const Error)).cause };
    REPLY_OK
}

unsafe extern "C-unwind" fn error_trace(obj: *mut u8, out: *mut Value) -> u8 {
    unsafe { *out = (*(obj as *const Error)).trace };
    REPLY_OK
}

unsafe extern "C-unwind" fn error_to_string(obj: *mut u8, out: *mut Value) -> u8 {
    let text = unsafe { Error::headline(obj as *const Error) };
    unsafe { *out = Str::value(Str::new(&text)) };
    REPLY_OK
}

/// The field names, interned once.
struct Members {
    kind: Symbol,
    message: Symbol,
    cause: Symbol,
    native: Symbol,
    origin: Symbol,
    trace: Symbol,
}

fn members() -> &'static Members {
    static MEMBERS: OnceLock<Members> = OnceLock::new();
    MEMBERS.get_or_init(|| Members {
        kind: Symbol::intern("kind"),
        message: Symbol::intern("message"),
        cause: Symbol::intern("cause"),
        native: Symbol::intern("native"),
        origin: Symbol::intern("origin"),
        trace: Symbol::intern("trace"),
    })
}

unsafe extern "C-unwind" fn error_get_member(obj: *mut u8, name: Symbol, out: *mut Value) -> u8 {
    let e = unsafe { &*(obj as *const Error) };
    let m = members();
    let v = match name {
        n if n == m.kind => Value::int(e.kind as i32),
        n if n == m.message => e.message,
        n if n == m.cause => e.cause,
        n if n == m.native => e.native,
        n if n == m.origin => Value::int(e.origin as i32),
        n if n == m.trace => e.trace,
        _ => return REPLY_MISSING,
    };
    unsafe { *out = v };
    REPLY_OK
}

static ERROR_PROTO: Protocol = Protocol {
    get_member: Some(error_get_member),
    to_string: Some(error_to_string),
    is_error: Some(error_is_error),
    error_message: Some(error_message),
    error_kind: Some(error_kind),
    error_cause: Some(error_cause),
    error_trace: Some(error_trace),
    ..Protocol::NONE
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Fault, Send};

    #[test]
    fn layouts_are_what_the_hooks_assume() {
        assert_eq!(size_of::<Str>(), 16);
        assert_eq!(size_of::<Trace>(), 24);
        assert_eq!(size_of::<TraceFrame>(), 40);
        assert_eq!(size_of::<Error>(), 48);
        assert_eq!(core::mem::offset_of!(Error, desc), 0);
        assert_eq!(ERROR_DESC.lang, LANG_CORE);
        assert_eq!(unsafe { desc_name(&ERROR_DESC) }, "caribou.Error");
    }

    /// Tests read objects the protocol hands back unrooted, so each holds
    /// the GC lock: no collection another thread starts can run meanwhile.
    fn locked() -> heap::GcGuard {
        heap::gc_guard()
    }

    #[test]
    fn a_string_round_trips_through_the_heap_and_the_protocol() {
        let _lock = locked();
        let s = Str::new_rooted("héllo");
        let p = s.ptr() as *mut Str;
        unsafe {
            assert_eq!(Str::as_str(p), "héllo");
            assert_eq!(Str::len(p), 6);
            assert_eq!(Str::from_value(s.value()), Some(p));
            assert_eq!(Str::from_value(Value::int(3)), None);
            assert_eq!(Str::from_value(Value::null()), None);
            assert_eq!(Send::len(p as *mut u8), Ok(6));
            assert_eq!(Send::to_string(p as *mut u8), Ok(s.value()));
            let other = Str::new_rooted("héllo");
            assert_eq!(Send::equals(p as *mut u8, other.value()), Ok(true));
            assert_eq!(Send::equals(p as *mut u8, Value::int(1)), Ok(false));
            assert_eq!(Send::hash(p as *mut u8), Send::hash(other.ptr()));
            assert_eq!(Send::call(p as *mut u8, &[]), Err(Fault::Unsupported));
        }
        let empty = Str::new_rooted("");
        assert_eq!(unsafe { Str::as_str(empty.ptr() as *const Str) }, "");
    }

    #[test]
    fn an_error_reads_back_and_collects_frames() {
        let _lock = locked();
        let e = Error::new_rooted(ErrorKind::Index, "out of range", 7);
        let p = e.ptr() as *mut Error;
        unsafe {
            assert_eq!(Error::kind(p), ErrorKind::Index);
            assert_eq!(Error::origin(p), 7);
            assert_eq!(Error::message_str(p), "out of range");
            assert!(Error::cause(p).is_null());
            assert!(Error::native(p).is_null());
            assert!(Error::trace(p).is_null());
            assert!(Error::frames(p).is_empty());
            assert_eq!(Error::describe(p), "Index: out of range");

            let cause = Error::new_rooted(ErrorKind::Runtime, "", 7);
            Error::with_cause(p, cause.value());
            assert_eq!(
                Error::from_value(Error::cause(p)),
                Some(cause.ptr() as *mut Error)
            );
            assert_eq!(Error::describe(cause.ptr() as *const Error), "Runtime");

            Error::push_frame(p, 7, "inner", Some("a.hx"), Some((12, 20)));
            Error::push_segment(p, 2, "outer");
            Error::push_frame(p, 2, "main", Some("m.wren"), None);
            let frames = Error::frames(p);
            assert_eq!(frames.len(), 3);
            assert_eq!(frames[0].lang, 7);
            assert_eq!(frames[0].name_str(), "inner");
            assert_eq!(frames[0].source_str(), Some("a.hx"));
            assert_eq!(frames[0].span(), Some((12, 20)));
            assert_eq!(frames[1].lang, 2);
            assert_eq!(frames[1].name_str(), "outer");
            assert_eq!(frames[1].source_str(), None);
            assert_eq!(frames[1].span(), None);
            assert_eq!(frames[2].source_str(), Some("m.wren"));
            assert_eq!(frames[2].span(), None);
            assert_eq!(
                Error::describe(p),
                "Index: out of range\n  at inner (a.hx:12..20) [lang 7]\n  at outer [lang 2]\n  at main (m.wren) [lang 2]"
            );
            let t = Trace::from_value(Error::trace(p)).unwrap();
            assert_eq!(Send::len(t as *mut u8), Ok(3));

            assert_eq!(Error::from_value(e.value()), Some(p));
            assert!(Error::from_value(cause.value()).is_some());
            assert_eq!(Error::from_value(Error::trace(p)), None);
            assert_eq!(Error::from_value(Value::number(1.5)), None);
            let s = Str::new_rooted("not an error");
            assert_eq!(Error::from_value(s.value()), None);
        }
    }

    #[test]
    fn the_trace_grows_past_its_first_capacity() {
        let _lock = locked();
        let e = Error::new_rooted(ErrorKind::User, "", 1);
        let p = e.ptr() as *mut Error;
        for i in 0..(Trace::FIRST_CAPACITY * 3 + 1) {
            unsafe { Error::push_segment(p, i as LangId, &format!("f{i}")) };
        }
        let frames = unsafe { Error::frames(p) };
        assert_eq!(frames.len(), Trace::FIRST_CAPACITY * 3 + 1);
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!(frame.lang, i as LangId);
            assert_eq!(unsafe { frame.name_str() }, format!("f{i}"));
        }
    }

    #[test]
    fn an_error_answers_the_error_protocol_and_its_members() {
        let _lock = locked();
        let e = Error::new_rooted(ErrorKind::Type, "no", 3);
        let obj = e.ptr();
        unsafe {
            assert!(Send::is_error(obj));
            assert_eq!(Send::error_kind(obj), Some(ErrorKind::Type));
            assert_eq!(Str::text(Send::error_message(obj).unwrap()), Some("no"));
            assert!(Send::error_cause(obj).unwrap().is_null());
            assert!(Send::error_trace(obj).unwrap().is_null());
            assert_eq!(Str::text(Send::to_string(obj).unwrap()), Some("Type: no"));
            assert_eq!(
                Send::get_member(obj, Symbol::intern("kind")),
                Ok(Value::int(ErrorKind::Type as i32))
            );
            assert_eq!(
                Send::get_member(obj, Symbol::intern("origin")),
                Ok(Value::int(3))
            );
            assert_eq!(
                Str::text(Send::get_member(obj, Symbol::intern("message")).unwrap()),
                Some("no")
            );
            assert_eq!(
                Send::get_member(obj, Symbol::intern("nothing")),
                Err(Fault::Missing)
            );
            assert_eq!(Send::call(obj, &[]), Err(Fault::Unsupported));

            let s = Str::new_rooted("plain");
            assert!(!Send::is_error(s.ptr()));
            assert_eq!(Send::error_kind(s.ptr()), None);
        }
    }

    #[test]
    fn an_error_survives_a_collection_while_handled() {
        let e = Error::new_rooted(ErrorKind::Arithmetic, "divide by zero", 4);
        let p = e.ptr() as *mut Error;
        let native = Str::new_rooted("native payload");
        unsafe {
            Error::set_native(p, native.value());
            Error::push_frame(p, 4, "div", Some("m.wren"), Some((9, 14)));
            Error::push_segment(p, 0, "<callable>");
        }
        drop(native);
        // Only the handle in `e` roots the graph now.
        heap::major();
        heap::major();
        let p = e.ptr() as *mut Error;
        unsafe {
            assert_eq!(Error::kind(p), ErrorKind::Arithmetic);
            assert_eq!(Error::message_str(p), "divide by zero");
            assert_eq!(Str::text(Error::native(p)), Some("native payload"));
            let frames = Error::frames(p);
            assert_eq!(frames.len(), 2);
            assert_eq!(frames[0].name_str(), "div");
            assert_eq!(frames[0].source_str(), Some("m.wren"));
            assert_eq!(frames[0].span(), Some((9, 14)));
            assert_eq!(frames[1].name_str(), "<callable>");
            assert_eq!(frames[1].source_str(), None);
        }
    }
}
