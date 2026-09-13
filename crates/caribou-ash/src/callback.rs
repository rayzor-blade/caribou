//! A function of another language as a Haxe function value.
//!
//! A value that answers `arity` crosses into Haxe as a closure Haxe can
//! call, pass and keep like any of its own: `Reflect.isFunction` says so,
//! `cb(x)` calls it, and a parameter declared `Int -> Void` takes it, the
//! runtime wrapping it to the declared type as it does for any closure.
//! It is the var-args closure `Reflect.makeVarArgs` makes, over an inner
//! closure bound to the function's ref (`wrenref.rs`); ash unpacks a call
//! into the inner closure's array, and the entry here sends the arguments
//! to the function through the bridge. The ref keeps the function alive
//! for as long as Haxe keeps the closure, whose bound value the scan sees,
//! and the closure going back to the function's language is the function
//! itself (`behind`). A result the function gives that Haxe has no form
//! for is null.

use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;

use ash_std::error::hlp_throw;
use ash_std::fun::{fun_var_args, hlp_alloc_closure_ptr, hlp_make_var_args};
use ash_std::types::{hlt_array, hlt_dyn};
use caribou::bridge;
use caribou::heap;
use caribou::protocol::Callable;
use caribou_abi::Value;
use caribou_abi::hl::{
    self, hl_type, hl_type_detail, hl_type_fun, hl_type_fun_closure, hl_type_fun_closure_type,
    varray, vclosure, vdynamic,
};

use crate::proto::{self, lang};
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

/// The foreign function a Haxe closure stands for, when it is one
/// `function_for` made: the var-args closure over the inner closure over
/// the ref.
pub(crate) unsafe fn behind(d: *mut vdynamic) -> Option<Value> {
    let outer = d as *mut vclosure;
    if unsafe { (*outer).fun } != unsafe { fun_var_args } as *mut c_void
        || unsafe { (*outer).hasValue } == 0
    {
        return None;
    }
    let inner = unsafe { (*outer).value } as *mut vclosure;
    if inner.is_null() || unsafe { (*inner).fun } != entry as *mut c_void {
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

/// The Haxe function value for the function `v` of another language.
pub(crate) fn function_for(v: Value) -> *mut vdynamic {
    let r = wrenref::wrap_foreign(v);
    // The ref is unrooted until the closure holds it.
    let root = heap::handle_new(r.as_object().unwrap() as *mut u8);
    let inner = unsafe {
        hlp_alloc_closure_ptr(
            inner_type().cast(),
            entry as *mut c_void,
            wrenref::wrenref_as_abstract(r),
        )
    };
    let function = unsafe { hlp_make_var_args(inner) };
    heap::handle_release(root);
    function.cast()
}

/// The call: `bound` is the ref, `args` what Haxe passed.
unsafe extern "C" fn entry(bound: *mut vdynamic, args: *mut varray) -> *mut vdynamic {
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
