//! WrenLift's compiled modules in a linked program: the VM they run in,
//! started with the program, and the casts a Haxe call to an `#export`
//! member makes at the boundary (docs/architecture/linking.md). A Wren
//! value is NaN-boxed, a `Num` its own `f64` bits; each cast goes directly
//! between Haxe's form and Wren's.

use std::sync::atomic::{AtomicPtr, Ordering};

use caribou_abi::hl::{hl_type, vdynamic};
use wren_lift::capi::{wlift_aot_new_vm, wlift_aot_run_programs};
use wren_lift::runtime::core::as_string;
use wren_lift::runtime::object::{NativeContext, ObjHeader, ObjType};
use wren_lift::runtime::value::Value;
use wren_lift::runtime::vm::VM;

/// The VM the linked modules run in, from program start.
static VM_PTR: AtomicPtr<VM> = AtomicPtr::new(std::ptr::null_mut());

/// Run at program start: a VM for the linked modules, entered on the
/// starting thread for the program's life so the bridge's calls into Wren
/// (a Wren function Haxe holds, say) reach it, and the modules' bodies run
/// in it, in link order. Their status is the program's when nonzero.
pub(crate) fn start() -> i32 {
    let vm = wlift_aot_new_vm();
    VM_PTR.store(vm, Ordering::Release);
    caribou_wren::import::set_function_classes(function_class);
    unsafe { caribou_wren::enter_vm(vm) };
    unsafe { wlift_aot_run_programs(vm) }
}

fn vm() -> Option<&'static mut VM> {
    unsafe { VM_PTR.load(Ordering::Acquire).as_mut() }
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
    let _kept = (!s.is_null()).then(|| caribou::heap::keep(s.cast()));
    match vm() {
        Some(vm) if !s.is_null() => vm
            .new_string(unsafe { caribou_ash::link::string_text(s) })
            .to_bits(),
        _ => Value::null().to_bits(),
    }
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
    let _kept = caribou_wren::keep_value(v);
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
    let message = vm
        .last_error
        .take()
        .unwrap_or_else(|| "runtime error".to_owned());
    caribou_ash::link::raise(&message, caribou_wren::lang());
}

/// A Haxe face as the Wren object it stands for: a receiver, or an object
/// passed.
///
/// # Safety
/// `face` is null or a live Haxe object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_from_haxe_face(face: *mut vdynamic, _t: *mut hl_type) -> u64 {
    // The face's object is what crosses; reaching it allocates nothing.
    let object = unsafe { caribou_ash::link::behind(face) };
    match (vm(), object) {
        (Some(vm), Some(object)) => caribou_wren::to_wren(vm, object)
            .unwrap_or(Value::null())
            .to_bits(),
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
    let v = Value::from_bits(v);
    // Binding only writes the two edges, so the caller's live arguments are
    // enough; no collection can start between them.
    unsafe { caribou_ash::link::bind_attachment(face, caribou_wren::from_wren(v)) }
}

/// A Wren object as its Haxe face, of the program's type `t`: the face
/// already in front of it, else a new one.
///
/// # Safety
/// `t` is the program's type of a face class.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_to_haxe_face(v: u64, t: *mut hl_type) -> *mut vdynamic {
    let v = Value::from_bits(v);
    let _kept = caribou_wren::keep_value(v);
    unsafe { caribou_ash::link::attachment_face(caribou_wren::from_wren(v), t) }
}

/// A Wren function as a Haxe closure of the program's function type `t`;
/// null for null.
///
/// # Safety
/// `t` is one of the program's function types.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_to_haxe_function(v: u64, t: *mut hl_type) -> *mut vdynamic {
    let v = Value::from_bits(v);
    let _kept = caribou_wren::keep_value(v);
    unsafe { caribou_ash::link::function(caribou_wren::from_wren(v), t) }
}

/// A Haxe closure as the Wren function standing for it: the function
/// itself when it came from Wren.
///
/// # Safety
/// `c` is null or a live Haxe closure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_from_haxe_function(
    c: *mut vdynamic,
    _t: *mut hl_type,
) -> u64 {
    // A Wren function Ash wrapped for Haxe goes home as itself.
    if let Some(bound) = unsafe { caribou_ash::link::holds(c) } {
        return unsafe { caribou_wren_held_fn(bound) };
    }
    let _kept = (!c.is_null()).then(|| caribou::heap::keep(c.cast()));
    let function = unsafe { caribou_ash::link::value(c) };
    match vm() {
        Some(vm) => caribou_wren::to_wren(vm, function)
            .unwrap_or(Value::null())
            .to_bits(),
        None => Value::null().to_bits(),
    }
}

