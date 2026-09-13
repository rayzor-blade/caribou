//! A Wren module's classes as a registry interface: what another language
//! reaches as `import "wren:hud"` or, through a namespace, `game:hud`.
//!
//! Every class the module defines is described from what the VM built for
//! it: its name, its superclass unless that is Object, the field names in
//! the VM's layout for it, and its method table. A table entry is Wren's
//! signature: `draw()`, `hit(_)`, `score` for a getter, `score=(_)` for a
//! setter, and under `static:` the class's own side, where a constructor is
//! `static:new(_)`. The table is a copy of the superclass's plus the class's
//! own entries, so the class defines an entry where it differs from the
//! superclass's at the same slot. Parameters and results are `Dyn`; Wren
//! declares no types. Operators and subscripts have no name an importer
//! can spell and are not published. A class is the module's when one of
//! its own methods was compiled in it; a class installed for another
//! language's is not.
//!
//! Each member's target is a `Callable::WrenMethod`: the class, the
//! signature, and whether the class or the first argument receives it. The
//! bridge sends it through the object protocol, so the call is the one
//! Wren code would make. The class stays valid while its module does, and
//! the VM must be entered on the calling thread. One constructor is the
//! class's `ctor`, `new` when there is one; any other is a static method
//! returning the class.
//!
//! The module's name is the registry's module (`hud` for `import "hud"`),
//! and `hud.Hud` is what an instance reports through `type_name`, kept on
//! the heap record for the protocol to answer.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use caribou::protocol::Callable;
use caribou::registry::{self, ClassIface, FieldIface, Interface, MethodIface, TypeRef};
use caribou::symbol;
use caribou::world::RegisterError;
use wren_lift::intern::SymbolId;
use wren_lift::runtime::engine::FuncId;
use wren_lift::runtime::object::{Method, ObjClass, ObjHeader, ObjType};
use wren_lift::runtime::value::Value as WValue;
use wren_lift::runtime::vm::VM;

use crate::heap::{record_for, wren_lang};
use crate::proto::from_wren;
use crate::types::Export;

/// The classes of one heap in the registry: each class to the type name
/// its instances report.
#[derive(Default)]
pub(crate) struct Exports {
    types: HashMap<usize, String>,
}

impl Exports {
    pub(crate) fn type_name(&self, class: *mut ObjClass) -> Option<&str> {
        self.types.get(&(class as usize)).map(String::as_str)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PublishError {
    /// The VM has loaded no module of that name.
    NoModule(String),
    /// The registry refused the interface.
    Refused(RegisterError),
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoModule(name) => write!(f, "no module `{name}` is loaded"),
            Self::Refused(e) => write!(f, "the registry refused the module: {e}"),
        }
    }
}

impl std::error::Error for PublishError {}

/// Publish every class `module` defines, under Wren's language, and answer
/// the interface. Publishing again replaces the earlier interface.
pub fn publish_module(vm: &VM, module: &str) -> Result<Arc<Interface>, PublishError> {
    let entry = vm
        .engine
        .modules
        .get(module)
        .ok_or_else(|| PublishError::NoModule(module.to_owned()))?;
    let rec = record_for(vm.object_class as *mut u8);
    let mut seen: Vec<*mut ObjClass> = Vec::new();
    for &value in &entry.vars {
        let Some(class) = class_of_module(vm, value, module) else {
            continue;
        };
        if seen.contains(&class) || rec.imports().borrow().installed(class) {
            continue;
        }
        seen.push(class);
    }
    // Every class's name first: a declared type may name any of them.
    let names: Vec<String> = seen
        .iter()
        .map(|&c| vm.interner.resolve(unsafe { (*c).name }).to_owned())
        .collect();
    let classes = seen
        .iter()
        .map(|&class| describe(vm, class, module, &names))
        .collect();
    let iface = Interface {
        lang: wren_lang(),
        module: module.to_owned(),
        classes,
    };
    registry::publish(iface.clone()).map_err(PublishError::Refused)?;
    let mut exports = rec.exports().borrow_mut();
    for (class, described) in seen.iter().zip(&iface.classes) {
        exports
            .types
            .insert(*class as usize, described.type_name.clone());
    }
    Ok(Arc::new(iface))
}

