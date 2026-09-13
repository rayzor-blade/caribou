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

use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::rc::Rc;

use caribou::bridge;
use caribou::error::{Error, Str};
use caribou::heap::{self, Handle};
use caribou::protocol::Callable;
use caribou::registry::{self, ClassIface, Interface, MethodIface};
use caribou::symbol::{self, Symbol};
use caribou::world::language_name;
use caribou_abi::{LangId, Value};
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

/// The field of every instance that holds the handle.
const HANDLE_FIELD: usize = 0;
/// Its name in the class's field layout: not a name Wren source can spell,
/// so a subclass's own fields never alias it.
const HANDLE_FIELD_NAME: &str = "__caribou_handle";

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
}

#[derive(Clone)]
struct Target {
    kind: Kind,
    /// For the trace frame.
    name: String,
    callable: Callable,
}

/// One installed class: per slot, the member it binds.
struct ClassBinding {
    targets: Vec<Target>,
}

/// The installed classes of one VM, kept on its heap record so they die
/// with it. Touched only by the VM's own thread.
#[derive(Default)]
pub(crate) struct Imports {
    classes: HashMap<usize, Rc<ClassBinding>>,
    /// `(lang, type name)` to the class that stands for it.
    by_type: HashMap<(LangId, String), *mut ObjClass>,
    /// Every live instance and the handle in its field, for the sweep and
    /// for `heap_drop`.
    live: HashMap<usize, Handle>,
}

impl Imports {
    /// Whether `class` was installed here for another language's.
    pub(crate) fn installed(&self, class: *mut ObjClass) -> bool {
        self.classes.contains_key(&(class as usize))
    }

