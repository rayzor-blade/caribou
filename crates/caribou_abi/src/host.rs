//! What a plugin reaches of the core: a table of functions the core hands
//! to `caribou_plugin_entry`, and the types over it a plugin's signatures
//! use. A plugin keeps a core value across calls as a [`Kept`], makes and
//! reads strings as [`Text`], calls a value it was given, and raises.

use core::ffi::c_void;
use core::ops::Deref;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::{ErrorKind, Value};

/// The core's table. Every function is called on the thread the plugin's
/// own function was called on, while that call is in progress.
#[repr(C)]
pub struct Host {
    /// A core string of `len` bytes of UTF-8 at `ptr`, copied; unrooted,
    /// so the plugin returns or keeps it before calling the core again.
    pub text_new: unsafe extern "C" fn(*const u8, usize) -> Text,
    /// `v` as a text when it is a core string, else the null text.
    pub text_of: unsafe extern "C" fn(Value) -> Text,
    /// Root `v` until `release`: a handle the collector honours. Zero for
    /// a value that is not an object, which needs no root.
    pub keep: unsafe extern "C" fn(Value) -> u32,
    /// The value behind a handle `keep` gave.
    pub kept: unsafe extern "C" fn(u32) -> Value,
    pub release: unsafe extern "C" fn(u32),
    /// Call `f` with `args`. Zero: `out` is the result. Anything else: the
    /// call raised, and `out` is the error value.
    pub call: unsafe extern "C" fn(Value, *const Value, usize, *mut Value) -> u8,
    /// Make an error of `kind` with the message pending: the plugin
    /// function then returns, its result ignored, and the caller sees the
    /// error.
    pub raise: unsafe extern "C" fn(ErrorKind, *const u8, usize),
    /// Make an error value pending, as `call` handed it back.
    pub raise_value: unsafe extern "C" fn(Value),
}

static HOST: AtomicPtr<Host> = AtomicPtr::new(core::ptr::null_mut());

/// Keep the table the core handed the entry. [`plugin!`](crate::plugin)
/// calls this.
pub fn install(host: *const Host) {
    HOST.store(host as *mut Host, Ordering::Release);
}

fn host() -> &'static Host {
    let p = HOST.load(Ordering::Acquire);
    assert!(!p.is_null(), "no core has loaded this plugin");
    unsafe { &*p }
}

/// A core string of `s`, for a plugin to return or keep.
pub fn text(s: &str) -> Text {
    unsafe { (host().text_new)(s.as_ptr(), s.len()) }
}

/// Call `f`, a function or anything else that answers a call, with
/// `args`. `Err` is the error value the call raised, which the plugin
/// handles or hands on with [`raise_value`].
pub fn call(f: Value, args: &[Value]) -> Result<Value, Value> {
    let mut out = Value::null();
    let reply = unsafe { (host().call)(f, args.as_ptr(), args.len(), &mut out) };
    if reply == 0 { Ok(out) } else { Err(out) }
}

/// Raise an error of `kind` from the plugin function in progress. The
/// function returns after this; what it returns is ignored.
pub fn raise(kind: ErrorKind, message: &str) {
    unsafe { (host().raise)(kind, message.as_ptr(), message.len()) }
}

/// Raise `err`, an error value a call handed back, on to the caller.
pub fn raise_value(err: Value) {
    unsafe { (host().raise_value)(err) }
}

/// The header of a core string as a plugin sees it: the core's own word,
/// the length in bytes, and the UTF-8 following. A plugin never writes
/// one; the core lays them out.
#[repr(C)]
pub struct TextData {
    pub core: *const c_void,
    pub len: usize,
}

/// A core string in a signature: a parameter borrowed for the call, or a
/// result made by [`text`]. One word, so it crosses as any pointer does.
/// Reads as a `str`.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Text(*const TextData);

impl Text {
    pub const NULL: Text = Text(core::ptr::null());

    /// A core string of `s`: [`text`].
    pub fn new(s: &str) -> Text {
        text(s)
    }

    /// `v` as a text when it is a core string.
    pub fn of(v: Value) -> Option<Text> {
        let t = unsafe { (host().text_of)(v) };
        if t.0.is_null() { None } else { Some(t) }
    }

    /// The core's constructor: a text over a header the core laid out.
    pub const unsafe fn from_raw(p: *const TextData) -> Text {
        Text(p)
    }

    pub fn is_null(self) -> bool {
        self.0.is_null()
    }

    /// The string as a value, to keep or to pass on.
    pub fn value(self) -> Value {
        if self.0.is_null() {
            Value::null()
        } else {
            Value::object(self.0 as *const c_void)
        }
    }

    pub fn as_str(&self) -> &str {
        if self.0.is_null() {
            return "";
        }
        unsafe {
            let d = &*self.0;
            let bytes = core::slice::from_raw_parts((self.0 as *const u8).add(size_of::<TextData>()), d.len);
            core::str::from_utf8_unchecked(bytes)
        }
    }
}

impl Deref for Text {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl core::fmt::Debug for Text {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl core::fmt::Display for Text {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A core value a plugin holds across calls: rooted until dropped, so
/// the collector keeps what it refers to. What a plugin stores in its
/// own structures in place of a bare [`Value`], which the collector
/// cannot see there.
pub struct Kept {
    value: Value,
    handle: u32,
}

impl Kept {
    pub fn new(v: Value) -> Kept {
        Kept { value: v, handle: unsafe { (host().keep)(v) } }
    }

    pub fn get(&self) -> Value {
        if self.handle == 0 {
            self.value
        } else {
            unsafe { (host().kept)(self.handle) }
        }
    }
}

impl Drop for Kept {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe { (host().release)(self.handle) }
        }
    }
}

impl core::fmt::Debug for Kept {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.get(), f)
    }
}
