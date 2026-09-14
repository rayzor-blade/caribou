//! The object protocol for Wren objects, and the value conversions under it.
//!
//! A Wren object crosses the bridge as its core address: the start of the
//! prefixed allocation, where word zero is its heap record's descriptor. Every
//! entry here receives that address and adds `PREFIX` to reach wren_lift's
//! object; `from_wren` and `to_wren` translate at every edge, so wren_lift
//! never sees a core address and the core never sees a wren_lift one.
//!
//! Every message is answered through the runtime's own methods, by Wren's
//! signature convention (`name`, `name=(_)`, `name(_,_)`, `[_]`, `count`,
//! `iterate(_)`), so a Wren class answers the protocol exactly as it answers
//! Wren code. A runtime error inside an entry is the VM's pending message;
//! it becomes a core `Error` whose native payload is that message as a core
//! string, and the entry answers `Raised`.
//!
//! Strings cross by value: a Wren string leaving becomes a core `Str`, and
//! a core `Str` entering becomes a Wren string. Every other object crosses
//! by its core address.
//!
//! The entries need the VM. It is the one entered on this thread with
//! `enter_vm` or `with_vm`, falling back to the VM wren_lift itself reports
//! as dispatching.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr;

use wren_lift::intern::SymbolId;

use caribou::bridge;
use caribou::error::{Error, Str};
use caribou::heap;
use caribou::protocol::{
    CallSite, Protocol, REPLY_MISSING, REPLY_OK, REPLY_RAISED, REPLY_UNSUPPORTED, Symbol,
};
use caribou_abi::{ErrorKind, LangId, Value};
use wren_lift::runtime::core::as_string;
use wren_lift::runtime::engine::FuncId;
use wren_lift::runtime::object::{
    Method, NativeContext, ObjClass, ObjClosure, ObjForeign, ObjHeader, ObjInstance, ObjString,
    ObjType,
};
use wren_lift::runtime::value::Value as WValue;
use wren_lift::runtime::vm::{self, VM};

use crate::heap::{PREFIX, WrenHeap, is_wren, record_address, record_at, record_for, wren_lang};

// ---------------------------------------------------------------------------
// The VM the entries use
// ---------------------------------------------------------------------------

thread_local! {
    static VM_HERE: Cell<*mut VM> = const { Cell::new(ptr::null_mut()) };
}

/// Make `vm` the VM the protocol entries use on this thread until
/// [`leave_vm`]; returns the VM entered before it, for `leave_vm`.
///
/// # Safety
/// `vm` must outlive its entry and be used on this thread only; an entry
/// borrows it mutably for the length of one message.
pub unsafe fn enter_vm(vm: *mut VM) -> *mut VM {
    let previous = VM_HERE.with(|cell| cell.replace(vm));
    if !previous.is_null() {
        record_for(unsafe { (*previous).object_class } as *mut u8).set_entered(ptr::null_mut());
    }
    if !vm.is_null() {
        record_for(unsafe { (*vm).object_class } as *mut u8).set_entered(vm);
    }
    previous
}

/// Restore what [`enter_vm`] replaced.
///
/// # Safety
/// `previous` is what the matching `enter_vm` returned.
pub unsafe fn leave_vm(previous: *mut VM) {
    unsafe { enter_vm(previous) };
}

/// Run `f` with `vm` entered. Messages the bridge delivers to Wren objects
/// while `f` runs reach this VM.
pub fn with_vm<R>(vm: &mut VM, f: impl FnOnce(&mut VM) -> R) -> R {
    let previous = unsafe { enter_vm(vm) };
    let result = f(vm);
    unsafe { leave_vm(previous) };
    result
}