/// `value` as a class one of whose own methods was compiled in `module`.
fn class_of_module(vm: &VM, value: WValue, module: &str) -> Option<*mut ObjClass> {
    let p = value.as_object()?;
    if unsafe { (*(p as *const ObjHeader)).obj_type } != ObjType::Class {
        return None;
    }
    let class = p as *mut ObjClass;
    let defined_here = own_methods(class).any(|(_, m)| match m {
        Method::Closure(closure) | Method::Constructor(closure) => {
            let id = FuncId(unsafe { (*(*closure).function).fn_id });
            vm.engine
                .func_module(id)
                .is_some_and(|m| m.as_str() == module)
        }
        _ => false,
    });
    defined_here.then_some(class)
}

/// The entries of `class`'s method table that differ from the
/// superclass's, with their slots.
fn own_methods(class: *mut ObjClass) -> impl Iterator<Item = (usize, Method)> {
    let c = unsafe { &*class };
    let superclass = unsafe { c.superclass.as_ref() };
    c.methods.iter().enumerate().filter_map(move |(slot, m)| {
        let m = (*m)?;
        let inherited = superclass
            .and_then(|s| s.methods.get(slot))
            .and_then(|s| *s)
            .is_some_and(|s| same_method(s, m));
        (!inherited).then_some((slot, m))
    })
}

fn same_method(a: Method, b: Method) -> bool {
    match (a, b) {
        (Method::Closure(x), Method::Closure(y))
        | (Method::Constructor(x), Method::Constructor(y)) => x == y,
        (Method::Native(x), Method::Native(y)) => x as usize == y as usize,
        (Method::ForeignC(x), Method::ForeignC(y)) => x as usize == y as usize,
        (Method::ForeignCDynamic(x), Method::ForeignCDynamic(y)) => x == y,
        _ => false,
    }
}

/// How a signature is spelled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Getter,
    Setter,
    Method,
}

