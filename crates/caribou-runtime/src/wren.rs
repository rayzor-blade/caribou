//! WrenLift's compiled modules in a linked program: the VM they run in,
//! started with the program, and the casts a Haxe call to an `#export`
//! member makes at the boundary (docs/architecture/linking.md). A Wren
//! value is NaN-boxed, a `Num` its own `f64` bits; each cast goes directly
//! between Haxe's form and Wren's.

use std::sync::atomic::{AtomicPtr, Ordering};

use caribou_abi::hl::{hl_type, vdynamic};
use wren_lift::capi::{wlift_aot_new_vm, wlift_aot_run_programs};
use wren_lift::runtime::core::as_string;
use wren_lift::runtime::value::Value;
use wren_lift::runtime::vm::VM;

/// The VM the linked modules run in, from program start.
static VM_PTR: AtomicPtr<VM> = AtomicPtr::new(std::ptr::null_mut());

/// Run at program start: a VM for the linked modules, and their bodies
/// run in it, in link order. Their status is the program's when nonzero.
pub(crate) fn start() -> i32 {
    let vm = wlift_aot_new_vm();
    VM_PTR.store(vm, Ordering::Release);
    unsafe { wlift_aot_run_programs(vm) }
}

fn vm() -> Option<&'static mut VM> {
    unsafe { VM_PTR.load(Ordering::Acquire).as_mut() }
}

#[unsafe(no_mangle)]
pub extern "C" fn caribou_wren_from_float(x: f64, _t: *mut hl_type) -> u64 {
    Value::num(x).to_bits()
}

#[unsafe(no_mangle)]
pub extern "C" fn caribou_wren_from_int(x: i32, _t: *mut hl_type) -> u64 {
    Value::num(f64::from(x)).to_bits()
}

#[unsafe(no_mangle)]
pub extern "C" fn caribou_wren_from_bool(b: bool, _t: *mut hl_type) -> u64 {
    Value::bool(b).to_bits()
}

/// A Haxe `String` as a Wren string, made in the linked modules' VM.
///
/// # Safety
/// `s` is null or a live `String` object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_from_haxe_string(s: *mut vdynamic, _t: *mut hl_type) -> u64 {
    match vm() {
        Some(vm) if !s.is_null() => vm.new_string(unsafe { caribou_ash::link::string_text(s) }).to_bits(),
        _ => Value::null().to_bits(),
    }
}

/// A Wren `Num` as a Haxe `Float`; anything else, which a declared `Num`
/// never returns, as NaN.
#[unsafe(no_mangle)]
pub extern "C" fn caribou_wren_to_float(v: u64, _t: *mut hl_type) -> f64 {
    Value::from_bits(v).as_num().unwrap_or(f64::NAN)
}

#[unsafe(no_mangle)]
pub extern "C" fn caribou_wren_to_int(v: u64, _t: *mut hl_type) -> i32 {
    Value::from_bits(v).as_num().map_or(0, |n| n as i32)
}

#[unsafe(no_mangle)]
pub extern "C" fn caribou_wren_to_bool(v: u64, _t: *mut hl_type) -> bool {
    Value::from_bits(v) == Value::bool(true)
}

/// A Wren string as a Haxe `String` of the program's type `t`; null for
/// anything else.
///
/// # Safety
/// `t` is the program's `String` type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_to_haxe_string(v: u64, t: *mut hl_type) -> *mut vdynamic {
    let v = Value::from_bits(v);
    if !v.is_string_object() {
        return std::ptr::null_mut();
    }
    unsafe { caribou_ash::link::string(t, as_string(v)) }
}

/// After a call into a linked module: the error it raised, thrown into the
/// Haxe caller.
#[unsafe(no_mangle)]
pub extern "C" fn caribou_wren_raise_pending() {
    let Some(vm) = vm() else {
        return;
    };
    if !vm.has_error {
        return;
    }
    vm.has_error = false;
    let message = vm.last_error.take().unwrap_or_else(|| "runtime error".to_owned());
    caribou_ash::link::raise(&message, caribou_wren::lang());
}

/// A Haxe face as the Wren object it stands for: a receiver, or an object
/// passed.
///
/// # Safety
/// `face` is null or a live Haxe object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_from_haxe_face(face: *mut vdynamic, _t: *mut hl_type) -> u64 {
    let object = unsafe { caribou_ash::link::behind(face) };
    match (vm(), object) {
        (Some(vm), Some(object)) => caribou_wren::to_wren(vm, object).unwrap_or(Value::null()).to_bits(),
        _ => Value::null().to_bits(),
    }
}

/// Bind the face Haxe just constructed to the Wren object the linked
/// constructor made.
///
/// # Safety
/// `face` is a live instance of a face class.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_bind_face(face: *mut vdynamic, v: u64, _t: *mut hl_type) {
    unsafe { caribou_ash::link::bind(face, caribou_wren::from_wren(Value::from_bits(v))) }
}

/// A Wren object as its Haxe face, of the program's type `t`: the face
/// already in front of it, else a new one.
///
/// # Safety
/// `t` is the program's type of a face class.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_to_haxe_face(v: u64, t: *mut hl_type) -> *mut vdynamic {
    unsafe { caribou_ash::link::face(caribou_wren::from_wren(Value::from_bits(v)), t) }
}
