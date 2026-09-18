//! The core's table for plugins (`caribou_abi::host::Host`): what
//! `caribou_plugin_entry` is handed. Every entry runs on the caller's
//! thread inside a plugin function the dispatcher is calling.

use std::ffi::c_void;

use caribou::bridge;
use caribou::error::{Error as CoreError, Str};
use caribou::heap::{self, Handle};
use caribou::protocol::Callable;
use caribou::world::LANG_CORE;
use caribou_abi::host::{Host, Text, TextData};
use caribou_abi::{ErrorKind, Value};

// A text is the core string itself: same header, the length where the
// plugin reads it.
const _: () = {
    assert!(size_of::<Str>() == size_of::<TextData>());
    assert!(std::mem::offset_of!(TextData, len) == Str::LEN_OFFSET);
};

pub static HOST: Host = Host {
    text_new,
    text_of,
    keep,
    kept,
    release,
    call,
    raise,
    raise_value,
};

unsafe fn text_at(ptr: *const u8, len: usize) -> &'static str {
    if len == 0 {
        return "";
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(bytes).unwrap_or("")
}

fn text_from(s: *mut Str) -> Text {
    unsafe { Text::from_raw(s as *const TextData) }
}

unsafe extern "C" fn text_new(ptr: *const u8, len: usize) -> Text {
    text_from(Str::new(unsafe { text_at(ptr, len) }))
}

unsafe extern "C" fn text_of(v: Value) -> Text {
    match unsafe { Str::from_value(v) } {
        Some(s) => text_from(s),
        None => Text::NULL,
    }
}

unsafe extern "C" fn keep(v: Value) -> u32 {
    match v.as_object() {
        Some(p) if !p.is_null() => heap::handle_new(p as *mut u8).as_raw(),
        _ => 0,
    }
}

unsafe extern "C" fn kept(h: u32) -> Value {
    let p = heap::handle_get(Handle::from_raw(h));
    if p.is_null() {
        Value::null()
    } else {
        Value::object(p as *const c_void)
    }
}

unsafe extern "C" fn release(h: u32) {
    heap::handle_release(Handle::from_raw(h));
}

unsafe extern "C" fn call(f: Value, args: *const Value, nargs: usize, out: *mut Value) -> u8 {
    let args = if nargs == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, nargs) }
    };
    match bridge::call(Callable::Dynamic(f), args, LANG_CORE) {
        Ok(v) => {
            unsafe { *out = v };
            0
        }
        Err(e) => {
            unsafe { *out = e };
            1
        }
    }
}

unsafe extern "C" fn raise(kind: ErrorKind, ptr: *const u8, len: usize) {
    let message = unsafe { text_at(ptr, len) };
    bridge::set_pending(CoreError::value(CoreError::new(kind, message, LANG_CORE)));
}

unsafe extern "C" fn raise_value(err: Value) {
    bridge::set_pending(err);
}
