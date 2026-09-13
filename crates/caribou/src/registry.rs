//! The module registry: the interfaces of loaded modules, published by the
//! adapter that loaded them and read by the adapter answering another
//! language's import.
//!
//! An interface describes one module of one language: its classes, each
//! with fields, methods and a constructor, every member typed and every
//! method carrying the `Callable` the bridge invokes for it. The table is
//! process-wide and keyed by `(lang, module)`; publishing the same key again
//! replaces the earlier interface.
//!
//! Imports address a module through a namespace, not a language name:
//! `import "game:Player"` names the namespace `game` and the module `Player`
//! in it. The namespace table is the world's configuration, published
//! process-wide by `World::new` so an adapter callback with no world handle
//! can resolve it. A namespace covers one or more languages and, when
//! configured with a module list, only those modules; a module of a
//! namespace's language is addressable there by its own name and, when the
//! name begins with the namespace's name and a dot, by the remainder, which
//! is how a Haxe package becomes a namespace. Every registered language is
//! also a namespace under its own name, so `haxe:game.Player` resolves with
//! or without configuration.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use caribou_abi::LangId;

use crate::protocol::Callable;
use crate::world::{self, RegisterError};

/// A type at a module's boundary, in terms every language can map to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeRef {
    Void,
    Bool,
    Int,
    Float,
    Str,
    /// An object of the named type, in the owning language's own terms:
    /// what [`ClassIface::type_name`] holds for the class that defines it.
    Object(String),
    Array(Box<TypeRef>),
    /// Anything; the value's own type decides at the crossing.
    Dyn,
    Fun,
}

#[derive(Clone, Debug)]
pub struct FieldIface {
    pub name: String,
    pub ty: TypeRef,
}

/// A method and how to call it. An instance method's `target` takes the
/// receiver first; a static's takes only its parameters. A constructor's
/// `target` takes the constructor's parameters and returns the new object.
#[derive(Clone, Debug)]
pub struct MethodIface {
    pub name: String,
    pub is_static: bool,
    pub params: Vec<TypeRef>,
    pub ret: TypeRef,
    pub target: Callable,
}

#[derive(Clone, Debug)]
pub struct ClassIface {
    /// The class's simple name: what an importer binds.
    pub name: String,
    /// The type's name in its own language, matched against
    /// [`TypeRef::Object`] and against what an instance reports, so an
    /// object coming back across the bridge finds its class.
    pub type_name: String,
    pub superclass: Option<String>,
    pub fields: Vec<FieldIface>,
    pub methods: Vec<MethodIface>,
    pub ctor: Option<MethodIface>,
}

/// One module's boundary.
#[derive(Clone, Debug)]
pub struct Interface {
    pub lang: LangId,
    pub module: String,
    pub classes: Vec<ClassIface>,
}

// A `Callable` holds code and type pointers of the publishing language's
// loaded program, which outlive the interface, and a dynamic callable's
// object is rooted by its publisher.
unsafe impl Send for Interface {}
unsafe impl Sync for Interface {}

impl Interface {
    pub fn class(&self, name: &str) -> Option<&ClassIface> {
        self.classes.iter().find(|c| c.name == name)
    }
}

/// One configured namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Namespace {
    pub name: String,
    /// Language names, as registered.
    pub langs: Vec<String>,
    /// The modules visible through this namespace, by their own names;
    /// `None` is every module of its languages.
    pub modules: Option<Vec<String>>,
}

struct Table {
    interfaces: HashMap<(LangId, String), Arc<Interface>>,
    /// `(lang, type name)` to `(module, class index)`.
    by_type: HashMap<(LangId, String), (String, usize)>,
}

static TABLE: LazyLock<RwLock<Table>> = LazyLock::new(|| {
    RwLock::new(Table {
        interfaces: HashMap::new(),
        by_type: HashMap::new(),
    })
});

static NAMESPACES: RwLock<Vec<Namespace>> = RwLock::new(Vec::new());

/// Install the world's namespace table, replacing the previous one.
pub(crate) fn set_namespaces(namespaces: Vec<Namespace>) {
    *NAMESPACES.write().unwrap() = namespaces;
}

pub fn namespaces() -> Vec<Namespace> {
    NAMESPACES.read().unwrap().clone()
}

