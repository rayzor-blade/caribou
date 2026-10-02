//! The Zyntax languages' typed dispatch: scalars by kind as the core
//! passes them, a string as Zyntax's own string, allocated as Zyntax
//! allocates its strings for the call, and a result string copied into a
//! core string. A dynamic value crosses as the program's `Any`: a
//! scalar or string as its own, anything else as a foreign object
//! (`foreign`). A value of a kind the core does not pass yet, an object,
//! an array or a function of a Zyntax type, is a `Type` error naming the
//! argument.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, RwLock};

use caribou::bridge;
use caribou::error::{Error, Str};
use caribou::native;
use caribou::protocol::{CallSite, REPLY_MISSING, REPLY_OK, REPLY_RAISED};
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
    !matches!(kind, hl::HOBJ | hl::HARRAY | hl::HFUN)
}

/// What the calls of one signature need, worked out on the first: each
/// argument's kind, the result's, and the pattern the call table
/// dispatches on. Kept for the process, as the signatures are.
struct Plan {
    kinds: Box<[hl::hl_type_kind]>,
    ret: hl::hl_type_kind,
    ret_word: u8,
    pattern: u32,
}

/// Plans by signature address.
static PLANS: LazyLock<RwLock<HashMap<usize, &'static Plan>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// The plan for `sig`, or the error a call of it raises.
fn plan(sig: *const hl_type) -> Result<&'static Plan, String> {
    if let Some(&plan) = PLANS.read().unwrap().get(&(sig as usize)) {
        return Ok(plan);
    }
    let (types, ret_type) = unsafe { native::parts(sig) };
    let kinds: Box<[hl::hl_type_kind]> = types.iter().map(|&t| unsafe { (*t).kind }).collect();
    if let Some(i) = kinds.iter().position(|&kind| !crosses(kind)) {
        return Err(format!(
            "argument {} of the function is of a Zyntax type the core does not pass yet",
            i + 1
        ));
    }
    let ret = unsafe { (*ret_type).kind };
    if !crosses(ret) {
        return Err("the function returns a Zyntax type the core does not pass yet".to_owned());
    }
    if kinds.len() > native::MAX_ARGS {
        return Err("the function's signature is not one the core can call".to_owned());
    }
    let word_kinds: Vec<u8> = kinds.iter().map(|&k| native::word_kind(k)).collect();
    let plan: &'static Plan = Box::leak(Box::new(Plan {
        pattern: native::pattern_of(&word_kinds),
        ret_word: native::word_kind(ret),
        kinds,
        ret,
    }));
    Ok(*PLANS.write().unwrap().entry(sig as usize).or_insert(plan))
}

/// The languages the slots below dispatch for; a free slot holds
/// `LANG_CORE`.
static LANGS: [AtomicU32; 8] = [const { AtomicU32::new(LANG_CORE) }; 8];

/// [`dispatch`] as the code of the language in slot `N`, so the program's
/// calls out name it as their caller.
unsafe extern "C-unwind" fn dispatch_as<const N: usize>(
    func: *const c_void,
    sig: *const hl_type,
    site: *mut CallSite,
    args: *const Value,
    nargs: usize,
    out: *mut Value,
) -> u8 {
    let lang = LANGS[N].load(Ordering::Relaxed);
    unsafe { dispatch_for(lang, func, sig, site, args, nargs, out) }
}

const SLOTS: [bridge::TypedDispatch; 8] = [
    dispatch_as::<0>,
    dispatch_as::<1>,
    dispatch_as::<2>,
    dispatch_as::<3>,
    dispatch_as::<4>,
    dispatch_as::<5>,
    dispatch_as::<6>,
    dispatch_as::<7>,
];

