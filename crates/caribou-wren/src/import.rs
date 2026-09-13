//! Another language's classes as Wren classes: what `import "game:Player"
//! for Player` binds.
//!
//! The registry publishes a module's interface; this module turns it into
//! a wren_lift module blob in memory, one `ClassMir` per published class,
//! and installs it through `interpret_bytecode`, the path a `.wlbc` takes.
//! Each installed class then gets one native method per published member:
//! `new(_)` for the constructor, `hit(_)` for a method, `hp` and `hp=(_)`
//! for a field, statics under `static:`. wren_lift binds a class's foreign
//! stubs only by `dlsym` in a `#!native` library, so the natives are bound
//! here, into the class's method table, right after the install.
//!
//! An instance of an installed class is an ordinary `ObjInstance` with one
//! field, holding a core `Handle` to the other language's object (as the
//! bridge sees it: a `HaxeRef` for Haxe) as a Wren number. The handle
//! roots the object for as long as the instance lives; the adapter's sweep
//! releases it when the instance dies, and `heap_drop` releases whatever
//! is left.
//!
//! A `NativeFn` is a bare `fn` and receives only the receiver and the
//! arguments, so the natives are trampolines: `SLOTS` distinct functions,
//! the `i`th of which calls the `i`th member bound on the receiver's class.
//! A call is one lookup, by class pointer, in the table its VM's heap
//! record keeps (walking to the superclass for a Wren subclass), then the
//! bridge call with the typed callable the interface published. Each class
//! is limited to `SLOTS` members.
//!
//! The VM the callbacks and the trampolines act on is the one entered on
//! this thread (`enter_vm`, `with_vm`) or the one wren_lift is dispatching
//! on: a `VMConfig` callback and a `NativeContext` carry no VM handle the
//! adapter could use to install a module.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::mem::MaybeUninit;
use std::ptr;
use std::rc::Rc;

use caribou::bridge;
use caribou::error::{Error, Str};
use caribou::hash::AddressMap;
use caribou::heap::{self, Handle};
use caribou::protocol::{CallSite, Callable};
use caribou::registry::{self, ClassIface, Interface, MethodIface};
use caribou::symbol::{self, Symbol};
use caribou::world::language_name;
use caribou_abi::{ErrorKind, LangId, Value};
use wren_lift::intern::Interner;
use wren_lift::mir::{BasicBlock, BlockId, ClassMir, MirFunction, ModuleMir, Terminator};
use wren_lift::runtime::engine::InterpretResult;
use wren_lift::runtime::object::{
    NativeContext, NativeFn, ObjClass, ObjHeader, ObjInstance, ObjType,
};
use wren_lift::runtime::value::Value as WValue;
use wren_lift::runtime::vm::{VM, VMConfig};
use wren_lift::sema::protocol::ProtocolSet;
use wren_lift::serialize;

use crate::heap::{WrenHeap, record_for, wren_lang};
use crate::proto::{current_vm, from_wren, to_wren};

/// Members one installed class can bind.
pub const SLOTS: usize = 256;

/// The fields of every instance: the handle on the object it stands for,
/// and the object's address, as numbers. The heap does not move, so the
/// address is good for as long as the handle holds.
const HANDLE_FIELD: usize = 0;
const OBJECT_FIELD: usize = 1;
/// Their names in the class's field layout: not names Wren source can
/// spell, so a subclass's own fields never alias them.
const FIELD_NAMES: [&str; 2] = ["__caribou_handle", "__caribou_object"];

// ---------------------------------------------------------------------------
// The per-heap table
// ---------------------------------------------------------------------------

/// What one trampoline slot of a class does.
#[derive(Clone)]
enum Kind {
    Ctor,
    Method,
    Static,
    Getter(Symbol),
    Setter(Symbol),
    /// A static field, read on the class object the callable holds.
    ClassGetter(Symbol),
    ClassSetter(Symbol),
    /// `call(...)` on a foreign function: the object behind the instance.
    Call,
    /// `arity` of a foreign function.
    Arity,
}

struct Target {
    kind: Kind,
    /// For the trace frame.
    name: String,
    callable: Callable,
    /// What the callee's protocol derived for this slot last time.
    site: CallSite,
}

impl Target {
    fn new(kind: Kind, name: String, callable: Callable) -> Target {
        Target {
            kind,
            name,
            callable,
            site: CallSite::new(),
        }
    }
}