/// The names a module of `lang` answers to inside the namespace `ns`, or
/// none when the namespace hides it.
fn import_names(ns: &Namespace, module: &str) -> Vec<String> {
    if let Some(allowed) = &ns.modules
        && !allowed.iter().any(|m| m == module)
    {
        return Vec::new();
    }
    let mut names = vec![module.to_owned()];
    if let Some(rest) = module
        .strip_prefix(&ns.name)
        .and_then(|r| r.strip_prefix('.'))
        && !rest.is_empty()
    {
        names.push(rest.to_owned());
    }
    names
}

/// Publish `iface`, replacing any interface of the same `(lang, module)`.
/// Refused when a configured namespace holding this module's language and
/// another would address a module of each by one import name.
pub fn publish(iface: Interface) -> Result<(), RegisterError> {
    let lang_name = world::language_name(iface.lang);
    let namespaces = NAMESPACES.read().unwrap();
    let mut table = TABLE.write().unwrap();
    for ns in namespaces.iter().filter(|ns| ns.langs.contains(&lang_name)) {
        let mine = import_names(ns, &iface.module);
        if mine.is_empty() {
            continue;
        }
        for other in ns.langs.iter().filter(|l| **l != lang_name) {
            let Some(other_id) = world::language_id(other) else {
                continue;
            };
            for (key, _) in table.interfaces.iter().filter(|(k, _)| k.0 == other_id) {
                if let Some(name) = import_names(ns, &key.1)
                    .into_iter()
                    .find(|n| mine.contains(n))
                {
                    return Err(RegisterError::ModuleClash {
                        namespace: ns.name.clone(),
                        module: name,
                        langs: [lang_name.clone(), other.clone()],
                    });
                }
            }
        }
    }
    drop(namespaces);
    let key = (iface.lang, iface.module.clone());
    if let Some(old) = table.interfaces.remove(&key) {
        for class in &old.classes {
            table.by_type.remove(&(old.lang, class.type_name.clone()));
        }
    }
    for (i, class) in iface.classes.iter().enumerate() {
        table.by_type.insert(
            (iface.lang, class.type_name.clone()),
            (iface.module.clone(), i),
        );
    }
    table.interfaces.insert(key, Arc::new(iface));
    Ok(())
}

/// The published interface of `module` in `lang`.
pub fn interface(lang: LangId, module: &str) -> Option<Arc<Interface>> {
    TABLE
        .read()
        .unwrap()
        .interfaces
        .get(&(lang, module.to_owned()))
        .cloned()
}

/// Every published interface of `lang`.
pub fn interfaces_of(lang: LangId) -> Vec<Arc<Interface>> {
    TABLE
        .read()
        .unwrap()
        .interfaces
        .iter()
        .filter(|(k, _)| k.0 == lang)
        .map(|(_, v)| v.clone())
        .collect()
}

/// The language and module `namespace:module` names, if published.
pub fn resolve(namespace: &str, module: &str) -> Option<(LangId, String)> {
    let namespaces = NAMESPACES.read().unwrap();
    let table = TABLE.read().unwrap();
    if let Some(ns) = namespaces.iter().find(|ns| ns.name == namespace) {
        for lang in &ns.langs {
            let Some(id) = world::language_id(lang) else {
                continue;
            };
            let prefixed = format!("{namespace}.{module}");
            for candidate in [module, prefixed.as_str()] {
                if ns
                    .modules
                    .as_ref()
                    .is_some_and(|allowed| !allowed.iter().any(|m| m == candidate))
                {
                    continue;
                }
                if table.interfaces.contains_key(&(id, candidate.to_owned())) {
                    return Some((id, candidate.to_owned()));
                }
            }
        }
        return None;
    }
    let id = world::language_id(namespace)?;
    table
        .interfaces
        .contains_key(&(id, module.to_owned()))
        .then(|| (id, module.to_owned()))
}

/// The interface `namespace:module` names.
pub fn lookup(namespace: &str, module: &str) -> Option<Arc<Interface>> {
    let (lang, module) = resolve(namespace, module)?;
    interface(lang, &module)
}

/// One class of `namespace:module`, with its interface.
pub fn lookup_class(namespace: &str, module: &str, class: &str) -> Option<(Arc<Interface>, usize)> {
    let iface = lookup(namespace, module)?;
    let index = iface.classes.iter().position(|c| c.name == class)?;
    Some((iface, index))
}

