//! The Zyntax languages' typed dispatch: scalars by kind as the core
//! passes them, a string as Zyntax's own string, allocated as Zyntax
//! allocates its strings for the call, and a result string copied into a
//! core string. A value of a kind the core does not pass yet, an
//! object, an array or a function of a Zyntax type, is a `Type` error
//! naming the argument.

use std::ffi::c_void;

use caribou::bridge;
use caribou::error::{Error, Str};
use caribou::native;
use caribou::protocol::{CallSite, REPLY_OK, REPLY_RAISED};
use caribou::world::LANG_CORE;
use caribou_abi::hl::{self, hl_type};
use caribou_abi::{ErrorKind, Value};
use zyntax_embed::ZyntaxString;

/// A core string as a Zyntax string, allocated as Zyntax's own strings are:
/// the callee may keep it or release it as any string of its own.
fn zyntax_string(text: &str) -> *mut c_void {
    ZyntaxString::from_str(text).into_raw().cast()
}

/// The text of a Zyntax string, read in place; none for a null pointer.
unsafe fn text_of(p: *const c_void) -> Option<String> {
    let string = unsafe { ZyntaxString::from_ptr(p.cast()) }?;
    Some(String::from_utf8_lossy(string.as_bytes()).into_owned())
}

fn raise(message: String) -> u8 {
    bridge::raise(Error::new(ErrorKind::Type, &message, LANG_CORE))
}

fn crosses(kind: hl::hl_type_kind) -> bool {
    !matches!(kind, hl::HOBJ | hl::HARRAY | hl::HFUN | hl::HDYN)
}

pub unsafe extern "C-unwind" fn dispatch(
    func: *const c_void,
    sig: *const hl_type,
    _site: *mut CallSite,
    args: *const Value,
    nargs: usize,
    out: *mut Value,
) -> u8 {
    let (types, ret_type) = unsafe { native::parts(sig) };
    if types.len() != nargs {
        return raise(format!(
            "the function takes {} arguments, not {nargs}",
            types.len()
        ));
    }
    let args = if nargs == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(args, nargs) }
    };
    let mut words = Vec::with_capacity(nargs);
    let mut word_kinds = Vec::with_capacity(nargs);
    for (i, (&v, &t)) in args.iter().zip(&types).enumerate() {
        let kind = unsafe { (*t).kind };
        if !crosses(kind) {
            return raise(format!(
                "argument {} of the function is of a Zyntax type the core does not pass yet",
                i + 1
            ));
        }
        let word = if kind == hl::HBYTES {
            match unsafe { Str::text(v) } {
                Some(text) => zyntax_string(text) as u64,
                None => {
                    return raise(format!(
                        "argument {} of the function must be a string, not {}",
                        i + 1,
                        bridge::describe(v)
                    ));
                }
            }
        } else {
            match native::word_of(v, kind) {
                Some(word) => word,
                None => {
                    return raise(format!(
                        "argument {} of the function cannot be {}",
                        i + 1,
                        bridge::describe(v)
                    ));
                }
            }
        };
        words.push(word);
        word_kinds.push(native::word_kind(kind));
    }
    let ret_kind = unsafe { (*ret_type).kind };
    if !crosses(ret_kind) {
        return raise("the function returns a Zyntax type the core does not pass yet".to_owned());
    }
    let Some(word) = (unsafe { native::call(func, &words, &word_kinds, ret_kind) }) else {
        return raise("the function's signature is not one the core can call".to_owned());
    };
    if bridge::has_pending() {
        return REPLY_RAISED;
    }
    let result = if ret_kind == hl::HBYTES {
        match unsafe { text_of(word as *const c_void) } {
            Some(text) => Str::value(Str::new(&text)),
            None => Value::null(),
        }
    } else {
        native::value_of(word, ret_kind)
    };
    unsafe { *out = result };
    REPLY_OK
}
