//! The Zyntax languages' typed dispatch: scalars by kind as the core
//! passes them, a string as Zyntax's own string, allocated as Zyntax
//! allocates its strings for the call, and a result string copied into a
//! core string. A dynamic value crosses as the program's `Any`: a
//! scalar or string as its own, anything else as a foreign object
//! (`foreign`). An object of a class a module publishes crosses as its
//! proxy (`object`). An array, or a function not passed as a dynamic
//! value, is a `Type` error naming the argument. An error the call left
//! pending is raised in the core, as its module describes it.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, RwLock};

use caribou::bridge;
use caribou::error::Error;
use caribou::native;
use caribou::protocol::{CallSite, REPLY_MISSING, REPLY_OK, REPLY_RAISED};
use caribou::world::LANG_CORE;
use caribou_abi::hl::{self, hl_type};
use caribou_abi::{ErrorKind, Value};
use std::sync::OnceLock;

use crate::object::{self, Class, Origin};

fn raise(message: String) -> u8 {
    bridge::raise(Error::new(ErrorKind::Type, &message, LANG_CORE))
}

fn crosses(kind: hl::hl_type_kind, class: Option<&String>) -> bool {
    match kind {
        hl::HOBJ => class.is_some(),
        hl::HARRAY | hl::HFUN | hl::HSTRUCT | hl::HPACKED => false,
        _ => true,
    }
}

/// What the dispatch knows of one published function beyond its kinds:
/// where it came from, and the class of each object it takes, receiver
/// first, and of the object it returns.
pub struct Callee {
    pub origin: Origin,
    pub params: Vec<Option<String>>,
    pub ret: Option<String>,
    /// Whether a call can leave an error pending, so the dispatch looks.
    pub may_raise: bool,
    /// For an async function, the kind of what its future settles with:
    /// a call returns the future, and the function runs on a task.
    pub settles: Option<hl::hl_type_kind>,
}

/// Callees by the address of the signature each was published with.
static CALLEES: LazyLock<RwLock<HashMap<usize, &'static Callee>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Know the function published with `signature`, a signature of its own,
/// as `callee`.
pub fn register(signature: *const hl_type, callee: Callee) {
    let callee: &'static Callee = Box::leak(Box::new(callee));
    CALLEES.write().unwrap().insert(signature as usize, callee);
}

/// What the calls of one signature need, worked out on the first: each
/// argument's kind, the result's, and the pattern the call table
/// dispatches on. Kept for the process, as the signatures are.
struct Plan {
    kinds: Box<[hl::hl_type_kind]>,
    /// The result's kind; an async function's settled result's.
    ret: hl::hl_type_kind,
    /// Whether the call returns a promise the result settles from.
    settles: bool,
    ret_word: u8,
    pattern: u32,
    callee: Option<&'static Callee>,
    /// The class each object parameter must be, as its type name.
    param_classes: Box<[Option<caribou::symbol::Symbol>]>,
    /// The class of the object the function returns, found on the first
    /// call that returns one: its module publishes it after the function.
    ret_class: OnceLock<Option<&'static Class>>,
}

impl Plan {
    fn param_class(&self, i: usize) -> Option<caribou::symbol::Symbol> {
        self.param_classes.get(i).copied().flatten()
    }

    fn ret_class(&self) -> Option<&'static Class> {
        *self.ret_class.get_or_init(|| {
            let callee = self.callee?;
            object::class(callee.origin.lang, callee.ret.as_deref()?)
        })
    }
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
    let callee = CALLEES.read().unwrap().get(&(sig as usize)).copied();
    let kinds: Box<[hl::hl_type_kind]> = types.iter().map(|&t| unsafe { (*t).kind }).collect();
    let param_class = |i: usize| callee.and_then(|c| c.params.get(i)?.as_ref());
    if let Some(i) = (0..kinds.len()).find(|&i| !crosses(kinds[i], param_class(i))) {
        return Err(format!(
            "argument {} of the function is of a Zyntax type the core does not pass yet",
            i + 1
        ));
    }
    let settles = callee.and_then(|c| c.settles);
    let ret = settles.unwrap_or(unsafe { (*ret_type).kind });
    if !crosses(ret, callee.and_then(|c| c.ret.as_ref())) {
        return Err("the function returns a Zyntax type the core does not pass yet".to_owned());
    }
    if kinds.len() > native::MAX_ARGS {
        return Err("the function's signature is not one the core can call".to_owned());
    }
    let n_params = kinds.len();
    let word_kinds: Vec<u8> = kinds.iter().map(|&k| native::word_kind(k)).collect();
    let plan: &'static Plan = Box::leak(Box::new(Plan {
        pattern: native::pattern_of(&word_kinds),
        // A promise is a pointer word.
        ret_word: if settles.is_some() {
            native::word_kind(hl::HBYTES)
        } else {
            native::word_kind(ret)
        },
        kinds,
        ret,
        settles: settles.is_some(),
        param_classes: (0..n_params)
            .map(|i| param_class(i).map(|name| caribou::symbol::intern(name)))
            .collect(),
        callee,
        ret_class: OnceLock::new(),
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
/// back as a value, or the error it left pending raised.
unsafe fn call(plan: &Plan, func: *const c_void, args: *const Value, out: *mut Value) -> u8 {
    let n = plan.kinds.len();
    let mut words = [0u64; native::MAX_ARGS];
    for (i, &kind) in plan.kinds.iter().enumerate() {
        let v = unsafe { *args.add(i) };
        words[i] = match object::word_in(v, kind, plan.param_class(i)) {
            Ok(word) => word,
            Err(m) => return raise(format!("argument {} of the function {m}", i + 1)),
        };
    }
    let Some(word) =
        (unsafe { native::call_pattern(func, &words[..n], plan.ret_word, plan.pattern) })
    else {
        return raise("the function's signature is not one the core can call".to_owned());
    };
    let origin = plan.callee.map(|c| &c.origin);
    let raised = plan
        .callee
        .filter(|c| c.may_raise)
        .and_then(|c| c.origin.take_error());
    if let Some(error) = raised {
        bridge::set_pending(error);
        return REPLY_RAISED;
    }
    if bridge::has_pending() {
        return REPLY_RAISED;
    }
    if plan.settles
        && let Some(callee) = plan.callee
    {
        let result = crate::task::Result {
            kind: plan.ret,
            class: plan.ret_class(),
            origin: &callee.origin,
            may_raise: callee.may_raise,
        };
        unsafe { *out = crate::task::start(word as *mut u8, callee.origin.lang, result) };
        return REPLY_OK;
    }
    match unsafe { object::value_out(word as u64, plan.ret, plan.ret_class(), origin) } {
        Ok(result) => {
            unsafe { *out = result };
            REPLY_OK
        }
        Err(m) => raise(m),
    }
}
