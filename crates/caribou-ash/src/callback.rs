//! A function of another language as a Haxe function value.
//!
//! A value that answers `arity` crosses into Haxe as a closure Haxe can
//! call, pass and keep like any of its own: `Reflect.isFunction` says so,
//! `cb(x)` calls it, and a parameter declared `Int -> Void` takes it, the
//! runtime wrapping it to the declared type as it does for any closure.
//!
//! Where the declaration names the function's type (`Fn(Num) -> Num` in
//! an export, a Haxe `Float -> Float`), the closure is one of that type:
//! Ash's record closure, whose bound value is a `Callback` here, holding
//! the function and the signature's shape. Compiled Haxe calls it as it
//! calls any closure of the type, Ash places the arguments as one word
//! each and calls `entry`, which reads them by kind, sends them through
//! the bridge, and answers with the result as one word. Nothing is boxed
//! on either side.
//!
//! Where the type is not known, or does not fit the registers a record
//! closure takes, it is the var-args closure `Reflect.makeVarArgs` makes,
//! over an inner closure bound to the function's ref (`wrenref.rs`); ash
//! unpacks a call into the inner closure's array, and `var_entry` sends
//! the arguments to the function through the bridge.
//!
//! Either way the closure's bound value keeps the function alive for as
//! long as Haxe keeps the closure, and the closure going back to the
//! function's language is the function itself (`behind`). A result the
//! function gives that Haxe has no form for is null.

use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;

use std::mem::MaybeUninit;
use std::sync::Mutex;

use ash_std::error::hlp_throw;
use ash_std::fun::{
    RecordClosure, fun_record, fun_var_args, hlp_alloc_closure_ptr, hlp_alloc_record_closure,
    hlp_make_var_args,
};
use ash_std::types::{hlt_array, hlt_dyn};
use caribou::bridge;
use caribou::heap::{self, Tracer, TypeDesc};
use caribou::protocol::{Callable, desc_of};
use caribou_abi::Value;
use caribou_abi::hl::{
    self, hl_type, hl_type_detail, hl_type_fun, hl_type_fun_closure, hl_type_fun_closure_type,
    hl_type_kind, varray, vclosure, vdynamic,
};
use caribou_abi::mem::{KIND_DYNAMIC, TRACED};

use crate::import::{value_to_word, word_to_value};
use crate::proto::{self, haxe_type, lang};
use crate::wrenref;

/// Arguments a call takes: `hlp_dyn_call`'s limit.
const MAX_ARGS: usize = 9;

struct InnerType(*mut hl_type);
unsafe impl Send for InnerType {}
unsafe impl Sync for InnerType {}

/// The inner closure's full type, `(bound, args: Array<Dynamic>) ->
/// Dynamic`; ash derives the bound-less closure type from it on first
/// use.
fn inner_type() -> *mut hl_type {
    static TYPE: OnceLock<InnerType> = OnceLock::new();
    TYPE.get_or_init(|| {
        let args = Box::leak(Box::new([
            hlt_dyn().cast::<hl_type>(),
            hlt_array().cast::<hl_type>(),
        ]));
        let fun = Box::leak(Box::new(hl_type_fun {
            args: args.as_mut_ptr(),
            ret: hlt_dyn().cast(),
            nargs: 2,
            parent: ptr::null_mut(),
            closure_type: hl_type_fun_closure_type {
                kind: hl::HVOID,
                p: ptr::null_mut(),
            },
            closure: hl_type_fun_closure {
                args: ptr::null_mut(),
                ret: ptr::null_mut(),
                nargs: 0,
                parent: ptr::null_mut(),
            },
        }));
        InnerType(Box::leak(Box::new(hl_type {
            kind: hl::HFUN,
            detail: hl_type_detail { fun },
            vobj_proto: ptr::null_mut(),
            mark_bits: ptr::null_mut(),
        })))
    })
    .0
}

/// The foreign function a Haxe closure stands for, when it is one this
/// module made: a record closure over a `Callback`, or the var-args
/// closure over the inner closure over the ref.
pub(crate) unsafe fn behind(d: *mut vdynamic) -> Option<Value> {
    let outer = d as *mut vclosure;
    if unsafe { (*outer).hasValue } == 0 {
        return None;
    }
    if unsafe { (*outer).fun } == unsafe { fun_record } as *mut c_void {
        let cb = unsafe { (*outer).value } as *mut u8;
        if cb.is_null() || !ptr::eq(unsafe { desc_of(cb) }, &raw const CALLBACK_DESC) {
            return None;
        }
        return Some(unsafe { (*(cb as *const Callback)).target });
    }
    if unsafe { (*outer).fun } != unsafe { fun_var_args } as *mut c_void {
        return None;
    }
    let inner = unsafe { (*outer).value } as *mut vclosure;
    if inner.is_null() || unsafe { (*inner).fun } != var_entry as *mut c_void {
        return None;
    }
    let bound = unsafe { (*inner).value };
    if bound.is_null() {
        return None;
    }
    Some(wrenref::unwrap_foreign(unsafe {
        wrenref::wrenref_from_abstract(bound)
    }))
}