    /// The binding of `class` or of its nearest bound superclass.
    fn binding_of(&self, mut class: *mut ObjClass) -> Option<Rc<ClassBinding>> {
        while !class.is_null() {
            if let Some(b) = self.classes.get(&(class as usize)) {
                return Some(b.clone());
            }
            class = unsafe { (*class).superclass };
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
/// installed module, installing it on first use; other names go to the
/// host's callbacks. The VM must be entered on the thread that runs it.
pub fn configure(config: &mut VMConfig) {
    let previous_resolve = config.resolve_module_fn.take();
    config.resolve_module_fn = Some(Box::new(move |name: &str, from: &str| {
        if let Some((ns, module)) = namespaced(name)
            && let Some((lang, module)) = registry::resolve(ns, module)
        {
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
        previous_resolve.as_ref().and_then(|f| f(name, from))
    }));
    let previous_load = config.load_module_fn.take();
    config.load_module_fn = Some(Box::new(move |name: &str, from: &str| {
        if namespaced(name).is_some() {
            return None;
        }
        previous_load.as_ref().and_then(|f| f(name, from))
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
            class_field_names.insert(class.name.clone(), vec![HANDLE_FIELD_NAME.to_owned()]);
            ClassMir {
                name: interner.intern(&class.name),
                superclass: None,
                methods: Vec::new(),
                num_fields: 1,
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
            Target {
                kind: Kind::Ctor,
                name: qualify("new"),
                callable: ctor.target,
            },
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
            Target {
                kind: if *is_static {
                    Kind::Static
                } else {
                    Kind::Method
                },
                name: qualify(name),
                callable: *target,
            },
        ));
    }
    for field in &class.fields {
        let sym = symbol::intern(&field.name);
        members.push((
            field.name.clone(),
            Target {
                kind: Kind::Getter(sym),
                name: qualify(&field.name),
                callable: Callable::Dynamic(Value::null()),
            },
        ));
        members.push((
            format!("{}=(_)", field.name),
            Target {
                kind: Kind::Setter(sym),
                name: qualify(&field.name),
                callable: Callable::Dynamic(Value::null()),
            },
        ));
    }
    members
}

/// Bind one native per member of `class` onto the installed `ptr`.
fn bind(vm: &mut VM, ptr: *mut ObjClass, class: &ClassIface) -> Result<ClassBinding, ImportError> {
    let members = members(class);
    if members.len() > SLOTS {
        return Err(ImportError(format!(
            "`{}` has {} members; a class can bind at most {SLOTS}",
            class.name,
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

/// Root `obj` from `instance`'s field, recording it as live.
fn adopt(rec: &WrenHeap, instance: *mut ObjInstance, obj: *mut u8) {
    let handle = heap::handle_new(obj);
    unsafe { (*instance).set_field(HANDLE_FIELD, WValue::num(f64::from(handle.as_raw()))) };
    rec.imports()
        .borrow_mut()
        .live
        .insert(instance as usize, handle);
}

/// The object an instance of an installed class stands for, as the bridge
/// value it crossed as; `None` for an instance of any other class.
pub(crate) fn foreign_of(v: WValue) -> Option<Value> {
    let ptr = v.as_object()?;
    if unsafe { (*(ptr as *const ObjHeader)).obj_type } != ObjType::Instance {
        return None;
    }
    let rec = record_for(ptr);
    if !rec.imports().borrow().live.contains_key(&(ptr as usize)) {
        return None;
    }
    let obj = heap::handle_get(unsafe { handle_of(ptr as *mut ObjInstance) });
    (!obj.is_null()).then(|| Value::object(obj as *const c_void))
}

/// A foreign object as an instance of the class installed for its type,
/// installing the class's module on first need. `None` when its language
/// publishes no class for it.
pub(crate) fn proxy(vm: &mut VM, v: Value) -> Option<WValue> {
    let obj = v.as_object()? as *mut u8;
    if obj.is_null() || !crate::installed() {
        return None;
    }
    let lang = bridge::language_of(v)?;
    let type_name = bridge::type_name(v)?;
    let rec = record_for(vm.object_class as *mut u8);
    let class = rec
        .imports()
        .borrow()
        .by_type
        .get(&(lang, type_name.clone()))
        .copied();
    // `v` is unrooted on the caller's frame, and installing and allocating
    // the instance both allocate.
    let root = heap::handle_new(obj);
    let class = class.or_else(|| {
        let (iface, _) = registry::class_for_type(lang, &type_name)?;
        install(vm, lang, &iface.module).ok()?;
        rec.imports()
            .borrow()
            .by_type
            .get(&(lang, type_name))
            .copied()
    });
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
    let vm = current_vm();
    if vm.is_null() {
        ctx.runtime_error("no Wren VM is entered on this thread".to_owned());
        return WValue::null();
    }
    // The same VM `ctx` is; `ctx` is not touched again.
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

    // Arguments cross first, rooted for the call.
    let mut roots: Vec<Handle> = Vec::new();
    let mut crossed: Vec<Value> = Vec::with_capacity(args.len());
    let release = |roots: &[Handle]| {
        for &h in roots {
            heap::handle_release(h);
        }
    };
    for &arg in &args[1..] {
        let (v, root) = cross_in(arg);
        crossed.push(v);
        if !root.is_null() {
            roots.push(root);
        }
    }

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
            let made = bridge::call_named(target.callable, &crossed, wren, &target.name);
            release(&roots);
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
            let r = bridge::call_named(target.callable, &crossed, wren, &target.name);
            release(&roots);
            r
        }
        Kind::Method | Kind::Getter(_) | Kind::Setter(_) => {
            let this = foreign_of(recv).ok_or_else(|| {
                release(&roots);
                format!("{} has no object behind it", vm.class_name_of(recv))
            })?;
            let r = match target.kind {
                Kind::Method => {
                    let mut with_this = Vec::with_capacity(crossed.len() + 1);
                    with_this.push(this);
                    with_this.extend_from_slice(&crossed);
                    bridge::call_named(target.callable, &with_this, wren, &target.name)
                }
                Kind::Getter(name) => bridge::get(this, name, wren),
                Kind::Setter(name) => {
                    bridge::set(this, name, crossed[0], wren).map(|()| crossed[0])
                }
                _ => unreachable!(),
            };
            release(&roots);
            r
        }
    };
    let value = result.map_err(message_of)?;
    if let Kind::Setter(_) = target.kind {
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
        assert_eq!(
            decoded.class_field_names["Player"],
            [HANDLE_FIELD_NAME.to_owned()]
        );
        assert_eq!(decoded.module.classes.len(), 1);
        let class = &decoded.module.classes[0];
        assert_eq!(decoded.interner.resolve(class.name), "Player");
        assert_eq!(class.num_fields, 1);
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