/// The class last asked for a binding and the binding, one count held.
#[derive(Default)]
struct LastBinding(Cell<Option<(usize, ptr::NonNull<ClassBinding>)>>);

impl LastBinding {
    fn get(&self) -> (usize, *const ClassBinding) {
        match self.0.get() {
            Some((class, b)) => (class, b.as_ptr()),
            None => (0, ptr::null()),
        }
    }

    fn set(&self, at: (usize, *const ClassBinding)) {
        self.0
            .set(ptr::NonNull::new(at.1 as *mut ClassBinding).map(|b| (at.0, b)));
    }

    fn replace(&self, at: (usize, *const ClassBinding)) -> (usize, *const ClassBinding) {
        let old = self.get();
        self.set(at);
        old
    }
}

/// One installed class: per slot, the member it binds.
struct ClassBinding {
    targets: Vec<Target>,
}

/// The installed classes of one VM, kept on its heap record so they die
/// with it. Touched only by the VM's own thread.
#[derive(Default)]
pub(crate) struct Imports {
    classes: AddressMap<Rc<ClassBinding>>,
    /// `(lang, type name)` to the class that stands for it.
    by_type: HashMap<(LangId, String), *mut ObjClass>,
    /// The class last asked for and its binding, one count held.
    last: LastBinding,
    /// Every live instance and the handle in its field, for the sweep and
    /// for `heap_drop`.
    live: AddressMap<Handle>,
    /// The class a foreign function is an instance of, once installed.
    function: Option<*mut ObjClass>,
}

impl Imports {
    /// Whether `class` was installed here for another language's.
    pub(crate) fn installed(&self, class: *mut ObjClass) -> bool {
        self.classes.contains_key(&(class as usize))
    }

    /// Forget the cached binding, giving up its count.
    fn forget_last(&self) {
        let (_, last) = self.last.replace((0, ptr::null()));
        if !last.is_null() {
            drop(unsafe { Rc::from_raw(last) });
        }
    }

    /// The binding of `class` or of its nearest bound superclass. The last
    /// answer is kept, since a call site mostly sees one class; a binding
    /// lives as long as its entry in `classes`, which only `release_all`
    /// clears.
    fn binding_of(&self, class: *mut ObjClass) -> Option<Rc<ClassBinding>> {
        let (last_class, last) = self.last.get();
        if last_class == class as usize && !last.is_null() {
            // A count for the caller; the cache keeps its own.
            unsafe { Rc::increment_strong_count(last) };
            return Some(unsafe { Rc::from_raw(last) });
        }
        let mut at = class;
        while !at.is_null() {
            if let Some(b) = self.classes.get(&(at as usize)) {
                self.forget_last();
                self.last.set((class as usize, Rc::into_raw(b.clone())));
                return Some(b.clone());
            }
            at = unsafe { (*at).superclass };
        }
        None
    }
}

/// A dead object of `rec`'s heap, before it is dropped: release the handle
/// an instance held. On the sweeping thread, the VM's, under the GC lock
/// `gc` holds.
pub(crate) fn finalize_dead(rec: &WrenHeap, obj: *mut u8, gc: &mut heap::ImmixAllocator) {
    if unsafe { (*(obj as *const ObjHeader)).obj_type } != ObjType::Instance {
        return;
    }
    let handle = rec.imports().borrow_mut().live.remove(&(obj as usize));
    if let Some(handle) = handle {
        gc.handle_release(handle);
    }
}

/// Every handle the heap's instances still hold, released with the GC
/// lock `gc` holds; the heap is going away with its VM.
pub(crate) fn release_all(rec: &WrenHeap, gc: &mut heap::ImmixAllocator) {
    let mut imports = rec.imports().borrow_mut();
    for (_, handle) in imports.live.drain() {
        gc.handle_release(handle);
    }
    imports.forget_last();
    imports.classes.clear();
    imports.by_type.clear();
}

// ---------------------------------------------------------------------------
// Configuration and install
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportError(String);

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ImportError {}

/// `ns:module` for a namespace the registry knows, else `None`.
fn namespaced(name: &str) -> Option<(&str, &str)> {
    let (ns, module) = name.split_once(':')?;
    if ns.is_empty() || module.is_empty() || ns.starts_with('@') {
        return None;
    }
    let known = registry::namespaces().iter().any(|n| n.name == ns)
        || caribou::world::language_id(ns).is_some();
    known.then_some((ns, module))
}