/// The VM entered on this thread, else the one wren_lift is dispatching on;
/// null when there is neither.
pub fn current_vm() -> *mut VM {
    let entered = VM_HERE.with(Cell::get);
    if entered.is_null() {
        vm::current_vm_ptr()
    } else {
        entered
    }
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A wren_lift value as a core value: a string as a fresh core `Str`, which
/// the caller roots before allocating again; an instance of a class
/// installed for another language's as the object it stands for; any
/// other object by its core address; every other value bit for bit (the
/// layouts agree on null, the booleans, numbers and the object tag).
pub fn from_wren(v: WValue) -> Value {
    match v.as_object() {
        Some(_) if v.is_string_object() => Str::value(Str::new(as_string(v))),
        Some(p) => match crate::import::foreign_of(v) {
            Some(obj) => obj,
            None => Value::object(p.wrapping_sub(PREFIX) as *const c_void),
        },
        None => Value::from_bits(v.to_bits()),
    }
}

/// A core value as a wren_lift value. An int becomes a number, Wren having
/// no other; a Wren object is translated back; a core `Str` becomes a Wren
/// string; a proxy another language holds one of this VM's objects through
/// becomes that object; any other object of another language becomes an
/// instance of the class installed for its type (see `import`), or `None`
/// when its language publishes none.
pub fn to_wren(vm: &mut VM, v: Value) -> Option<WValue> {
    if let Some(n) = v.as_int() {
        return Some(WValue::num(f64::from(n)));
    }
    let Some(p) = v.as_object() else {
        return Some(WValue::from_bits(v.to_bits()));
    };
    if p.is_null() {
        return Some(WValue::null());
    }
    let p = p as *mut u8;
    if is_wren(p) {
        return Some(WValue::object(p.wrapping_add(PREFIX)));
    }
    if let Some(text) = unsafe { Str::text(v) } {
        let s = vm.alloc_string(text.to_owned());
        return Some(made(vm, s));
    }
    // What the object stands for, when it stands for one: the object of
    // this VM a ref holds, or the native a wrapper holds, which the
    // instance already made for it is found by.
    let native = crate::import::native_of(p);
    if !native.is_null() {
        if let Some(instance) = crate::import::stand_in_for(vm, native) {
            return Some(instance);
        }
        // A native is an object's start; one of this VM's has the record
        // at word zero, where any other object has its own type.
        let rec = record_for(vm.object_class as *mut u8);
        if unsafe { *(native as *const usize) } == rec as *const WrenHeap as usize {
            return Some(WValue::object(native.wrapping_add(PREFIX)));
        }
    }
    crate::import::proxy(vm, v, native)
}

/// [`from_wren`], for a host handing a Wren value to the bridge.
pub fn wrap(v: WValue) -> Value {
    from_wren(v)
}

/// [`to_wren`], for a host taking a bridge value into Wren.
pub fn unwrap(vm: &mut VM, v: Value) -> Option<WValue> {
    to_wren(vm, v)
}

/// wren_lift's object at the core address `obj`.
#[inline(always)]
unsafe fn wren_ptr(obj: *mut u8) -> *mut u8 {
    unsafe { obj.add(PREFIX) }
}

unsafe fn receiver(obj: *mut u8) -> WValue {
    WValue::object(unsafe { wren_ptr(obj) })
}

unsafe fn obj_type(obj: *mut u8) -> ObjType {
    unsafe { (*(wren_ptr(obj) as *const ObjHeader)).obj_type }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A Wren runtime message's place in the core's list. wren_lift keeps one
/// message string for every error, so `Fiber.abort` is not told apart from
/// the runtime's own.
fn kind_of(message: &str) -> ErrorKind {
    if message.starts_with("Null does not implement") {
        ErrorKind::NullAccess
    } else if message.contains("out of bounds") {
        ErrorKind::Index
    } else if message.starts_with("Stack overflow") || message == "stack overflow" {
        ErrorKind::StackOverflow
    } else {
        ErrorKind::Runtime
    }
}

/// Raise `message` as a Wren error: a core `Error` of Wren's language whose
/// native payload is the message as a core string, what `Fiber.try` would
/// answer with.
fn raise_wren(message: String) -> u8 {
    let kind = kind_of(&message);
    let native = Str::new(&message);
    let root = heap::handle_new(native as *mut u8);
    let e = Error::new(kind, &message, wren_lang());
    unsafe { Error::set_native(e, Str::value(native)) };
    heap::handle_release(root);
    bridge::raise(e)
}

fn raise_core(kind: ErrorKind, message: &str) -> u8 {
    bridge::raise(Error::new(kind, message, wren_lang()))
}

// ---------------------------------------------------------------------------
// Guarded runs
// ---------------------------------------------------------------------------

/// What the thread keeps for compiled code across a run, put back when a
/// throw lands in the guard and the frames between are gone.
struct JitMark {
    ctx: wren_lift::codegen::runtime_fns::JitContext,
    roots: usize,
    frames: usize,
    depth: u32,
    disabled: bool,
}

impl JitMark {
    fn take() -> JitMark {
        let j = unsafe { &*wren_lift::codegen::runtime_fns::jit_state() };
        JitMark {
            ctx: j.ctx,
            roots: j.roots.len(),
            frames: j.frames.len(),
            depth: j.depth,
            disabled: j.disabled,
        }
    }

    fn restore(&self, vm: &mut VM) {
        let j = unsafe { &mut *wren_lift::codegen::runtime_fns::jit_state() };
        j.ctx = self.ctx;
        j.roots.truncate(self.roots);
        j.frames.truncate(self.frames);
        j.depth = self.depth;
        j.disabled = self.disabled;
        vm.pending_fiber_action = None;
    }
}

/// Run `body` on `vm` under the bridge's guard: a throw in another
/// language's code the run calls into lands here, below the Wren frames
/// and above the caller's, instead of at every crossing. `Err` is the
/// reply for a throw that landed, the error pending and the thread's
/// JIT state as it was.
fn guarded<R, F: FnOnce(&mut VM) -> R>(vm: &mut VM, body: F) -> Result<R, u8> {
    struct Run<'a, R, F> {
        vm: &'a mut VM,
        body: Option<F>,
        result: Option<R>,
    }
    unsafe extern "C-unwind" fn thunk<R, F: FnOnce(&mut VM) -> R>(ctx: *mut c_void) {
        let run = unsafe { &mut *(ctx as *mut Run<'_, R, F>) };
        if let Some(body) = run.body.take() {
            run.result = Some(body(run.vm));
        }
    }
    let mark = JitMark::take();
    let mut run = Run {
        vm,
        body: Some(body),
        result: None,
    };
    let returned = unsafe {
        bridge::run_guarded(thunk::<R, F>, &mut run as *mut Run<'_, R, F> as *mut c_void)
    };
    if returned {
        return run.result.ok_or(REPLY_RAISED);
    }
    mark.restore(run.vm);
    Err(REPLY_RAISED)
}

/// Enter compiled Wren code from another language, from `site` when the
/// caller keeps one: `bridge::enter` decides whether the run is under
/// the guard, and a throw that lands there leaves the thread's JIT
/// state as it was.
fn entered<R, F: FnOnce(&mut VM) -> R>(
    vm: &mut VM,
    site: Option<&CallSite>,
    body: F,
) -> Result<R, u8> {
    struct Run<'a, R, F> {
        vm: &'a mut VM,
        body: Option<F>,
        result: Option<R>,
    }
    unsafe extern "C-unwind" fn thunk<R, F: FnOnce(&mut VM) -> R>(ctx: *mut c_void) {
        let run = unsafe { &mut *(ctx as *mut Run<'_, R, F>) };
        if let Some(body) = run.body.take() {
            run.result = Some(body(run.vm));
        }
    }
    let mut mark = None;
    let mut run = Run {
        vm,
        body: Some(body),
        result: None,
    };
    let returned = unsafe {
        bridge::enter(
            site,
            || mark = Some(JitMark::take()),
            thunk::<R, F>,
            &mut run as *mut Run<'_, R, F> as *mut c_void,
        )
    };
    if returned {
        return run.result.ok_or(REPLY_RAISED);
    }
    if let Some(mark) = mark {
        mark.restore(run.vm);
    }
    Err(REPLY_RAISED)
}

/// The runtime seam's `run_guarded`: a run of the VM's fiber under the
/// bridge's guard. A throw that lands is raised on the VM as a runtime
/// error, the way one at a crossing is, and the run takes it from there.
pub(crate) unsafe extern "C" fn run_guarded(
    vm: *mut c_void,
    body: unsafe extern "C" fn(*mut c_void),
    ctx: *mut c_void,
) -> bool {
    let vm = unsafe { &mut *(vm as *mut VM) };
    match guarded(vm, |_| unsafe { body(ctx) }) {
        Ok(()) => true,
        Err(_) => {
            let message = match bridge::take_pending() {
                Some(err) => crate::import::message_of(err),
                None => "error caught by the host".to_owned(),
            };
            vm.runtime_error(message);
            false
        }
    }
}

/// The VM's pending error, taken, as a raise; `None` when there is none.
fn take_error(vm: &mut VM) -> Option<u8> {
    if !vm.has_error {
        return None;
    }
    vm.has_error = false;
    let message = vm
        .last_error
        .take()
        .unwrap_or_else(|| "Runtime error.".to_owned());
    Some(raise_wren(message))
}

/// The VM this entry runs on, or the raise to answer with: the VM entered
/// on this thread, which must be the one `obj` belongs to. A value of
/// another VM, or of one that is gone, has no VM to run on. With it, the
/// address of its record, which names the VM to a call site.
fn vm_of(obj: *mut u8) -> Result<(&'static mut VM, usize), u8> {
    // The object's own record says whether its VM is entered here.
    let key = record_address(obj);
    let entered = unsafe { record_at(key) }.entered_here();
    if !entered.is_null() {
        return Ok((unsafe { &mut *entered }, key));
    }
    let vm = vm_here()?;
    let mine = record_for(vm.object_class as *mut u8) as *const WrenHeap as usize;
    if key != mine {
        return Err(raise_core(
            ErrorKind::Runtime,
            "the object belongs to another Wren VM, or to one that is gone",
        ));
    }
    Ok((vm, mine))
}

fn vm_here() -> Result<&'static mut VM, u8> {
    let vm = current_vm();
    if vm.is_null() {
        return Err(raise_core(
            ErrorKind::Internal,
            "no Wren VM is entered on this thread",
        ));
    }
    Ok(unsafe { &mut *vm })
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// The arity a full signature declares: its `_` parameters, `[_]=(_)`
/// counting two.
fn arity_of(sig: &str) -> usize {
    let params = sig.find(['(', '[']).unwrap_or(sig.len());
    sig[params..].matches('_').count()
}

/// Wren's signature for `name` taking `arity` arguments: `name()`,
/// `name(_)`, `name(_,_)`.
fn signature(name: &str, arity: usize) -> String {
    let mut sig = String::with_capacity(name.len() + 2 * arity + 2);
    sig.push_str(name);
    sig.push('(');
    for i in 0..arity {
        if i > 0 {
            sig.push(',');
        }
        sig.push('_');
    }
    sig.push(')');
    sig
}

/// `args` as Wren values, or the raise for the first that cannot cross.
/// Arguments crossed into Wren, with room for a receiver before them. On
/// the stack for what a compiled body takes in registers, else on the
/// heap. Only the slots in use are written; the lead slots hold null until
/// the caller fills them.
struct Args {
    inline: [MaybeUninit<WValue>; Args::INLINE],
    spill: Vec<WValue>,
    len: usize,
}

impl Args {
    const INLINE: usize = 10;

    /// `args` crossed, after `lead` null slots.
    fn cross(vm: &mut VM, lead: usize, args: *const Value, n: usize) -> Result<Args, u8> {
        let args = if n == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(args, n) }
        };
        let len = lead + n;
        let mut out = Args {
            inline: [MaybeUninit::uninit(); Args::INLINE],
            spill: Vec::new(),
            len,
        };
        if len > Args::INLINE {
            out.spill = vec![WValue::null(); len];
        } else {
            for slot in &mut out.inline[..lead] {
                slot.write(WValue::null());
            }
        }
        for (i, &arg) in args.iter().enumerate() {
            let v = cross(vm, arg)?;
            if len > Args::INLINE {
                out.spill[lead + i] = v;
            } else {
                out.inline[lead + i].write(v);
            }
        }
        Ok(out)
    }

    /// Room for a receiver and nothing else.
    fn receiver_only() -> Args {
        let mut inline = [MaybeUninit::uninit(); Args::INLINE];
        inline[0].write(WValue::null());
        Args {
            inline,
            spill: Vec::new(),
            len: 1,
        }
    }

    fn slice_mut(&mut self) -> &mut [WValue] {
        if self.len > Args::INLINE {
            &mut self.spill
        } else {
            unsafe { self.inline[..self.len].assume_init_mut() }
        }
    }

    fn as_slice(&self) -> &[WValue] {
        if self.len > Args::INLINE {
            &self.spill
        } else {
            unsafe { self.inline[..self.len].assume_init_ref() }
        }
    }
}

fn wren_args(vm: &mut VM, args: *const Value, n: usize) -> Result<Args, u8> {
    Args::cross(vm, 0, args, n)
}

#[inline]
fn cross(vm: &mut VM, v: Value) -> Result<WValue, u8> {
    // A number, which most arguments are, crosses without a call.
    if let Some(n) = v.as_int() {
        return Ok(WValue::num(f64::from(n)));
    }
    if v.as_object().is_none() {
        return Ok(WValue::from_bits(v.to_bits()));
    }
    to_wren(vm, v).ok_or_else(|| {
        raise_core(
            ErrorKind::Type,
            &format!("{} cannot cross into Wren", bridge::describe(v)),
        )
    })
}

/// The shape a core symbol is asked in: what its Wren signature is built
/// from beside the name.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Shape {
    /// `name(_,_)` by arity, or the symbol as it is when it spells a
    /// signature.
    Call(u8),
    /// `name`.
    Get,
    /// `name=(_)`.
    Set,
}

/// The signatures the protocol sends of its own accord, for a sequence
/// and `to_string`, in the order `Signatures::fixed` keeps their symbols
/// and the record keeps their sites.
#[derive(Clone, Copy)]
pub(crate) enum Fixed {
    Index,
    SetIndex,
    Count,
    Iterate,
    IteratorValue,
    ToString,
}

impl Fixed {
    pub(crate) const COUNT: usize = 6;
    const ALL: [Fixed; Fixed::COUNT] = [
        Fixed::Index,
        Fixed::SetIndex,
        Fixed::Count,
        Fixed::Iterate,
        Fixed::IteratorValue,
        Fixed::ToString,
    ];

    fn text(self) -> &'static str {
        match self {
            Fixed::Index => "[_]",
            Fixed::SetIndex => "[_]=(_)",
            Fixed::Count => "count",
            Fixed::Iterate => "iterate(_)",
            Fixed::IteratorValue => "iteratorValue(_)",
            Fixed::ToString => "toString",
        }
    }
}

/// The VM's symbols for the signatures the bridge asks for, so a call
/// hashes no name: per core symbol and shape, the instance signature's
/// symbol and its `static:` twin's, and the fixed ones once.
#[derive(Default)]
pub(crate) struct Signatures {
    known: HashMap<(u32, Shape), (SymbolId, SymbolId)>,
    fixed: Option<[(SymbolId, SymbolId); Fixed::COUNT]>,
}

impl Signatures {
    fn of(&mut self, vm: &mut VM, name: Symbol, shape: Shape) -> (SymbolId, SymbolId) {
        *self.known.entry((name.0, shape)).or_insert_with(|| {
            let text = name.name();
            let sig = match shape {
                Shape::Call(_) if text.contains('(') => text.to_owned(),
                Shape::Call(n) => signature(text, usize::from(n)),
                Shape::Get => text.to_owned(),
                Shape::Set => format!("{text}=(_)"),
            };
            intern_both(vm, &sig)
        })
    }

    fn fixed(&mut self, vm: &mut VM, which: Fixed) -> (SymbolId, SymbolId) {
        self.fixed
            .get_or_insert_with(|| Fixed::ALL.map(|s| intern_both(vm, s.text())))[which as usize]
    }
}

fn intern_both(vm: &mut VM, sig: &str) -> (SymbolId, SymbolId) {
    let instance = vm.interner.intern(sig);
    let statics = vm.interner.intern(&format!("static:{sig}"));
    (instance, statics)
}

/// A method found for a receiver, and the class it was found on.
#[derive(Clone, Copy)]
struct Found {
    method: Method,
    class: *mut ObjClass,
}

/// The method `recv` answers `sig` with: on its class, or as a static
/// when `recv` is itself a class.
fn find(vm: &VM, recv: WValue, sig: SymbolId, statics: SymbolId) -> Option<Found> {
    let class = vm.class_of(recv);
    if !class.is_null()
        && let Some(&method) = unsafe { (*class).find_method(sig) }
    {
        return Some(Found { method, class });
    }
    let p = recv.as_object()?;
    if unsafe { (*(p as *const ObjHeader)).obj_type } != ObjType::Class {
        return None;
    }
    let class = p as *mut ObjClass;
    let method = *unsafe { (*class).find_method(statics) }?;
    Some(Found { method, class })
}

/// The method `recv` answers the signature of `name` in `shape` with.
/// The signature's symbols come from `site` when it was filled for this
/// VM (`key`), else from the VM's signature cache, and are left in `site`.
fn find_by(
    vm: &mut VM,
    key: usize,
    recv: WValue,
    name: Symbol,
    shape: Shape,
    site: Option<&CallSite>,
) -> Option<Found> {
    let (sig, statics) = match site.and_then(|s| s.get(key)) {
        Some((sig, statics)) => (
            SymbolId::from_raw(sig as u32),
            SymbolId::from_raw(statics as u32),
        ),
        None => {
            let rec = record_for(vm.object_class as *mut u8);
            let syms = rec.signatures().borrow_mut().of(vm, name, shape);
            if let Some(site) = site {
                site.set(key, syms.0.index() as usize, syms.1.index() as usize);
            }
            syms
        }
    };
    find(vm, recv, sig, statics)
}

/// Count a call the VM's dispatch does not: a body only ever entered from
/// another language still compiles.
fn tick(vm: &mut VM, closure: *mut ObjClosure) {
    let id = FuncId(unsafe { (*(*closure).function).fn_id });
    let compiled = vm
        .engine
        .jit_code
        .get(id.0 as usize)
        .is_some_and(|p| !p.is_null());
    if !compiled && vm.engine.record_call(id) {
        vm.engine.request_tier_up(id, &vm.interner);
    }
}

/// Leave the whole send in `site` for the next call from it: the closure
/// and the class it was found on, under the VM's record, so the next call
/// checks the receiver against the class and dispatches. Only a closure
/// or a constructor; anything else keeps the plain path.
fn leave_direct(site: Option<&CallSite>, key: usize, found: &Found) {
    let Some(site) = site else {
        return;
    };
    match found.method {
        Method::Closure(closure) => {
            site.set_direct(direct_call, key, closure as usize, found.class as usize);
        }
        Method::Constructor(closure) => {
            site.set_direct(
                direct_construct,
                key,
                closure as usize,
                found.class as usize,
            );
        }
        _ => {}
    }
}

/// The VM and receiver a direct send runs on, when the site still fits:
/// the object is this thread's VM's, and the receiver is the class the
/// site was filled for, or an instance of exactly it.
#[inline]
fn direct_receiver(
    site: &CallSite,
    obj: *mut u8,
) -> Option<(&'static mut VM, WValue, *mut ObjClass)> {
    let (key, _, class) = site.words();
    if record_address(obj) != key {
        return None;
    }
    let vm = unsafe { record_at(key) }.entered_here();
    if vm.is_null() {
        return None;
    }
    let vm = unsafe { &mut *vm };
    let recv = unsafe { receiver(obj) };
    let class = class as *mut ObjClass;
    let fits = recv.as_object() == Some(class as *mut u8) || vm.class_of(recv) == class;
    fits.then_some((vm, recv, class))
}

/// The direct send of a method or getter: the closure the site holds, on
/// the receiver, through the VM's own dispatch.
unsafe extern "C-unwind" fn direct_call(
    site: *const CallSite,
    obj: usize,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let site = unsafe { &*site };
    let Some((vm, recv, class)) = direct_receiver(site, obj as *mut u8) else {
        return REPLY_MISSING;
    };
    let (_, closure, _) = site.words();
    // The receiver and what a compiled body takes in registers, on the
    // stack; anything wider goes the long way.
    if n >= Args::INLINE {
        return REPLY_MISSING;
    }
    let mut with_recv = [MaybeUninit::<WValue>::uninit(); Args::INLINE];
    with_recv[0].write(recv);
    for (i, &arg) in unsafe { std::slice::from_raw_parts(args, n) }
        .iter()
        .enumerate()
    {
        match cross(vm, arg) {
            Ok(v) => with_recv[1 + i].write(v),
            Err(code) => return code,
        };
    }
    let with_recv = unsafe { with_recv[..=n].assume_init_ref() };
    // The closure the site found, called as a send would once it has found
    // it, with the thread's JIT state read once.
    let bits = match entered(vm, Some(site), |vm| {
        wren_lift::codegen::runtime_fns::call_found_closure(
            vm,
            closure as *mut ObjClosure,
            with_recv,
            class,
        )
    }) {
        Ok(bits) => bits,
        Err(code) => return code,
    };
    finish(vm, Some(WValue::from_bits(bits)), "", out)
}

/// The direct send of a constructor: the class is the receiver.
unsafe extern "C-unwind" fn direct_construct(
    site: *const CallSite,
    obj: usize,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let site = unsafe { &*site };
    let Some((vm, recv, class)) = direct_receiver(site, obj as *mut u8) else {
        return REPLY_MISSING;
    };
    let (_, closure, _) = site.words();
    let closure = closure as *mut ObjClosure;
    let mut args = match Args::cross(vm, 1, args, n) {
        Ok(args) => args,
        Err(code) => return code,
    };
    args.slice_mut()[0] = recv;
    tick(vm, closure);
    let bits = match entered(vm, Some(site), |vm| {
        wren_lift::codegen::runtime_fns::dispatch_method_pub(
            vm,
            Method::Constructor(closure),
            args.as_slice(),
            Some(class),
        )
    }) {
        Ok(bits) => bits,
        Err(code) => return code,
    };
    let instance = made(vm, WValue::from_bits(bits));
    finish(vm, Some(instance), "", out)
}

/// An object made on Wren's behalf. The allocation is a safepoint, as
/// wren_lift's own are: a cycle that is due runs here, the object pinned,
/// so a program that makes Wren objects only through the bridge still
/// collects them.
pub(crate) fn made(vm: &mut VM, v: WValue) -> WValue {
    WValue::from_bits(unsafe { wren_lift::codegen::runtime_fns::finish_alloc(vm, v) })
}

/// Run `found` on `recv` with `args`, whose slot 0 is free for the
/// receiver, through the VM's own method dispatch: what its compiled code
/// calls once it has found a method.
fn run(
    vm: &mut VM,
    recv: WValue,
    found: Found,
    args: &mut Args,
    site: Option<&CallSite>,
    out: *mut Value,
) -> u8 {
    args.slice_mut()[0] = recv;
    let ran = entered(vm, site, |vm| match found.method {
        Method::Closure(closure) => wren_lift::codegen::runtime_fns::call_found_closure(
            vm,
            closure,
            args.as_slice(),
            found.class,
        ),
        Method::Constructor(closure) => {
            tick(vm, closure);
            let bits = wren_lift::codegen::runtime_fns::dispatch_method_pub(
                vm,
                found.method,
                args.as_slice(),
                Some(found.class),
            );
            made(vm, WValue::from_bits(bits)).to_bits()
        }
        _ => wren_lift::codegen::runtime_fns::dispatch_method_pub(
            vm,
            found.method,
            args.as_slice(),
            Some(found.class),
        ),
    });
    let bits = match ran {
        Ok(bits) => bits,
        Err(code) => return code,
    };
    finish(vm, Some(WValue::from_bits(bits)), "", out)
}

fn finish(vm: &mut VM, result: Option<WValue>, sig: &str, out: *mut Value) -> u8 {
    if let Some(code) = take_error(vm) {
        return code;
    }
    match result {
        Some(v) => {
            unsafe { *out = from_wren(v) };
            REPLY_OK
        }
        None => raise_core(
            ErrorKind::Internal,
            &format!("the Wren VM could not run `{sig}`"),
        ),
    }
}

/// The slot index of the field `name` (or `_name`) on the instance's class,
/// from the layouts the VM keeps per class.
fn field_slot(vm: &VM, recv: WValue, name: &str) -> Option<usize> {
    let class_name = vm.class_name_of(recv);
    let layout = vm.field_layouts.get(&class_name)?;
    layout
        .iter()
        .position(|f| f == name || (f.starts_with('_') && f[1..] == *name))
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

/// A getter, else a field of an instance by name.
unsafe extern "C-unwind" fn get_member(obj: *mut u8, name: Symbol, out: *mut Value) -> u8 {
    get_at(obj, name, None, out)
}

unsafe extern "C-unwind" fn get_member_at(
    obj: *mut u8,
    name: Symbol,
    site: *mut CallSite,
    out: *mut Value,
) -> u8 {
    get_at(obj, name, unsafe { site.as_ref() }, out)
}

fn get_at(obj: *mut u8, name: Symbol, site: Option<&CallSite>, out: *mut Value) -> u8 {
    let (vm, key) = match vm_of(obj) {
        Ok(vm) => vm,
        Err(code) => return code,
    };
    let recv = unsafe { receiver(obj) };
    if let Some(found) = find_by(vm, key, recv, name, Shape::Get, site) {
        leave_direct(site, key, &found);
        let mut args = Args::receiver_only();
        return run(vm, recv, found, &mut args, site, out);
    }
    let name = name.name();
    if unsafe { obj_type(obj) } == ObjType::Instance
        && let Some(slot) = field_slot(vm, recv, name)
        && let Some(v) = unsafe { &*(wren_ptr(obj) as *const ObjInstance) }.get_field(slot)
    {
        unsafe { *out = from_wren(v) };
        return REPLY_OK;
    }
    REPLY_MISSING
}

/// A setter `name=(_)`, else a field of an instance by name.
unsafe extern "C-unwind" fn set_member(obj: *mut u8, name: Symbol, value: Value) -> u8 {
    set_at(obj, name, None, value)
}

unsafe extern "C-unwind" fn set_member_at(
    obj: *mut u8,
    name: Symbol,
    site: *mut CallSite,
    value: Value,
) -> u8 {
    set_at(obj, name, unsafe { site.as_ref() }, value)
}

fn set_at(obj: *mut u8, name: Symbol, site: Option<&CallSite>, value: Value) -> u8 {
    let (vm, key) = match vm_of(obj) {
        Ok(vm) => vm,
        Err(code) => return code,
    };
    let recv = unsafe { receiver(obj) };
    let mut args = match Args::cross(vm, 1, &value, 1) {
        Ok(args) => args,
        Err(code) => return code,
    };
    if let Some(found) = find_by(vm, key, recv, name, Shape::Set, site) {
        leave_direct(site, key, &found);
        let mut ignored = Value::null();
        return run(vm, recv, found, &mut args, site, &mut ignored);
    }
    let name = name.name();
    if unsafe { obj_type(obj) } == ObjType::Instance
        && let Some(slot) = field_slot(vm, recv, name)
    {
        unsafe { &mut *(wren_ptr(obj) as *mut ObjInstance) }.set_field(slot, args.as_slice()[1]);
        return REPLY_OK;
    }
    REPLY_MISSING
}

/// A method by name and arity, or by a full signature (`hit(_)`), which a
/// `WrenMethod` callable sends and which must agree with the arity. With
/// no arguments, a getter of that name answers when the class has no
/// `name()`: languages without Wren's distinction call `toString()`. A
/// method the class lacks raises Wren's own error, as a call in Wren would.
unsafe extern "C-unwind" fn invoke(
    obj: *mut u8,
    name: Symbol,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    invoke_at_opt(obj, name, None, args, n, out)
}

unsafe extern "C-unwind" fn invoke_at(
    obj: *mut u8,
    name: Symbol,
    site: *mut CallSite,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    invoke_at_opt(obj, name, unsafe { site.as_ref() }, args, n, out)
}

fn invoke_at_opt(
    obj: *mut u8,
    name: Symbol,
    site: Option<&CallSite>,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    let (vm, key) = match vm_of(obj) {
        Ok(vm) => vm,
        Err(code) => return code,
    };
    let recv = unsafe { receiver(obj) };
    let text = name.name();
    // A site is one name and arity: what it was filled under has passed.
    if site.is_none_or(|s| s.get(key).is_none()) && text.contains('(') {
        let arity = arity_of(text);
        if arity != n {
            return raise_core(
                ErrorKind::Type,
                &format!("`{text}` takes {arity} arguments, not {n}"),
            );
        }
    }
    if n > u8::MAX as usize {
        return raise_core(ErrorKind::Type, "too many arguments");
    }
    // The method by arity, else the getter of that name for a call with
    // no arguments; the site remembers the method's signature only.
    let found = match find_by(vm, key, recv, name, Shape::Call(n as u8), site) {
        Some(found) => found,
        None => match (n == 0)
            .then(|| find_by(vm, key, recv, name, Shape::Get, None))
            .flatten()
        {
            Some(found) => found,
            None => {
                let class = vm.class_name_of(recv);
                let sig = if text.contains('(') {
                    text.to_owned()
                } else {
                    signature(text, n)
                };
                return raise_wren(format!("{class} does not implement '{sig}'"));
            }
        },
    };
    leave_direct(site, key, &found);
    let mut args = match Args::cross(vm, 1, args, n) {
        Ok(args) => args,
        Err(code) => return code,
    };
    run(vm, recv, found, &mut args, site, out)
}

/// A closure, called with the arguments as its parameters.
unsafe extern "C-unwind" fn call(
    obj: *mut u8,
    args: *const Value,
    n: usize,
    out: *mut Value,
) -> u8 {
    if unsafe { obj_type(obj) } != ObjType::Closure {
        return REPLY_UNSUPPORTED;
    }
    let (vm, _) = match vm_of(obj) {
        Ok(vm) => vm,
        Err(code) => return code,
    };
    let args = match wren_args(vm, args, n) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let closure = unsafe { wren_ptr(obj) } as *mut ObjClosure;
    // A closure's compiled body when it has one; a `Fn` takes no receiver
    // and belongs to no class.
    let bits = match guarded(vm, |vm| {
        wren_lift::codegen::runtime_fns::call_found_closure(
            vm,
            closure,
            args.as_slice(),
            ptr::null_mut(),
        )
    }) {
        Ok(bits) => bits,
        Err(code) => return code,
    };
    finish(vm, Some(WValue::from_bits(bits)), "call", out)
}

/// Send a fixed signature with `args` if the class has it, else
/// `Unsupported`. The record's site for the signature stands for every
/// such send into this VM, so the run is guarded only once one has
/// called back.
fn send_if_present(obj: *mut u8, which: Fixed, args: &[Value], out: *mut Value) -> u8 {
    let (vm, _) = match vm_of(obj) {
        Ok(vm) => vm,
        Err(code) => return code,
    };
    let recv = unsafe { receiver(obj) };
    let rec = record_for(vm.object_class as *mut u8);
    let (sig, statics) = rec.signatures().borrow_mut().fixed(vm, which);
    let Some(found) = find(vm, recv, sig, statics) else {
        return REPLY_UNSUPPORTED;
    };
    let mut args = match Args::cross(vm, 1, args.as_ptr(), args.len()) {
        Ok(args) => args,
        Err(code) => return code,
    };
    run(vm, recv, found, &mut args, Some(rec.fixed_site(which)), out)
}

unsafe extern "C-unwind" fn index(obj: *mut u8, key: Value, out: *mut Value) -> u8 {
    send_if_present(obj, Fixed::Index, &[key], out)
}

unsafe extern "C-unwind" fn set_index(obj: *mut u8, key: Value, value: Value) -> u8 {
    let mut ignored = Value::null();
    send_if_present(obj, Fixed::SetIndex, &[key, value], &mut ignored)
}

/// `count`, for anything that has one.
/// A closure's arity: what `call` takes.
unsafe extern "C-unwind" fn arity(obj: *mut u8, out: *mut usize) -> u8 {
    if unsafe { obj_type(obj) } != ObjType::Closure {
        return REPLY_UNSUPPORTED;
    }
    let closure = unsafe { wren_ptr(obj) } as *const ObjClosure;
    let function = unsafe { (*closure).function };
    if function.is_null() {
        return REPLY_UNSUPPORTED;
    }
    unsafe { *out = usize::from((*function).arity) };
    REPLY_OK
}

unsafe extern "C-unwind" fn len(obj: *mut u8, out: *mut usize) -> u8 {
    let mut count = Value::null();
    let code = send_if_present(obj, Fixed::Count, &[], &mut count);
    if code != REPLY_OK {
        return code;
    }
    match count.as_number() {
        Some(n) if n >= 0.0 => {
            unsafe { *out = n as usize };
            REPLY_OK
        }
        _ => REPLY_UNSUPPORTED,
    }
}

/// Wren's iterator protocol: `iterate(_)` advances the state, `false`
/// ending it; `iteratorValue(_)` reads the element at a state.
unsafe extern "C-unwind" fn iterate(obj: *mut u8, state: *mut Value, out: *mut Value) -> u8 {
    let mut next = Value::null();
    let code = send_if_present(obj, Fixed::Iterate, &[unsafe { *state }], &mut next);
    if code != REPLY_OK {
        return code;
    }
    if next.as_bool() == Some(false) {
        return REPLY_MISSING;
    }
    unsafe { *state = next };
    send_if_present(obj, Fixed::IteratorValue, &[next], out)
}

/// `toString`, as a core string: what it answered, having crossed as one,
/// or a description of a non-string it answered.
unsafe extern "C-unwind" fn to_string(obj: *mut u8, out: *mut Value) -> u8 {
    let mut text = Value::null();
    let code = send_if_present(obj, Fixed::ToString, &[], &mut text);
    if code != REPLY_OK {
        return code;
    }
    if unsafe { Str::text(text) }.is_none() {
        text = Str::value(Str::new(&bridge::describe(text)));
    }
    unsafe { *out = text };
    REPLY_OK
}

/// The text of a core value that is a Wren string.
fn wren_string<'a>(v: Value) -> Option<&'a str> {
    let p = v.as_object()? as *mut u8;
    if p.is_null() || !is_wren(p) {
        return None;
    }
    if unsafe { obj_type(p) } != ObjType::String {
        return None;
    }
    Some(unsafe { &(*(wren_ptr(p) as *const ObjString)).value })
}

/// A string's content hash; identity for everything else, as Wren's own
/// equality has it.
unsafe extern "C-unwind" fn hash(obj: *mut u8, out: *mut u64) -> u8 {
    let h = if unsafe { obj_type(obj) } == ObjType::String {
        unsafe { (*(wren_ptr(obj) as *const ObjString)).hash }
    } else {
        (unsafe { wren_ptr(obj) }) as usize as u64
    };
    unsafe { *out = h };
    REPLY_OK
}

/// Wren's `==`: strings by content, other objects by identity. A core
/// string compares by content against a Wren string; an object of another
/// language is never equal.
unsafe extern "C-unwind" fn equals(obj: *mut u8, other: Value, out: *mut bool) -> u8 {
    let mine = unsafe { receiver(obj) };
    let same = match other.as_object() {
        Some(p) if !p.is_null() && is_wren(p as *mut u8) => {
            mine.equals(WValue::object((p as *mut u8).wrapping_add(PREFIX)))
        }
        Some(_) => match (wren_string(Value::object(obj as *const c_void)), unsafe {
            Str::text(other)
        }) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        },
        None => mine.equals(WValue::from_bits(other.to_bits())),
    };
    unsafe { *out = same };
    REPLY_OK
}

/// A foreign object's data.
unsafe extern "C-unwind" fn unwrap_native(obj: *mut u8, out: *mut *mut c_void) -> u8 {
    if unsafe { obj_type(obj) } != ObjType::Foreign {
        return REPLY_UNSUPPORTED;
    }
    let foreign = unsafe { &*(wren_ptr(obj) as *const ObjForeign) };
    unsafe { *out = foreign.data.as_ptr() as *mut c_void };
    REPLY_OK
}

/// The name the receiver's class was published under (`hud.Hud`), else
/// its bare name.
unsafe extern "C-unwind" fn type_name(obj: *mut u8, out: *mut Symbol) -> u8 {
    // The class is in the object's header; the VM is needed only to name
    // one that was never published.
    let class = unsafe { (*(wren_ptr(obj) as *const ObjHeader)).class };
    let exported = record_for(unsafe { wren_ptr(obj) })
        .exports()
        .borrow()
        .type_name(class);
    let name = match exported {
        Some(sym) => sym,
        None => {
            let (vm, _) = match vm_of(obj) {
                Ok(vm) => vm,
                Err(code) => return code,
            };
            caribou::symbol::intern(&vm.class_name_of(unsafe { receiver(obj) }))
        }
    };
    unsafe { *out = name };
    REPLY_OK
}

/// The shadow is the object's bridge word (see `heap`): one object of one
/// language, whichever keeps one first. No VM is needed: the word is the
/// object's own, and its VM being gone changes nothing.
unsafe extern "C-unwind" fn shadow(obj: *mut u8, lang: LangId, out: *mut *mut u8) -> u8 {
    match crate::heap::shadow_of(unsafe { wren_ptr(obj) }, lang) {
        Some(p) => {
            unsafe { *out = p };
            REPLY_OK
        }
        None => REPLY_MISSING,
    }
}

unsafe extern "C-unwind" fn keep_shadow(obj: *mut u8, shadow: *mut u8, out: *mut *mut u8) -> u8 {
    match crate::heap::keep_shadow(unsafe { wren_ptr(obj) }, shadow) {
        Ok(()) => REPLY_OK,
        Err(Some(kept)) => {
            unsafe { *out = kept };
            REPLY_MISSING
        }
        Err(None) => REPLY_UNSUPPORTED,
    }
}

unsafe extern "C-unwind" fn drop_shadow(obj: *mut u8, shadow: *mut u8) -> u8 {
    crate::heap::drop_shadow(unsafe { wren_ptr(obj) }, shadow);
    REPLY_OK
}

/// No Wren object is an error by itself: wren_lift's error is a message,
/// and one that reaches the bridge arrives wrapped in a core `Error` whose
/// native payload is that message as a core string. So the error entries
/// stay unanswered.
pub static WREN_PROTO: Protocol = Protocol {
    get_member: Some(get_member),
    set_member: Some(set_member),
    invoke: Some(invoke),
    get_member_at: Some(get_member_at),
    set_member_at: Some(set_member_at),
    invoke_at: Some(invoke_at),
    call: Some(call),
    index: Some(index),
    set_index: Some(set_index),
    len: Some(len),
    arity: Some(arity),
    iterate: Some(iterate),
    to_string: Some(to_string),
    hash: Some(hash),
    equals: Some(equals),
    unwrap_native: Some(unwrap_native),
    type_name: Some(type_name),
    shadow: Some(shadow),
    keep_shadow: Some(keep_shadow),
    drop_shadow: Some(drop_shadow),
    ..Protocol::NONE
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_cross_bit_for_bit_and_ints_become_numbers() {
        for bits in [
            Value::null().to_bits(),
            Value::bool(true).to_bits(),
            Value::bool(false).to_bits(),
            Value::number(2.5).to_bits(),
            Value::number(-0.0).to_bits(),
        ] {
            let w = WValue::from_bits(bits);
            assert_eq!(from_wren(w).to_bits(), bits);
        }
        assert!(WValue::from_bits(Value::null().to_bits()).is_null());
        assert_eq!(
            WValue::from_bits(Value::bool(true).to_bits()).as_bool(),
            Some(true)
        );
        assert_eq!(
            WValue::from_bits(Value::number(2.5).to_bits()).as_num(),
            Some(2.5)
        );
        // An int is not a Wren value at all, so it is converted.
        let int = WValue::from_bits(Value::int(7).to_bits());
        assert!(!int.is_num() && !int.is_object() && !int.is_null() && !int.is_bool());
    }

    /// The object is read for its kind, so it is a real header; its
    /// address is what crosses, less the prefix.
    #[test]
    fn an_object_crosses_by_its_core_address() {
        let header = Box::new(ObjHeader::new(ObjType::Range));
        let wren_side = &*header as *const ObjHeader as *mut u8;
        let v = from_wren(WValue::object(wren_side));
        assert_eq!(
            v.as_object(),
            Some(wren_side.wrapping_sub(PREFIX) as *mut c_void)
        );
        assert_eq!(
            WValue::object(wren_side).to_bits() & !0xFFFF,
            v.to_bits() & !0xFFFF
        );
    }

    #[test]
    fn signatures_follow_wrens_convention() {
        assert_eq!(signature("inc", 0), "inc()");
        assert_eq!(signature("add", 1), "add(_)");
        assert_eq!(signature("at", 3), "at(_,_,_)");
        assert_eq!(arity_of("draw()"), 0);
        assert_eq!(arity_of("hit(_)"), 1);
        assert_eq!(arity_of("do_it(_,_)"), 2);
        assert_eq!(arity_of("hp=(_)"), 1);
        assert_eq!(arity_of("[_]=(_)"), 2);
    }

    #[test]
    fn kinds_come_from_the_message() {
        assert_eq!(
            kind_of("Null does not implement 'x'"),
            ErrorKind::NullAccess
        );
        assert_eq!(kind_of("Subscript out of bounds."), ErrorKind::Index);
        assert_eq!(
            kind_of("Counter does not implement 'nope()'"),
            ErrorKind::Runtime
        );
        assert_eq!(kind_of("boom"), ErrorKind::Runtime);
    }
}