// ---------------------------------------------------------------------------
// A closure of a known type
// ---------------------------------------------------------------------------

/// The shape of a function type a callback is made for: the full closure
/// type Ash wants, its bound value first, and the kinds the words are
/// read and written by. Made once per type and kept for the process, as
/// the type is.
struct Shape {
    full: *mut hl_type,
    args: Vec<hl_type_kind>,
    ret: hl_type_kind,
    ret_type: *const hl_type,
    pattern: u32,
    ret_code: u8,
}

unsafe impl Send for Shape {}
unsafe impl Sync for Shape {}

static SHAPES: Mutex<Vec<(usize, &'static Shape)>> = Mutex::new(Vec::new());

/// The shape of the function type `t`, when a record closure takes it.
fn shape_for(t: *const hl_type) -> Option<&'static Shape> {
    let mut shapes = SHAPES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, s)) = shapes.iter().find(|(k, _)| *k == t as usize) {
        return Some(s);
    }
    let fun = unsafe { (*t).detail.fun.as_ref() }?;
    let n = fun.nargs.max(0) as usize;
    if n > MAX_ARGS {
        return None;
    }
    let mut args = Vec::with_capacity(n);
    let mut codes = Vec::with_capacity(n);
    let mut full_args = vec![hlt_dyn().cast::<hl_type>()];
    for i in 0..n {
        let ty = unsafe { *fun.args.add(i) };
        let kind = unsafe { (*ty).kind };
        args.push(kind);
        codes.push(match kind {
            hl::HF64 => 2,
            hl::HF32 => 1,
            _ => 0,
        });
        full_args.push(ty);
    }
    let ret = unsafe { (*fun.ret).kind };
    let full_args = Box::leak(full_args.into_boxed_slice());
    let full_fun = Box::leak(Box::new(hl_type_fun {
        args: full_args.as_mut_ptr(),
        ret: fun.ret,
        nargs: n as i32 + 1,
        parent: ptr::null_mut(),
        closure_type: hl_type_fun_closure_type {
            kind: hl::HVOID,
            p: ptr::null_mut(),
        },
        closure: hl_type_fun_closure {
            args: ptr::null_mut(),
            ret: ptr::null_mut(),
            nargs: 0,
            parent: ptr::null_mut(),
        },
    }));
    let full = Box::leak(Box::new(hl_type {
        kind: hl::HFUN,
        detail: hl_type_detail { fun: full_fun },
        vobj_proto: ptr::null_mut(),
        mark_bits: ptr::null_mut(),
    }));
    let shape: &'static Shape = Box::leak(Box::new(Shape {
        full,
        args,
        ret,
        ret_type: fun.ret,
        pattern: ash_native_call::pattern_of(&codes),
        ret_code: match ret {
            hl::HF64 => 2,
            hl::HF32 => 1,
            _ => 0,
        },
    }));
    shapes.push((t as usize, shape));
    Some(shape)
}

/// What a typed closure is bound to: Ash's record closure, whose first
/// word is this object's descriptor, then the function and its shape.
#[repr(C)]
struct Callback {
    rc: RecordClosure,
    target: Value,
    shape: &'static Shape,
}

unsafe extern "C" fn trace_callback(obj: *mut u8, tracer: *mut Tracer) {
    let cb = unsafe { &*(obj as *const Callback) };
    unsafe { (*tracer).mark_value(cb.target.to_bits()) };
}

static mut CALLBACK_DESC: TypeDesc = {
    let mut d = TypeDesc::new(haxe_type());
    d.trace = Some(trace_callback);
    d.name = "callback".as_ptr();
    d.name_len = "callback".len();
    d
};

pub(crate) fn set_lang(lang: caribou_abi::LangId) {
    unsafe { CALLBACK_DESC.lang = lang };
}