/// The Wren module name an interface installs under: the language's own
/// namespace, so every namespace addressing one module reaches one class.
fn canonical(lang: LangId, module: &str) -> String {
    format!("{}:{}", language_name(lang), module)
}

/// Install the import callbacks, ahead of any the host set: a name
/// `ns:module` under a namespace the registry knows resolves to the
/// module, loading it on first use through the registry's loaders, and
/// installing it when it is another language's; a plain name is served
/// from the project's roots (`project.rs`), beside the importer first;
/// what is neither goes to the host's callbacks. The VM must be entered
/// on the thread that runs it.
pub fn configure(config: &mut VMConfig) {
    let previous_resolve = config.resolve_module_fn.take();
    config.resolve_module_fn = Some(Box::new(move |name: &str, from: &str| {
        if let Some((ns, module)) = namespaced(name) {
            let found = match registry::resolve_or_load(ns, module) {
                Ok(found) => found,
                Err(e) => {
                    eprintln!("caribou: {e}");
                    return None;
                }
            };
            if let Some((lang, module)) = found {
                if lang == wren_lang() {
                    return Some(module);
                }
                let vm = current_vm();
                if vm.is_null() {
                    return None;
                }
                return match install(unsafe { &mut *vm }, lang, &module) {
                    Ok(name) => Some(name),
                    Err(e) => {
                        eprintln!("caribou: {e}");
                        None
                    }
                };
            }
        }
        match previous_resolve.as_ref().and_then(|f| f(name, from)) {
            Some(resolved) => Some(resolved),
            None => Some(crate::project::relative(name, from)),
        }
    }));
    let previous_load = config.load_module_fn.take();
    config.load_module_fn = Some(Box::new(move |name: &str, from: &str| {
        if namespaced(name).is_some() {
            return None;
        }
        previous_load
            .as_ref()
            .and_then(|f| f(name, from))
            .or_else(|| crate::project::source(name))
    }));
}

/// Install the published module `module` of `lang` into `vm`, if it is not
/// already, and return its Wren module name.
pub fn install(vm: &mut VM, lang: LangId, module: &str) -> Result<String, ImportError> {
    if !crate::installed() {
        return Err(ImportError(
            "imports need the core heap under the VM: `caribou_wren::install` first".to_owned(),
        ));
    }
    let name = canonical(lang, module);
    if vm.engine.modules.contains_key(&name) {
        return Ok(name);
    }
    let iface = registry::interface(lang, module)
        .ok_or_else(|| ImportError(format!("`{name}` is not published")))?;
    let bytes = blob(&iface).map_err(|e| ImportError(format!("`{name}`: {e}")))?;
    if vm.interpret_bytecode(&name, &bytes) != InterpretResult::Success {
        return Err(ImportError(format!("`{name}` did not install")));
    }
    let rec = record_for(vm.object_class as *mut u8);
    for class in &iface.classes {
        let value = vm
            .find_imported_var_from(&class.name, &name)
            .ok_or_else(|| ImportError(format!("`{name}` installed no `{}`", class.name)))?;
        let ptr = value.as_object().unwrap_or(std::ptr::null_mut()) as *mut ObjClass;
        if ptr.is_null() || unsafe { (*(ptr as *const ObjHeader)).obj_type } != ObjType::Class {
            return Err(ImportError(format!(
                "`{name}` installed `{}` as no class",
                class.name
            )));
        }
        let binding = bind(vm, ptr, class)?;
        let mut imports = rec.imports().borrow_mut();
        imports.classes.insert(ptr as usize, Rc::new(binding));
        imports
            .by_type
            .insert((iface.lang, class.type_name.clone()), ptr);
    }
    Ok(name)
}