/// The class whose instances `lang` names `type_name`, with its interface.
pub fn class_for_type(lang: LangId, type_name: &str) -> Option<(Arc<Interface>, usize)> {
    let table = TABLE.read().unwrap();
    let (module, index) = table.by_type.get(&(lang, type_name.to_owned()))?;
    let iface = table.interfaces.get(&(lang, module.clone()))?.clone();
    Some((iface, *index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Adapter, Config, World};
    use caribou_abi::Value;

    struct Fake(&'static str);

    impl Adapter for Fake {
        fn languages(&self) -> Vec<String> {
            vec![self.0.to_owned()]
        }
        fn assign_languages(&mut self, _ids: &[LangId]) {}
    }

    fn iface(lang: LangId, module: &str, class: &str) -> Interface {
        Interface {
            lang,
            module: module.to_owned(),
            classes: vec![ClassIface {
                name: class.to_owned(),
                type_name: module.to_owned(),
                superclass: None,
                fields: vec![],
                methods: vec![MethodIface {
                    name: "f".to_owned(),
                    is_static: false,
                    params: vec![TypeRef::Int],
                    ret: TypeRef::Bool,
                    target: Callable::Dynamic(Value::null()),
                }],
                ctor: None,
            }],
        }
    }

    #[test]
    fn namespaces_resolve_and_a_clash_is_refused() {
        let _serial = world::SERIAL.lock().unwrap();
        let mut world = World::new(Config {
            namespaces: vec![
                Namespace {
                    name: "rgame".to_owned(),
                    langs: vec!["rlang_a".to_owned(), "rlang_b".to_owned()],
                    modules: None,
                },
                Namespace {
                    name: "rsubset".to_owned(),
                    langs: vec!["rlang_a".to_owned()],
                    modules: Some(vec!["rgame.Player".to_owned()]),
                },
            ],
            ..Config::default()
        });
        let a = world.register(Box::new(Fake("rlang_a"))).unwrap()[0];
        let b = world.register(Box::new(Fake("rlang_b"))).unwrap()[0];

        publish(iface(a, "rgame.Player", "Player")).unwrap();
        publish(iface(a, "rgame.Enemy", "Enemy")).unwrap();
        // The configured namespace, by the module's own name and without
        // the package prefix; the language namespace, by its own name.
        assert_eq!(
            resolve("rgame", "Player"),
            Some((a, "rgame.Player".to_owned()))
        );
        assert_eq!(
            resolve("rgame", "rgame.Player"),
            Some((a, "rgame.Player".to_owned()))
        );
        assert_eq!(
            resolve("rlang_a", "rgame.Player"),
            Some((a, "rgame.Player".to_owned()))
        );
        assert_eq!(resolve("rlang_a", "Player"), None);
        assert_eq!(resolve("rgame", "Nope"), None);
        assert_eq!(resolve("nowhere", "Player"), None);
        // The subset hides what it does not list, and prefixes only its
        // own name.
        assert_eq!(
            resolve("rsubset", "rgame.Player"),
            Some((a, "rgame.Player".to_owned()))
        );
        assert_eq!(resolve("rsubset", "rgame.Enemy"), None);
        assert_eq!(resolve("rsubset", "Player"), None);

        let (found, index) = lookup_class("rgame", "Player", "Player").unwrap();
        assert_eq!(found.lang, a);
        assert_eq!(found.classes[index].methods[0].ret, TypeRef::Bool);
        assert!(lookup_class("rgame", "Player", "Other").is_none());
        let (by_type, index) = class_for_type(a, "rgame.Player").unwrap();
        assert_eq!(by_type.module, "rgame.Player");
        assert_eq!(by_type.classes[index].name, "Player");

        // The other language publishing a module addressable as
        // `rgame:Player` too is refused; a distinct name is not.
        let err = publish(iface(b, "Player", "Player")).unwrap_err();
        assert_eq!(
            err,
            RegisterError::ModuleClash {
                namespace: "rgame".to_owned(),
                module: "Player".to_owned(),
                langs: ["rlang_b".to_owned(), "rlang_a".to_owned()],
            }
        );
        publish(iface(b, "Hud", "Hud")).unwrap();
        assert_eq!(resolve("rgame", "Hud"), Some((b, "Hud".to_owned())));

        // Republishing replaces, and the type index follows.
        let mut again = iface(a, "rgame.Player", "Player");
        again.classes[0].type_name = "rgame.Player2".to_owned();
        publish(again).unwrap();
        assert!(class_for_type(a, "rgame.Player").is_none());
        assert!(class_for_type(a, "rgame.Player2").is_some());
        assert_eq!(interfaces_of(a).len(), 2);
    }
}