/// A signature's name, arity and shape; `None` for an operator or a
/// subscript.
fn parse(sig: &str) -> Option<(&str, usize, Shape)> {
    let (name, rest) = match sig.find(['(', '=']) {
        Some(i) => sig.split_at(i),
        None => (sig, ""),
    };
    if !is_identifier(name) {
        return None;
    }
    if rest.is_empty() {
        return Some((name, 0, Shape::Getter));
    }
    if let Some(params) = rest.strip_prefix("=(").and_then(|r| r.strip_suffix(')')) {
        return (params == "_").then_some((name, 1, Shape::Setter));
    }
    let params = rest.strip_prefix('(')?.strip_suffix(')')?;
    let arity = if params.is_empty() {
        0
    } else {
        params.split(',').count()
    };
    Some((name, arity, Shape::Method))
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn describe(vm: &VM, class: *mut ObjClass, module: &str, names: &[String]) -> ClassIface {
    let name = vm.interner.resolve(unsafe { (*class).name }).to_owned();
    let type_name = format!("{module}.{name}");
    let superclass = unsafe { (*class).superclass };
    let superclass = (!superclass.is_null() && superclass != vm.object_class).then(|| {
        vm.interner
            .resolve(unsafe { (*superclass).name })
            .to_owned()
    });
    let fields = vm
        .field_layouts
        .get(&name)
        .map(|layout| {
            layout
                .iter()
                .map(|f| FieldIface {
                    name: f.clone(),
                    ty: TypeRef::Dyn,
                })
                .collect()
        })
        .unwrap_or_default();
    let class_value = from_wren(WValue::object(class as *mut u8));

    let mut methods = Vec::new();
    let mut ctors = Vec::new();
    for (slot, method) in own_methods(class) {
        let sig = vm.interner.resolve(SymbolId::from_raw(slot as u32));
        let (sig, is_static) = match sig.strip_prefix("static:") {
            Some(rest) => (rest, true),
            None => (sig, false),
        };
        let Some((base, arity, _)) = parse(sig) else {
            continue;
        };
        let is_constructor = matches!(method, Method::Constructor(_));
        // What its `#export` attribute says, read off the running class; an
        // attribute that does not fit leaves the member as Wren spells it.
        let export = unsafe {
            (*class)
                .method_attributes
                .get(&SymbolId::from_raw(slot as u32))
        }
        .and_then(|entries| Export::from_entries(entries).ok().flatten())
        .filter(|e| e.params.len() == arity);
        let member = MethodIface {
            name: export.as_ref().map_or(base, |e| e.name.as_str()).to_owned(),
            is_static,
            params: (0..arity)
                .map(|i| {
                    export
                        .as_ref()
                        .map_or(TypeRef::Dyn, |e| e.param(i, module, names))
                })
                .collect(),
            ret: if is_constructor {
                TypeRef::Object(type_name.clone())
            } else {
                export
                    .as_ref()
                    .and_then(|e| e.ret(module, names))
                    .unwrap_or(TypeRef::Dyn)
            },
            target: Callable::WrenMethod {
                class: class_value,
                signature: symbol::intern(sig),
                is_static,
            },
        };
        if is_constructor {
            ctors.push(member);
        } else {
            methods.push(member);
        }
    }
    let ctor = (!ctors.is_empty()).then(|| {
        let pick = ctors.iter().position(|c| c.name == "new").unwrap_or(0);
        ctors.remove(pick)
    });
    methods.extend(ctors);
    ClassIface {
        name,
        type_name,
        superclass,
        fields,
        // A Wren class keeps its static state behind static getters and
        // setters, which are methods.
        statics: Vec::new(),
        methods,
        ctor,
        class_object: class_value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{immix_vm, parent_of};
    use crate::with_vm;
    use caribou::bridge;
    use caribou::error::Str;
    use caribou::heap;
    use caribou::registry::{MethodKind, class_for_type};
    use caribou::world::{Config, LANG_CORE, World};
    use caribou_abi::Value;
    use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};

    use crate::heap::record_for;

    const HUD: &str = r#"
class Hud {
  construct new(p) { _p = p }
  draw() { }
  score { 3 }
  score=(v) { _p = v }
  #export = "spawn(p: Num) -> Hud"
  static make(p) { return Hud.new(p) }
  +(other) { this }
}
class Panel is Hud {
  construct new(p) { super(p) }
  construct blank() { _p = 0 }
  title { "panel" }
}
var Alias = Hud
"#;

    /// One member's shape: name, kind, static, arity.
    fn shape(m: &MethodIface) -> (String, &'static str, bool, usize) {
        let kind = match m.kind() {
            MethodKind::Method => "method",
            MethodKind::Getter => "getter",
            MethodKind::Setter => "setter",
        };
        (m.name.clone(), kind, m.is_static, m.params.len())
    }

    #[test]
    fn a_modules_classes_publish_with_their_members() {
        if parent_of(
            "publish::tests::a_modules_classes_publish_with_their_members",
            &[],
        ) {
            return;
        }
        crate::install().expect("a fresh process takes the table");
        let mut world = World::new(Config::default());
        let wren = world
            .register(Box::new(crate::Runtime::new()))
            .expect("wren registers")[0];
        let mut vm = immix_vm(ExecutionMode::Interpreter);
        assert_eq!(vm.interpret("hud", HUD), InterpretResult::Success);

        assert_eq!(
            publish_module(&vm, "nope").err(),
            Some(PublishError::NoModule("nope".to_owned()))
        );
        let iface = publish_module(&vm, "hud").expect("the module publishes");
        assert_eq!(iface.lang, wren);
        assert_eq!(iface.module, "hud");
        let names: Vec<&str> = iface.classes.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["Hud", "Panel"],
            "each class once, the alias not at all"
        );

        let hud = iface.class("Hud").unwrap();
        assert_eq!(hud.type_name, "hud.Hud");
        assert_eq!(hud.superclass, None);
        let fields: Vec<&str> = hud.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(fields, ["p"], "as the VM's layout names it");
        assert!(hud.fields.iter().all(|f| f.ty == TypeRef::Dyn));
        let ctor = hud.ctor.as_ref().expect("a constructor");
        assert_eq!(shape(ctor), ("new".to_owned(), "method", true, 1));
        assert_eq!(ctor.ret, TypeRef::Object("hud.Hud".to_owned()));
        let hud_class = vm.find_imported_var_from("Hud", "hud").unwrap();
        match ctor.target {
            Callable::WrenMethod {
                class,
                signature,
                is_static,
            } => {
                assert_eq!(class.to_bits(), from_wren(hud_class).to_bits());
                assert_eq!(signature.name(), "new(_)");
                assert!(is_static);
            }
            other => panic!("{other:?}"),
        }
        let mut members: Vec<_> = hud.methods.iter().map(shape).collect();
        members.sort();
        assert_eq!(
            members,
            [
                ("draw".to_owned(), "method", false, 0),
                ("score".to_owned(), "getter", false, 0),
                ("score".to_owned(), "setter", false, 1),
                ("spawn".to_owned(), "method", true, 1),
            ],
            "operators are left out"
        );
        // The name and types come from the `#export` the VM kept for the
        // method; the target is still Wren's own signature.
        let make = hud.methods.iter().find(|m| m.name == "spawn").unwrap();
        assert_eq!(make.params, [TypeRef::Float]);
        assert_eq!(make.ret, TypeRef::Object("hud.Hud".to_owned()));
        assert!(matches!(
            make.target,
            Callable::WrenMethod { signature, .. } if signature.name() == "make(_)"
        ));
        assert!(
            hud.methods
                .iter()
                .filter(|m| m.name != "spawn")
                .all(|m| m.ret == TypeRef::Dyn)
        );
        assert!(
            hud.methods
                .iter()
                .filter(|m| m.name != "spawn")
                .all(|m| m.params.iter().all(|p| *p == TypeRef::Dyn))
        );

        // A subclass publishes only what it defines, over the inherited
        // layout; its second constructor is a static factory.
        let panel = iface.class("Panel").unwrap();
        assert_eq!(panel.superclass.as_deref(), Some("Hud"));
        assert_eq!(panel.type_name, "hud.Panel");
        assert_eq!(panel.fields.len(), 1);
        assert_eq!(
            shape(panel.ctor.as_ref().unwrap()),
            ("new".to_owned(), "method", true, 1)
        );
        let mut members: Vec<_> = panel.methods.iter().map(shape).collect();
        members.sort();
        assert_eq!(
            members,
            [
                ("blank".to_owned(), "method", true, 0),
                ("title".to_owned(), "getter", false, 0),
            ]
        );
        assert_eq!(
            panel
                .methods
                .iter()
                .find(|m| m.name == "blank")
                .unwrap()
                .ret,
            TypeRef::Object("hud.Panel".to_owned())
        );
        let (found, index) = class_for_type(wren, "hud.Panel").expect("indexed by type name");
        assert_eq!(found.classes[index].name, "Panel");

        // The members are callable through the bridge, and an instance
        // reports the published type name.
        let member = |class: &ClassIface, name: &str, kind: MethodKind| {
            class
                .methods
                .iter()
                .find(|m| m.name == name && m.kind() == kind)
                .unwrap()
                .target
        };
        with_vm(&mut vm, |_| {
            let h = bridge::call_named(ctor.target, &[Value::number(7.0)], LANG_CORE, "Hud.new")
                .expect("constructed");
            // Nothing in Wren refers to it: the handle is what keeps it.
            let root = heap::handle_new(h.as_object().unwrap() as *mut u8);
            assert_eq!(bridge::type_name(h).as_deref(), Some("hud.Hud"));
            assert_eq!(
                bridge::get(h, caribou::symbol::intern("p"), LANG_CORE).map(|v| v.as_number()),
                Ok(Some(7.0))
            );
            let score = member(hud, "score", MethodKind::Getter);
            assert_eq!(
                bridge::call(score, &[h], LANG_CORE).map(|v| v.as_number()),
                Ok(Some(3.0))
            );
            let set_score = member(hud, "score", MethodKind::Setter);
            bridge::call(set_score, &[h, Value::number(9.0)], LANG_CORE).expect("set");
            assert_eq!(
                bridge::get(h, caribou::symbol::intern("p"), LANG_CORE).map(|v| v.as_number()),
                Ok(Some(9.0))
            );
            assert_eq!(
                bridge::call(member(hud, "draw", MethodKind::Method), &[h], LANG_CORE),
                Ok(Value::null())
            );
            let made = bridge::call(
                member(hud, "spawn", MethodKind::Method),
                &[Value::number(1.0)],
                LANG_CORE,
            )
            .expect("made");
            assert_eq!(bridge::type_name(made).as_deref(), Some("hud.Hud"));
            // A string result crosses as a core string.
            let p = bridge::call(member(panel, "blank", MethodKind::Method), &[], LANG_CORE)
                .expect("a Panel");
            let title = bridge::call(member(panel, "title", MethodKind::Getter), &[p], LANG_CORE)
                .expect("title");
            assert_eq!(unsafe { Str::text(title) }, Some("panel"));
            assert_eq!(bridge::type_name(p).as_deref(), Some("hud.Panel"));
            // The wrong arity for a signature is refused before Wren sees
            // it; for a getter it is Wren's own missing-method error.
            let kind_of = |err: Value| unsafe {
                caribou::error::Error::kind(caribou::error::Error::from_value(err).unwrap())
            };
            let draw = member(hud, "draw", MethodKind::Method);
            let err = bridge::call(draw, &[h, Value::null()], LANG_CORE).unwrap_err();
            assert_eq!(kind_of(err), caribou_abi::ErrorKind::Type);
            let err = bridge::call(score, &[h, Value::null()], LANG_CORE).unwrap_err();
            assert_eq!(kind_of(err), caribou_abi::ErrorKind::Runtime);
            heap::handle_release(root);
        });

        // Publishing again replaces; a class of another module is not this
        // module's, and an installed one never is.
        assert_eq!(
            vm.interpret("other", "import \"hud\" for Hud\nclass Own {}"),
            InterpretResult::Success
        );
        let other = publish_module(&vm, "other").expect("publishes");
        let names: Vec<&str> = other.classes.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            Vec::<&str>::new(),
            "a class with no methods of its own is not claimed"
        );
        let again = publish_module(&vm, "hud").unwrap();
        assert_eq!(again.classes.len(), 2);
    }

    /// Construct a `Hud` that nothing in Wren refers to, root it by a core
    /// handle, and answer the handle with the object's address inverted:
    /// the caller's frame must hold no word the conservative scan of the
    /// stack would take for the object.
    #[inline(never)]
    fn held_hud(vm: &mut VM, iface: &Interface) -> (heap::Handle, usize) {
        let ctor = iface.class("Hud").unwrap().ctor.as_ref().unwrap();
        with_vm(vm, |_| {
            let h = bridge::call(ctor.target, &[Value::number(1.0)], LANG_CORE).unwrap();
            let p = h.as_object().unwrap() as *mut u8;
            (heap::handle_new(p), !(p as usize))
        })
    }

    /// A core handle is the one reference another language has to a Wren
    /// object; wren_lift's cycle keeps what a handle roots, and reclaims
    /// it once the handle is released.
    #[test]
    fn a_core_handle_roots_a_wren_object_through_wren_lifts_cycle() {
        if parent_of(
            "publish::tests::a_core_handle_roots_a_wren_object_through_wren_lifts_cycle",
            &[],
        ) {
            return;
        }
        crate::install().expect("a fresh process takes the table");
        let mut world = World::new(Config::default());
        world
            .register(Box::new(crate::Runtime::new()))
            .expect("wren registers");
        let mut vm = immix_vm(ExecutionMode::Interpreter);
        assert_eq!(vm.interpret("hud", HUD), InterpretResult::Success);
        let iface = publish_module(&vm, "hud").unwrap();
        let rec = record_for(vm.object_class as *mut u8);

        let (handle, hidden) = held_hud(&mut vm, &iface);
        vm.collect_garbage();
        assert!(
            crate::heap::owns_start(rec, !hidden),
            "the handle kept the object through a cycle"
        );
        heap::handle_release(handle);
        vm.collect_garbage();
        assert!(
            !crate::heap::owns_start(rec, !hidden),
            "released, the object went with the next cycle"
        );
    }

    #[test]
    fn signatures_parse_by_shape_and_operators_do_not() {
        assert_eq!(parse("draw()"), Some(("draw", 0, Shape::Method)));
        assert_eq!(parse("hit(_)"), Some(("hit", 1, Shape::Method)));
        assert_eq!(parse("at(_,_,_)"), Some(("at", 3, Shape::Method)));
        assert_eq!(parse("score"), Some(("score", 0, Shape::Getter)));
        assert_eq!(parse("score=(_)"), Some(("score", 1, Shape::Setter)));
        assert_eq!(parse("_hidden"), Some(("_hidden", 0, Shape::Getter)));
        for op in [
            "+(_)", "==(_)", "-", "!", "[_]", "[_]=(_)", "[_,_]", "..(_)", "is(_)x",
        ] {
            assert_eq!(parse(op), None, "{op}");
        }
        assert_eq!(parse("is(_)"), Some(("is", 1, Shape::Method)));
    }
}