/// The module blob: an empty top level and one foreign-shaped class per
/// published class, each with the handle field and nothing else.
fn blob(iface: &Interface) -> Result<Vec<u8>, serialize::SerializeError> {
    let mut interner = Interner::new();
    let mut top_level = MirFunction::new(interner.intern("<module>"), 0);
    let mut entry = BasicBlock::new(BlockId(0));
    entry.terminator = Terminator::ReturnNull;
    top_level.blocks.push(entry);
    top_level.next_block = 1;
    let mut var_names = Vec::with_capacity(iface.classes.len());
    let mut class_field_names = HashMap::new();
    let classes = iface
        .classes
        .iter()
        .map(|class| {
            var_names.push(class.name.clone());
            class_field_names.insert(
                class.name.clone(),
                FIELD_NAMES.iter().map(|n| (*n).to_owned()).collect(),
            );
            ClassMir {
                name: interner.intern(&class.name),
                superclass: None,
                methods: Vec::new(),
                num_fields: FIELD_NAMES.len() as u16,
                protocols: ProtocolSet::EMPTY,
                attributes: Vec::new(),
                native_library: None,
                foreign_methods: Vec::new(),
            }
        })
        .collect();
    let module = ModuleMir {
        top_level,
        classes,
        closures: Vec::new(),
    };
    let var_sources = vec![None; var_names.len()];
    serialize::emit(
        &interner,
        &module,
        &var_names,
        &var_sources,
        &class_field_names,
    )
}

/// Wren's signature for `name` taking `arity` arguments.
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

/// The members of `class` in slot order, each with its Wren method-table
/// signature: the constructor, the methods, then a getter and a setter per
/// field.
fn members(class: &ClassIface) -> Vec<(String, Target)> {
    let mut members: Vec<(String, Target)> = Vec::new();
    let qualify = |member: &str| format!("{}.{member}", class.name);
    if let Some(ctor) = &class.ctor {
        members.push((
            format!("static:{}", signature("new", ctor.params.len())),
            Target::new(Kind::Ctor, qualify("new"), ctor.target),
        ));
    }
    for method in &class.methods {
        let MethodIface {
            name,
            is_static,
            params,
            target,
            ..
        } = method;
        let sig = signature(name, params.len());
        members.push((
            if *is_static {
                format!("static:{sig}")
            } else {
                sig
            },
            Target::new(
                if *is_static {
                    Kind::Static
                } else {
                    Kind::Method
                },
                qualify(name),
                *target,
            ),
        ));
    }
    for field in &class.fields {
        let sym = symbol::intern(&field.name);
        members.push((
            field.name.clone(),
            Target::new(
                Kind::Getter(sym),
                qualify(&field.name),
                Callable::Dynamic(Value::null()),
            ),
        ));
        members.push((
            format!("{}=(_)", field.name),
            Target::new(
                Kind::Setter(sym),
                qualify(&field.name),
                Callable::Dynamic(Value::null()),
            ),
        ));
    }
    // A static field is a static getter and setter on the class object.
    for field in &class.statics {
        let sym = symbol::intern(&field.name);
        members.push((
            format!("static:{}", field.name),
            Target::new(
                Kind::ClassGetter(sym),
                qualify(&field.name),
                Callable::Dynamic(class.class_object),
            ),
        ));
        members.push((
            format!("static:{}=(_)", field.name),
            Target::new(
                Kind::ClassSetter(sym),
                qualify(&field.name),
                Callable::Dynamic(class.class_object),
            ),
        ));
    }
    members
}

/// Bind one native per member of `class` onto the installed `ptr`.
fn bind(vm: &mut VM, ptr: *mut ObjClass, class: &ClassIface) -> Result<ClassBinding, ImportError> {
    bind_members(vm, ptr, &class.name, members(class))
}

/// Bind one native per `(signature, target)` onto the installed `ptr`.
fn bind_members(
    vm: &mut VM,
    ptr: *mut ObjClass,
    name: &str,
    members: Vec<(String, Target)>,
) -> Result<ClassBinding, ImportError> {
    if members.len() > SLOTS {
        return Err(ImportError(format!(
            "`{name}` has {} members; a class can bind at most {SLOTS}",
            members.len()
        )));
    }
    let mut targets: Vec<Target> = Vec::with_capacity(members.len());
    for (sig, target) in members {
        let sym = vm.interner.intern(&sig);
        unsafe { (*ptr).bind_native(sym, trampoline(targets.len())) };
        targets.push(target);
    }
    Ok(ClassBinding { targets })
}

// ---------------------------------------------------------------------------
// Instances
// ---------------------------------------------------------------------------

/// The handle in an instance's field, or null.
unsafe fn handle_of(instance: *mut ObjInstance) -> Handle {
    let v = unsafe { (*instance).get_field(HANDLE_FIELD) }.unwrap_or(WValue::null());
    match v.as_num() {
        Some(n) if n > 0.0 => Handle::from_raw(n as u32),
        _ => Handle::NULL,
    }
}