/// The Haxe function value of type `t` for the function `v` of another
/// language: a record closure when the type fits one, else the var-args
/// closure.
pub(crate) fn function_for_typed(v: Value, t: *const hl_type) -> *mut vdynamic {
    let Some(shape) = shape_for(t) else {
        return function_for(v);
    };
    let cb = unsafe {
        heap::alloc_gen(
            &raw mut CALLBACK_DESC as *mut hl_type,
            size_of::<Callback>(),
            KIND_DYNAMIC | TRACED,
        )
    } as *mut Callback;
    if cb.is_null() {
        heap::out_of_memory("a callback");
    }
    unsafe {
        ptr::addr_of_mut!((*cb).rc).write(RecordClosure {
            host: &raw const CALLBACK_DESC as usize,
            entry,
            context: cb as usize,
            pattern: shape.pattern,
            arity: shape.args.len() as u32,
            ret_kind: shape.ret_code,
            full: shape.full.cast(),
        });
        ptr::addr_of_mut!((*cb).target).write(v);
        ptr::addr_of_mut!((*cb).shape).write(shape);
    }
    // The callback is unrooted until the closure holds it: it is on this
    // frame, which the conservative scan sees. The closure is of the
    // program's own type `t`, so a call site declaring it calls directly.
    let closure = unsafe {
        hlp_alloc_record_closure(t as *mut hl_type as *mut _, ptr::addr_of_mut!((*cb).rc))
    };
    if closure.is_null() {
        return function_for(v);
    }
    closure.cast()
}

/// The call of a typed closure: the words by the shape's kinds, the
/// result as one word by its return kind.
unsafe extern "C" fn entry(context: usize, words: *const i64) -> i64 {
    let cb = unsafe { &*(context as *const Callback) };
    let shape = cb.shape;
    let n = shape.args.len();
    // On the stack, where the conservative scan sees them across the call;
    // only the slots in use are written.
    let mut crossed = [MaybeUninit::<Value>::uninit(); MAX_ARGS];
    for (i, slot) in crossed.iter_mut().enumerate().take(n) {
        slot.write(unsafe { word_to_value(*words.add(i), shape.args[i]) });
    }
    let crossed = unsafe { crossed[..n].assume_init_ref() };
    let result = bridge::call_named(Callable::Dynamic(cb.target), crossed, lang(), "callback");
    let thrown = match result {
        Ok(v) => match unsafe { value_to_word(v, shape.ret, shape.ret_type) } {
            Ok(word) => return word,
            // A result Haxe has no form for is null, as for the var-args
            // form, when the type can take one.
            Err(_) if shape.ret_code == 0 && !matches!(shape.ret, hl::HI32 | hl::HBOOL) => {
                return 0;
            }
            Err(m) => proto::throwable(proto::error_value("callback", &m)),
        },
        Err(e) => proto::throwable(e),
    };
    unsafe { hlp_throw(thrown.cast()) };
    std::process::abort()
}

// ---------------------------------------------------------------------------
// A closure of any type
// ---------------------------------------------------------------------------

/// The Haxe function value for the function `v` of another language, of no
/// particular type.
pub(crate) fn function_for(v: Value) -> *mut vdynamic {
    let r = wrenref::wrap_foreign(v);
    // The ref is unrooted until the closure holds it.
    let root = heap::handle_new(r.as_object().unwrap() as *mut u8);
    let inner = unsafe {
        hlp_alloc_closure_ptr(
            inner_type().cast(),
            var_entry as *mut c_void,
            wrenref::wrenref_as_abstract(r),
        )
    };
    let function = unsafe { hlp_make_var_args(inner) };
    heap::handle_release(root);
    function.cast()
}

/// The call: `bound` is the ref, `args` what Haxe passed.
unsafe extern "C" fn var_entry(bound: *mut vdynamic, args: *mut varray) -> *mut vdynamic {
    match unsafe { run(bound, args) } {
        Ok(v) => v,
        Err(thrown) => {
            unsafe { hlp_throw(thrown.cast()) };
            std::process::abort()
        }
    }
}

/// Everything owned here is dropped before the throw.
unsafe fn run(bound: *mut vdynamic, args: *mut varray) -> Result<*mut vdynamic, *mut vdynamic> {
    let function = wrenref::unwrap_foreign(unsafe { wrenref::wrenref_from_abstract(bound.cast()) });
    let n = if args.is_null() {
        0
    } else {
        unsafe { (*args).size }.max(0) as usize
    };
    if n > MAX_ARGS {
        return Err(proto::throwable(proto::error_value(
            "callback",
            &format!("a call takes at most {MAX_ARGS} arguments, not {n}"),
        )));
    }
    // On the stack, where the conservative scan sees them across the call.
    let mut crossed = [Value::null(); MAX_ARGS];
    let items: *mut *mut vdynamic = unsafe { hl::aptr(args) };
    for (i, slot) in crossed.iter_mut().enumerate().take(n) {
        *slot = unsafe { proto::dyn_to_value(*items.add(i)) };
    }
    let result = bridge::call_named(
        Callable::Dynamic(function),
        &crossed[..n],
        lang(),
        "callback",
    );
    match result {
        // A result Haxe has no form for is null: a callback's last
        // expression is often not meant for the caller at all.
        Ok(v) => Ok(unsafe { proto::value_to_dyn(v, hl::HDYN) }.unwrap_or(ptr::null_mut())),
        Err(e) => Err(proto::throwable(e)),
    }
}
