//! Calling native code by a signature known at run time: what a language
//! whose functions are machine code with HashLink-shaped signatures (a
//! plugin's, a Zyntax module's) dispatches a `Callable::Typed` through.
//! The signature is an `hl_type` of kind `HFUN` built from scalar kinds,
//! one object per distinct signature for the process; a `Value` crosses
//! as the word of its kind, and the result comes back the same way,
//! through `ash_native_call`'s generated table. Nothing is boxed.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use caribou_abi::hl::{
    self, hl_type, hl_type_detail, hl_type_fun, hl_type_fun_closure, hl_type_fun_closure_type,
};
use caribou_abi::Value;

use crate::error::{Int64, Str};
use crate::registry::TypeRef;

/// The `hl_type` of a scalar kind, one per kind, for a signature's args.
pub fn kind_type(kind: hl::hl_type_kind) -> *mut hl_type {
    static KINDS: LazyLock<RwLock<HashMap<hl::hl_type_kind, usize>>> =
        LazyLock::new(|| RwLock::new(HashMap::new()));
    if let Some(&t) = KINDS.read().unwrap().get(&kind) {
        return t as *mut hl_type;
    }
    let t = Box::into_raw(Box::new(hl_type {
        kind,
        detail: hl_type_detail {
            abs_name: std::ptr::null(),
        },
        vobj_proto: std::ptr::null_mut(),
        mark_bits: std::ptr::null_mut(),
    }));
    KINDS.write().unwrap().insert(kind, t as usize);
    t
}

/// The `HFUN` `hl_type` for `params` and `ret`, the same object for the
/// same types: what a `Callable::Typed` carries and a dispatcher reads.
pub fn signature(params: &[*const hl_type], ret: *const hl_type) -> *const hl_type {
    static SIGNATURES: LazyLock<RwLock<HashMap<Vec<usize>, usize>>> =
        LazyLock::new(|| RwLock::new(HashMap::new()));
    let key: Vec<usize> = params
        .iter()
        .chain(std::iter::once(&ret))
        .map(|&t| t as usize)
        .collect();
    if let Some(&t) = SIGNATURES.read().unwrap().get(&key) {
        return t as *const hl_type;
    }
    let args: Vec<*mut hl_type> = params.iter().map(|&t| t as *mut hl_type).collect();
    let args = Box::leak(args.into_boxed_slice());
    let fun = Box::into_raw(Box::new(hl_type_fun {
        args: args.as_mut_ptr(),
        ret: ret as *mut hl_type,
        nargs: params.len() as i32,
        parent: std::ptr::null_mut(),
        closure_type: hl_type_fun_closure_type {
            kind: hl::HVOID,
            p: std::ptr::null_mut(),
        },
        closure: hl_type_fun_closure {
            args: std::ptr::null_mut(),
            ret: std::ptr::null_mut(),
            nargs: 0,
            parent: std::ptr::null_mut(),
        },
    }));
    let t = Box::into_raw(Box::new(hl_type {
        kind: hl::HFUN,
        detail: hl_type_detail { fun },
        vobj_proto: std::ptr::null_mut(),
        mark_bits: std::ptr::null_mut(),
    }));
    SIGNATURES.write().unwrap().insert(key, t as usize);
    t
}

/// The argument types and the result type of an `HFUN` signature.
///
/// # Safety
/// `sig` is a signature [`signature`] made.
pub unsafe fn parts(sig: *const hl_type) -> (Vec<*const hl_type>, *const hl_type) {
    let fun = unsafe { &*(*sig).detail.fun };
    let args = (0..fun.nargs as usize)
        .map(|i| unsafe { *fun.args.add(i) } as *const hl_type)
        .collect();
    (args, fun.ret as *const hl_type)
}

/// The registry's type for a scalar kind.
pub fn type_ref(kind: hl::hl_type_kind) -> TypeRef {
    match kind {
        hl::HVOID => TypeRef::Void,
        hl::HUI8 | hl::HUI16 | hl::HI32 | hl::HI64 => TypeRef::Int,
        hl::HF32 | hl::HF64 => TypeRef::Float,
        hl::HBOOL => TypeRef::Bool,
        hl::HBYTES => TypeRef::Str,
        _ => TypeRef::Dyn,
    }
}