/// Root `obj` from `instance`'s fields, recording it as live.
fn adopt(rec: &WrenHeap, instance: *mut ObjInstance, obj: *mut u8) {
    let handle = heap::handle_new(obj);
    unsafe {
        (*instance).set_field(HANDLE_FIELD, WValue::num(f64::from(handle.as_raw())));
        (*instance).set_field(OBJECT_FIELD, WValue::num(obj as usize as f64));
    }
    rec.imports()
        .borrow_mut()
        .live
        .insert(instance as usize, handle);
}

/// The object an instance of an installed class stands for, as the bridge
/// value it crossed as; `None` for an instance of any other class, or one
/// whose constructor never reached the installed class's. Only `adopt`
/// writes the fields, so a handle in the first means the second is the
/// object, and no lock or lookup is needed.
pub(crate) fn foreign_of(v: WValue) -> Option<Value> {
    let ptr = v.as_object()?;
    if unsafe { (*(ptr as *const ObjHeader)).obj_type } != ObjType::Instance {
        return None;
    }
    let instance = ptr as *mut ObjInstance;
    if unsafe { handle_of(instance) }.is_null() {
        return None;
    }
    let obj = unsafe { (*instance).get_field(OBJECT_FIELD) }?.as_num()? as usize;
    (obj != 0).then(|| Value::object(obj as *const c_void))
}

/// A foreign object as an instance of the class installed for its type,
/// installing the class's module on first need. `None` when its language
/// publishes no class for it.
/// The class a foreign function is an instance of: `Function`, in the
/// bridge's own module, answering `call` with up to `MAX_CALL_ARITY`
/// arguments and `arity`, as a `Fn` does. Installed on first need.
pub const FUNCTION_CLASS: &str = "Function";
const FUNCTION_MODULE: &str = "caribou:Function";
const MAX_CALL_ARITY: usize = 8;

/// The most parameters a Wren signature takes.
const WIDEST: usize = 16;

fn function_class(vm: &mut VM) -> Result<*mut ObjClass, ImportError> {
    let rec = record_for(vm.object_class as *mut u8);
    if let Some(class) = rec.imports().borrow().function {
        return Ok(class);
    }
    let shell = Interface {
        lang: caribou::world::LANG_CORE,
        module: FUNCTION_MODULE.to_owned(),
        classes: vec![ClassIface {
            name: FUNCTION_CLASS.to_owned(),
            type_name: FUNCTION_CLASS.to_owned(),
            superclass: None,
            fields: Vec::new(),
            statics: Vec::new(),
            methods: Vec::new(),
            ctor: None,
            class_object: Value::null(),
        }],
    };
    let bytes = blob(&shell).map_err(|e| ImportError(format!("`{FUNCTION_MODULE}`: {e}")))?;
    if vm.interpret_bytecode(FUNCTION_MODULE, &bytes) != InterpretResult::Success {
        return Err(ImportError(format!("`{FUNCTION_MODULE}` did not install")));
    }
    let value = vm
        .find_imported_var_from(FUNCTION_CLASS, FUNCTION_MODULE)
        .ok_or_else(|| ImportError(format!("`{FUNCTION_MODULE}` installed no class")))?;
    let ptr = value.as_object().unwrap_or(std::ptr::null_mut()) as *mut ObjClass;
    let mut members: Vec<(String, Target)> = Vec::with_capacity(MAX_CALL_ARITY + 2);
    for arity in 0..=MAX_CALL_ARITY {
        members.push((
            signature("call", arity),
            Target::new(
                Kind::Call,
                format!("{FUNCTION_CLASS}.call"),
                Callable::Dynamic(Value::null()),
            ),
        ));
    }
    members.push((
        "arity".to_owned(),
        Target::new(
            Kind::Arity,
            format!("{FUNCTION_CLASS}.arity"),
            Callable::Dynamic(Value::null()),
        ),
    ));
    let binding = bind_members(vm, ptr, FUNCTION_CLASS, members)?;
    let mut imports = rec.imports().borrow_mut();
    imports.classes.insert(ptr as usize, Rc::new(binding));
    imports.function = Some(ptr);
    Ok(ptr)
}