/// A Wren typed array as a Haxe `Bytes` of the program's type `t`, over
/// the array's storage; null for anything else.
///
/// # Safety
/// `t` is the program's `haxe.io.Bytes`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_to_haxe_bytes(v: u64, t: *mut hl_type) -> *mut vdynamic {
    let v = Value::from_bits(v);
    let typed_array = v.as_object().is_some_and(|p| {
        let kind = unsafe { (*(p as *const ObjHeader)).obj_type };
        kind == ObjType::TypedArray
    });
    if !typed_array {
        return std::ptr::null_mut();
    }
    let _kept = caribou_wren::keep_value(v);
    let buffer = caribou_wren::from_wren(v);
    let _buffer_kept = caribou::heap::keep_value(buffer);
    match buffer.as_object() {
        Some(p) => unsafe { caribou_ash::link::caribou_haxe_buffer_to_bytes(p as *mut _, t) },
        None => std::ptr::null_mut(),
    }
}

/// A Haxe `Bytes` as Wren sees a buffer: the typed array it came from, or
/// a sequence of its bytes.
///
/// # Safety
/// `b` is null or a live `Bytes` of the program's type `t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_from_haxe_bytes(b: *mut vdynamic, t: *mut hl_type) -> u64 {
    let buffer = unsafe { caribou_ash::link::caribou_haxe_bytes_to_buffer(b, t) };
    if buffer.is_null() {
        return Value::null().to_bits();
    }
    let buffer = caribou_abi::Value::object(buffer.cast());
    let _kept = caribou::heap::keep_value(buffer);
    match vm() {
        Some(vm) => caribou_wren::to_wren(vm, buffer)
            .unwrap_or(Value::null())
            .to_bits(),
        None => Value::null().to_bits(),
    }
}

/// A Haxe object as Wren holds it: an instance of the class the compiled
/// program made for its type, the module `haxe:<type>`, whose members call
/// the Haxe members; null for null.
///
/// # Safety
/// `obj` is null or a live Haxe object of the program's type `t` or a
/// subtype.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_from_haxe_object(
    obj: *mut vdynamic,
    _t: *mut hl_type,
) -> u64 {
    let Some(vm) = vm().filter(|_| !obj.is_null()) else {
        return Value::null().to_bits();
    };
    // An object of one of the program's classes is a core object as
    // itself: its type rules out a string, a buffer and a face.
    let object = caribou_abi::Value::object(obj.cast());
    caribou_wren::object_to_wren(vm, object)
        .unwrap_or(Value::null())
        .to_bits()
}

/// A Wren instance of a class made for a Haxe type as the Haxe object it
/// holds; null for anything else. It reads the instance and allocates
/// nothing, so nothing is rooted.
///
/// # Safety
/// `t` is one of the program's object types.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_to_haxe_object(v: u64, _t: *mut hl_type) -> *mut vdynamic {
    caribou_wren::import::foreign_of(Value::from_bits(v))
        .and_then(|object| object.as_object())
        .map_or(std::ptr::null_mut(), |p| p.cast())
}

/// A Haxe exception thrown in a member Wren called, as the error of the
/// call: the fiber aborts with its message once the call returns.
///
/// # Safety
/// `exc` is null or a live Haxe value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_raise_haxe(exc: *mut vdynamic) {
    let message = unsafe { caribou_ash::link::exception_text(exc) };
    if let Some(vm) = vm() {
        vm.runtime_error(message);
    }
}

/// The class the linked modules have for the Haxe function type `t`, the
/// module `haxe:<type>` a build makes for a function type Wren receives
/// (`caribou-driver`'s `foreign.rs`); its `call` calls the function.
fn function_class(vm: &mut VM, t: usize) -> Option<*mut wren_lift::runtime::object::ObjClass> {
    let ty = unsafe { caribou_ash::link::type_ref(t as *const hl_type) };
    let module = format!("haxe:{}", caribou_ash::link::spell(&ty)?);
    let class = vm.find_imported_var_from("Function", &module)?;
    class.as_object().map(|p| p.cast())
}

/// A Wren function as the bound value of the Haxe closure Ash makes for it
/// (`ash:closure`): its cell, which the closure keeps alive.
#[unsafe(no_mangle)]
pub extern "C" fn caribou_wren_hold_fn(v: u64) -> *mut std::ffi::c_void {
    let v = Value::from_bits(v);
    let _kept = caribou_wren::keep_value(v);
    caribou_ash::link::hold(caribou_wren::from_wren(v))
}

/// The Wren function a closure Ash made keeps in `bound`.
///
/// # Safety
/// `bound` is what [`caribou_wren_hold_fn`] gave.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_wren_held_fn(bound: *mut std::ffi::c_void) -> u64 {
    let function = unsafe { caribou_ash::link::held(bound) };
    match vm() {
        Some(vm) => caribou_wren::to_wren(vm, function)
            .unwrap_or(Value::null())
            .to_bits(),
        None => Value::null().to_bits(),
    }
}