/// The dispatch for `lang`: its slot's, or the plain one when every
/// slot is another language's.
pub fn for_lang(lang: caribou_abi::LangId) -> bridge::TypedDispatch {
    for (slot, held) in LANGS.iter().enumerate() {
        let taken = held.compare_exchange(LANG_CORE, lang, Ordering::Relaxed, Ordering::Relaxed);
        if taken.is_ok() || taken == Err(lang) {
            return SLOTS[slot];
        }
    }
    dispatch
}

pub unsafe extern "C-unwind" fn dispatch(
    func: *const c_void,
    sig: *const hl_type,
    site: *mut CallSite,
    args: *const Value,
    nargs: usize,
    out: *mut Value,
) -> u8 {
    unsafe { dispatch_for(LANG_CORE, func, sig, site, args, nargs, out) }
}

/// A call of `func` as `sig` says, by the code of `lang`; `LANG_CORE`
/// for a language that has no slot. A site the caller keeps is left a
/// direct send, so its next call skips the bridge's typed path.
unsafe fn dispatch_for(
    lang: caribou_abi::LangId,
    func: *const c_void,
    sig: *const hl_type,
    site: *mut CallSite,
    args: *const Value,
    nargs: usize,
    out: *mut Value,
) -> u8 {
    let plan = match plan(sig) {
        Ok(plan) => plan,
        Err(message) => return raise(message),
    };
    if plan.kinds.len() != nargs {
        return raise(format!(
            "the function takes {} arguments, not {nargs}",
            plan.kinds.len()
        ));
    }
    if let Some(site) = unsafe { site.as_ref() } {
        site.set_direct(
            direct,
            func as usize,
            plan as *const Plan as usize,
            lang as usize,
        );
    }
    unsafe { call_as(lang, plan, func, args, out) }
}

/// The direct send a site keeps: the plan and language the first call
/// left, for the function it was made for.
unsafe extern "C-unwind" fn direct(
    site: *const CallSite,
    target: usize,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let (key, plan, lang) = unsafe { (*site).words() };
    let plan = unsafe { &*(plan as *const Plan) };
    if key != target || plan.kinds.len() != n {
        return REPLY_MISSING;
    }
    unsafe {
        call_as(
            lang as caribou_abi::LangId,
            plan,
            target as *const c_void,
            args,
            out,
        )
    }
}

unsafe fn call_as(
    lang: caribou_abi::LangId,
    plan: &Plan,
    func: *const c_void,
    args: *const Value,
    out: *mut Value,
) -> u8 {
    if lang == LANG_CORE {
        unsafe { call(plan, func, args, out) }
    } else {
        crate::foreign::as_caller(lang, || unsafe { call(plan, func, args, out) })
    }
}

/// Each argument as the word its kind passes, the call, and its result
/// back as a value.
unsafe fn call(plan: &Plan, func: *const c_void, args: *const Value, out: *mut Value) -> u8 {
    let n = plan.kinds.len();
    let mut words = [0u64; native::MAX_ARGS];
    for (i, &kind) in plan.kinds.iter().enumerate() {
        let v = unsafe { *args.add(i) };
        words[i] = if kind == hl::HDYN {
            crate::foreign::any_of(v) as u64
        } else if kind == hl::HBYTES {
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
    }
    let Some(word) =
        (unsafe { native::call_pattern(func, &words[..n], plan.ret_word, plan.pattern) })
    else {
        return raise("the function's signature is not one the core can call".to_owned());
    };
    if bridge::has_pending() {
        return REPLY_RAISED;
    }
    let result = if plan.ret == hl::HDYN {
        match unsafe { crate::foreign::value_of(word as zyntax_embed::foreign::Any) } {
            Ok((v, _)) => v,
            Err(e) => return raise(e.message),
        }
    } else if plan.ret == hl::HBYTES {
        match unsafe { text_of(word as *const c_void) } {
            Some(text) => Str::value(Str::new(&text)),
            None => Value::null(),
        }
    } else {
        native::value_of(word, plan.ret)
    };
    unsafe { *out = result };
    REPLY_OK
}