/// The instance of an installed class that stands for `v`: of the class
/// installed for its type, or of `Function` when `v` is a function.
pub(crate) fn proxy(vm: &mut VM, v: Value) -> Option<WValue> {
    let obj = v.as_object()? as *mut u8;
    if obj.is_null() || !crate::installed() {
        return None;
    }
    let lang = bridge::language_of(v)?;
    let rec = record_for(vm.object_class as *mut u8);
    // `v` is unrooted on the caller's frame, and installing and allocating
    // the instance both allocate.
    let root = heap::handle_new(obj);
    let class = if bridge::arity(v).is_some() {
        function_class(vm).ok()
    } else if let Some(type_name) = bridge::type_name(v) {
        let known = rec
            .imports()
            .borrow()
            .by_type
            .get(&(lang, type_name.clone()))
            .copied();
        known.or_else(|| {
            let (iface, _) = registry::class_for_type(lang, &type_name)?;
            install(vm, lang, &iface.module).ok()?;
            rec.imports()
                .borrow()
                .by_type
                .get(&(lang, type_name))
                .copied()
        })
    } else {
        None
    };
    let instance = class.map(|class| {
        let instance = vm.alloc_instance(class);
        adopt(rec, instance.as_object().unwrap() as *mut ObjInstance, obj);
        instance
    });
    heap::handle_release(root);
    instance
}

// ---------------------------------------------------------------------------
// Trampolines
// ---------------------------------------------------------------------------

macro_rules! row {
    ($hi:literal; $($lo:literal),*) => {
        [$({
            fn t(ctx: &mut dyn NativeContext, args: &[WValue]) -> WValue {
                dispatch($hi + $lo, ctx, args)
            }
            t as NativeFn
        }),*]
    };
}

macro_rules! rows {
    ($($hi:literal),*) => {
        [$(row!($hi; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15)),*]
    };
}

static TRAMPOLINES: [[NativeFn; 16]; 16] = rows!(
    0, 16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240
);

fn trampoline(slot: usize) -> NativeFn {
    TRAMPOLINES[slot >> 4][slot & 15]
}

/// A Wren argument as a bridge value, with the root it needs for the
/// call: an instance of an installed class becomes the object it stands
/// for; a string becomes a fresh core `Str`, rooted here.
fn cross_in(v: WValue) -> (Value, Handle) {
    if let Some(obj) = foreign_of(v) {
        return (obj, Handle::NULL);
    }
    let crossed = from_wren(v);
    let root = match crossed.as_object() {
        Some(p) if v.is_string_object() => heap::handle_new(p as *mut u8),
        _ => Handle::NULL,
    };
    (crossed, root)
}

/// A bridge result as a Wren value.
fn cross_out(vm: &mut VM, v: Value) -> Result<WValue, String> {
    to_wren(vm, v).ok_or_else(|| format!("{} cannot cross into Wren", bridge::describe(v)))
}

/// The message of an error the bridge returned to Wren.
fn message_of(err: Value) -> String {
    match unsafe { Error::from_value(err) } {
        Some(e) => {
            let message = unsafe { Error::message_str(e) };
            if message.is_empty() {
                "error".to_owned()
            } else {
                message.to_owned()
            }
        }
        None => match unsafe { Str::text(err) } {
            Some(text) => text.to_owned(),
            None => bridge::describe(err),
        },
    }
}

fn dispatch(slot: usize, ctx: &mut dyn NativeContext, args: &[WValue]) -> WValue {
    // `ctx` is the VM itself, which is the only `NativeContext` wren_lift
    // dispatches a native with; its data pointer is the VM's address, and
    // is what the entered VM would answer without the thread-local read.
    let vm = ctx as *mut dyn NativeContext as *mut VM;
    debug_assert!(vm == current_vm(), "the native's context is the entered VM");
    let vm = unsafe { &mut *vm };
    match run(vm, slot, args) {
        Ok(v) => v,
        Err(message) => {
            vm.runtime_error(message);
            WValue::null()
        }
    }
}