/// How the table passes a word of `kind`: 0 an integer or pointer, 1 an
/// `f32`, 2 an `f64`.
pub fn word_kind(kind: hl::hl_type_kind) -> u8 {
    match kind {
        hl::HF32 => 1,
        hl::HF64 => 2,
        _ => 0,
    }
}

/// The word for `v` as an argument of `kind`: an integer as itself, a
/// float as its bits, a bool as 0 or 1, a `DYN` as the value's bits, a
/// `BYTES` as the core string's address, borrowed for the call. `None`
/// for a value the kind cannot take.
pub fn word_of(v: Value, kind: hl::hl_type_kind) -> Option<u64> {
    Some(match kind {
        hl::HUI8 | hl::HUI16 | hl::HI32 | hl::HI64 => Int64::of(v)? as u64,
        hl::HF32 => (v.as_number().or_else(|| v.as_int().map(f64::from))? as f32)
            .to_bits()
            .into(),
        hl::HF64 => v
            .as_number()
            .or_else(|| v.as_int().map(f64::from))?
            .to_bits(),
        hl::HBOOL => u64::from(v.as_bool()?),
        hl::HDYN => v.to_bits(),
        hl::HBYTES => (unsafe { Str::from_value(v) }?) as u64,
        _ => return None,
    })
}

/// The value a result word of `kind` is.
pub fn value_of(word: i64, kind: hl::hl_type_kind) -> Value {
    match kind {
        hl::HVOID => Value::null(),
        hl::HUI8 => Value::int(i32::from(word as u8)),
        hl::HUI16 => Value::int(i32::from(word as u16)),
        hl::HI32 => Value::int(word as i32),
        hl::HI64 => Int64::value(word),
        hl::HF32 => Value::number(f64::from(f32::from_bits(word as u32))),
        hl::HF64 => Value::number(f64::from_bits(word as u64)),
        hl::HBOOL => Value::bool(word & 1 != 0),
        hl::HDYN => Value::from_bits(word as u64),
        // A core string made through the host, or none.
        hl::HBYTES if word != 0 => Value::object(word as *const std::ffi::c_void),
        _ => Value::null(),
    }
}

/// Call `func` with `words`, each passed as its `word_kinds` entry says,
/// returning a word of `ret_kind`; `None` when the table has no entry
/// for that shape.
///
/// # Safety
/// `func` takes exactly those arguments by the C convention.
pub unsafe fn call(
    func: *const std::ffi::c_void,
    words: &[u64],
    word_kinds: &[u8],
    ret_kind: hl::hl_type_kind,
) -> Option<i64> {
    let pattern = ash_native_call::pattern_of(word_kinds);
    unsafe {
        ash_native_call::dispatch_by_pattern(
            func as *mut std::ffi::c_void,
            words,
            word_kind(ret_kind),
            pattern,
        )
    }
}

/// A typed dispatcher for a language whose signatures are scalar kinds
/// alone: each argument as the word of its kind, the result read back
/// the same way. A value a kind cannot take is a `Type` error naming
/// the argument; a signature the table does not cover is an error too.
///
/// # Safety
/// As the bridge calls a `TypedDispatch`: `func` takes what `sig` says.
pub unsafe extern "C-unwind" fn dispatch(
    func: *const std::ffi::c_void,
    sig: *const hl_type,
    _site: *mut crate::protocol::CallSite,
    args: *const Value,
    nargs: usize,
    out: *mut Value,
) -> u8 {
    use crate::bridge;
    use crate::error::Error;
    use crate::world::LANG_CORE;
    use caribou_abi::ErrorKind;

    let raise = |message: String| bridge::raise(Error::new(ErrorKind::Type, &message, LANG_CORE));
    let (types, ret_type) = unsafe { parts(sig) };
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
        let Some(word) = word_of(v, kind) else {
            return raise(format!(
                "argument {} of the function cannot be {}",
                i + 1,
                bridge::describe(v)
            ));
        };
        words.push(word);
        word_kinds.push(word_kind(kind));
    }
    let ret_kind = unsafe { (*ret_type).kind };
    let Some(word) = (unsafe { call(func, &words, &word_kinds, ret_kind) }) else {
        return raise("the function's signature is not one the core can call".to_owned());
    };
    if bridge::has_pending() {
        return crate::protocol::REPLY_RAISED;
    }
    unsafe { *out = value_of(word, ret_kind) };
    crate::protocol::REPLY_OK
}