fn run(vm: &mut VM, slot: usize, args: &[WValue]) -> Result<WValue, String> {
    let recv = args[0];
    let Some(obj) = recv.as_object() else {
        return Err("the receiver is not an object".to_owned());
    };
    let is_class = unsafe { (*(obj as *const ObjHeader)).obj_type } == ObjType::Class;
    let class = if is_class {
        obj as *mut ObjClass
    } else {
        vm.class_of(recv)
    };
    let rec = record_for(obj);
    let Some(binding) = rec.imports().borrow().binding_of(class) else {
        return Err(format!(
            "{} is not an imported class",
            vm.class_name_of(recv)
        ));
    };
    let target = &binding.targets[slot];
    let wren = wren_lang();

    // Arguments cross first, rooted for the call. On the stack: Wren's
    // widest signature, with a slot before them for `this`; only the slots
    // in use are written.
    let n = args.len() - 1;
    let mut roots = [Handle::NULL; WIDEST];
    let mut buf = [MaybeUninit::<Value>::uninit(); WIDEST + 1];
    if n > WIDEST {
        return Err(format!("{} takes too many arguments", target.name));
    }
    let release = |roots: &[Handle]| {
        for &h in roots {
            if !h.is_null() {
                heap::handle_release(h);
            }
        }
    };
    buf[0].write(Value::null());
    for (i, &arg) in args[1..].iter().enumerate() {
        let (v, root) = cross_in(arg);
        buf[1 + i].write(v);
        roots[i] = root;
    }
    let roots = &roots[..n];
    // Slot 0 is `this` for a method and unused otherwise.
    let with_this = unsafe { buf[..=n].assume_init_mut() };

    let result = match target.kind {
        Kind::Ctor => {
            // A subclass's constructor arrives with its instance already
            // made; a call on the class makes one.
            let instance = if is_class {
                vm.alloc_instance(class)
            } else {
                recv
            };
            let ptr = instance.as_object().unwrap() as *mut ObjInstance;
            let made = bridge::call_named(target.callable, &with_this[1..], wren, &target.name);
            release(roots);
            let made = made.map_err(message_of)?;
            let Some(haxe) = made.as_object().filter(|p| !p.is_null()) else {
                return Err(format!(
                    "{} returned {}",
                    target.name,
                    bridge::describe(made)
                ));
            };
            adopt(rec, ptr, haxe as *mut u8);
            return Ok(instance);
        }
        Kind::Static => {
            let r = bridge::call_at(
                target.callable,
                &target.site,
                &with_this[1..],
                wren,
                &target.name,
            );
            release(roots);
            r
        }
        Kind::Call | Kind::Arity => {
            let this = foreign_of(recv).ok_or_else(|| {
                release(roots);
                format!("{} has no function behind it", vm.class_name_of(recv))
            })?;
            let r = match target.kind {
                Kind::Call => {
                    bridge::call_named(Callable::Dynamic(this), &with_this[1..], wren, &target.name)
                }
                _ => match bridge::arity(this) {
                    Some(n) => Ok(Value::number(n as f64)),
                    None => Err(Error::value(Error::new(
                        ErrorKind::Type,
                        "not a function",
                        wren,
                    ))),
                },
            };
            release(roots);
            r
        }
        Kind::ClassGetter(name) | Kind::ClassSetter(name) => {
            let Callable::Dynamic(class_object) = target.callable else {
                unreachable!("a static field's target is its class object");
            };
            let r = match target.kind {
                Kind::ClassGetter(_) => bridge::get_at(class_object, name, &target.site, wren),
                _ => bridge::set_at(class_object, name, &target.site, with_this[1], wren)
                    .map(|()| with_this[1]),
            };
            release(roots);
            r
        }
        Kind::Method | Kind::Getter(_) | Kind::Setter(_) => {
            let this = foreign_of(recv).ok_or_else(|| {
                release(roots);
                format!("{} has no object behind it", vm.class_name_of(recv))
            })?;
            let r = match target.kind {
                Kind::Method => {
                    with_this[0] = this;
                    bridge::call_at(target.callable, &target.site, with_this, wren, &target.name)
                }
                Kind::Getter(name) => bridge::get_at(this, name, &target.site, wren),
                Kind::Setter(name) => bridge::set_at(this, name, &target.site, with_this[1], wren)
                    .map(|()| with_this[1]),
                _ => unreachable!(),
            };
            release(roots);
            r
        }
    };
    let value = result.map_err(message_of)?;
    if let Kind::Setter(_) | Kind::ClassSetter(_) = target.kind {
        // The assigned value, as Wren's own setters evaluate to.
        return Ok(args[1]);
    }
    // Rooted before anything can allocate: the result is not.
    let root = match value.as_object() {
        Some(p) if !p.is_null() => heap::handle_new(p as *mut u8),
        _ => Handle::NULL,
    };
    let out = cross_out(vm, value);
    heap::handle_release(root);
    out
}

impl Drop for Imports {
    fn drop(&mut self) {
        debug_assert!(
            self.live.is_empty(),
            "handles released before the record drops"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_and_slots() {
        assert_eq!(signature("new", 0), "new()");
        assert_eq!(signature("hit", 1), "hit(_)");
        assert_eq!(signature("spawnAt", 2), "spawnAt(_,_)");
        // Every slot is its own function.
        let mut seen = std::collections::HashSet::new();
        for slot in 0..SLOTS {
            assert!(
                seen.insert(trampoline(slot) as usize),
                "slot {slot} repeats"
            );
        }
    }

    /// The blob wren_lift installs: one class per published class, the
    /// handle field and nothing else; the members are bound afterwards.
    #[test]
    fn the_blob_holds_the_class_shell_and_members_bind_by_slot() {
        use caribou::registry::{FieldIface, TypeRef};
        let player = ClassIface {
            name: "Player".to_owned(),
            type_name: "game.Player".to_owned(),
            superclass: None,
            fields: vec![
                FieldIface {
                    name: "hp".to_owned(),
                    ty: TypeRef::Int,
                },
                FieldIface {
                    name: "name".to_owned(),
                    ty: TypeRef::Str,
                },
            ],
            statics: vec![FieldIface {
                name: "spawned".to_owned(),
                ty: TypeRef::Int,
            }],
            methods: vec![
                MethodIface {
                    name: "hit".to_owned(),
                    is_static: false,
                    params: vec![TypeRef::Int],
                    ret: TypeRef::Bool,
                    target: Callable::Dynamic(Value::null()),
                },
                MethodIface {
                    name: "spawnAt".to_owned(),
                    is_static: true,
                    params: vec![TypeRef::Float, TypeRef::Float],
                    ret: TypeRef::Object("game.Player".to_owned()),
                    target: Callable::Dynamic(Value::null()),
                },
            ],
            ctor: Some(MethodIface {
                name: "new".to_owned(),
                is_static: true,
                params: vec![TypeRef::Str],
                ret: TypeRef::Object("game.Player".to_owned()),
                target: Callable::Dynamic(Value::null()),
            }),
            class_object: Value::null(),
        };
        let iface = Interface {
            lang: 7,
            module: "game.Player".to_owned(),
            classes: vec![player.clone()],
        };
        let bytes = blob(&iface).unwrap();
        let decoded = serialize::load(&bytes).unwrap();
        assert_eq!(decoded.var_names, ["Player"]);
        assert_eq!(decoded.var_sources, [None]);
        assert_eq!(decoded.class_field_names["Player"], FIELD_NAMES);
        assert_eq!(decoded.module.classes.len(), 1);
        let class = &decoded.module.classes[0];
        assert_eq!(decoded.interner.resolve(class.name), "Player");
        assert_eq!(class.num_fields as usize, FIELD_NAMES.len());
        assert!(class.methods.is_empty() && class.foreign_methods.is_empty());
        assert!(class.native_library.is_none() && class.superclass.is_none());
        assert!(decoded.module.closures.is_empty());
        assert_eq!(decoded.module.top_level.blocks.len(), 1);
        let bound: Vec<String> = members(&player)
            .iter()
            .map(|(sig, t)| format!("{sig} -> {}", t.name))
            .collect();
        assert_eq!(
            bound,
            [
                "static:new(_) -> Player.new",
                "hit(_) -> Player.hit",
                "static:spawnAt(_,_) -> Player.spawnAt",
                "hp -> Player.hp",
                "hp=(_) -> Player.hp",
                "name -> Player.name",
                "name=(_) -> Player.name",
                "static:spawned -> Player.spawned",
                "static:spawned=(_) -> Player.spawned",
            ]
        );
        if std::env::var_os("CARIBOU_DUMP_BLOB").is_some() {
            println!("{class:#?}");
            println!(
                "top level: {:#?}",
                decoded.module.top_level.blocks[0].terminator
            );
            for (i, line) in bound.iter().enumerate() {
                println!("slot {i}: {line} ({:#x})", trampoline(i) as usize);
            }
        }
    }

    #[test]
    fn only_known_namespaces_are_namespaced() {
        assert_eq!(namespaced("./foo"), None);
        assert_eq!(namespaced("@hatch:window"), None);
        assert_eq!(namespaced("nowhere:Player"), None);
        assert_eq!(namespaced(":x"), None);
    }
}
