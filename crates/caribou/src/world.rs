//! The driver's handle: adapter registry, language table and namespace
//! table. Module loading, call and events arrive with the reload pipeline.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex};

use caribou_abi::LangId;

use crate::heap;
use crate::registry;
use crate::sched;

pub use crate::registry::Namespace;

/// A runtime taught the core. Implemented by each resident adapter.
pub trait Adapter: 'static {
    /// The languages this adapter serves, in the order it wants ids.
    fn languages(&self) -> Vec<String>;
    /// Called once with the ids the world assigned, in the same order.
    fn assign_languages(&mut self, ids: &[LangId]);
}

/// One entry in the language table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Language {
    pub id: LangId,
    pub name: String,
    pub adapter: usize,
}

/// What `World::new` takes.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Kept for the reactor; nothing reads it yet.
    pub name: String,
    /// The namespaces imports are addressed through, beside the one every
    /// registered language gets under its own name. See `caribou::registry`.
    pub namespaces: Vec<Namespace>,
}

/// The driver's handle. One per OS thread; `new` initialises that thread's
/// scheduler and the process heap on first use.
pub struct World {
    adapters: Vec<Box<dyn Adapter>>,
    languages: Vec<Language>,
    by_name: HashMap<String, LangId>,
}

static NEXT_LANG: AtomicU32 = AtomicU32::new(1);

/// `LangId` 0 is reserved: the core's own types.
pub const LANG_CORE: LangId = 0;

/// Ids are process-wide, so their names are too: what an error trace or a
/// diagnostic prints for a language, whichever world registered it.
static LANG_NAMES: LazyLock<Mutex<HashMap<LangId, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The reverse: a name to the id most recently registered under it, for
/// resolving a namespace's language names.
static LANG_IDS: LazyLock<Mutex<HashMap<String, LangId>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The registered name of `lang`; `core` for the core's own id, `lang N`
/// for an id no world has registered.
pub fn language_name(lang: LangId) -> String {
    if lang == LANG_CORE {
        return "core".to_owned();
    }
    LANG_NAMES
        .lock()
        .unwrap()
        .get(&lang)
        .cloned()
        .unwrap_or_else(|| format!("lang {lang}"))
}

/// The id most recently registered under `name`, in any world.
pub fn language_id(name: &str) -> Option<LangId> {
    LANG_IDS.lock().unwrap().get(name).copied()
}

impl World {
    /// Also publishes `config.namespaces` process-wide, replacing the table
    /// an earlier world published.
    pub fn new(config: Config) -> World {
        heap::init();
        // Materialise this thread's scheduler and install the heap's poll hook.
        let _ = sched::world_id();
        registry::set_namespaces(config.namespaces);
        World {
            adapters: Vec::new(),
            languages: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    /// Register an adapter and assign its languages. A name already taken by
    /// another adapter is an error: a symbol means what it means inside the
    /// language asking, and two adapters cannot both answer for one.
    pub fn register(
        &mut self,
        mut adapter: Box<dyn Adapter>,
    ) -> Result<Vec<LangId>, RegisterError> {
        let names = adapter.languages();
        if names.is_empty() {
            return Err(RegisterError::NoLanguages);
        }
        for name in &names {
            if self.by_name.contains_key(name) {
                return Err(RegisterError::NameTaken(name.clone()));
            }
        }
        let index = self.adapters.len();
        let ids: Vec<LangId> = names
            .iter()
            .map(|_| NEXT_LANG.fetch_add(1, Ordering::Relaxed))
            .collect();
        for (name, &id) in names.iter().zip(&ids) {
            self.by_name.insert(name.clone(), id);
            LANG_NAMES.lock().unwrap().insert(id, name.clone());
            LANG_IDS.lock().unwrap().insert(name.clone(), id);
            self.languages.push(Language {
                id,
                name: name.clone(),
                adapter: index,
            });
        }
        adapter.assign_languages(&ids);
        self.adapters.push(adapter);
        Ok(ids)
    }

    pub fn language(&self, name: &str) -> Option<LangId> {
        self.by_name.get(name).copied()
    }

    pub fn languages(&self) -> &[Language] {
        &self.languages
    }

    pub fn adapter_for(&self, lang: LangId) -> Option<&dyn Adapter> {
        let entry = self.languages.iter().find(|l| l.id == lang)?;
        self.adapters.get(entry.adapter).map(|a| a.as_ref())
    }

    /// Run scheduler turns until nothing is ready or the deadline passes.
    /// Returns whether live tasks remain.
    pub fn tick(&mut self, deadline: Option<std::time::Instant>) -> bool {
        sched::tick(deadline)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegisterError {
    NoLanguages,
    NameTaken(String),
    /// Two languages of one namespace would both answer `namespace:module`.
    ModuleClash {
        namespace: String,
        module: String,
        langs: [String; 2],
    },
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterError::NoLanguages => write!(f, "adapter registers no languages"),
            RegisterError::NameTaken(name) => write!(f, "language `{name}` is already registered"),
            RegisterError::ModuleClash {
                namespace,
                module,
                langs,
            } => write!(
                f,
                "`{namespace}:{module}` names a module of both `{}` and `{}`",
                langs[0], langs[1]
            ),
        }
    }
}

impl std::error::Error for RegisterError {}

/// `World::new` replaces the process-wide namespace table, so tests that
/// make worlds take turns.
#[cfg(test)]
pub(crate) static SERIAL: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        names: Vec<&'static str>,
        got: Vec<LangId>,
    }

    impl Adapter for Fake {
        fn languages(&self) -> Vec<String> {
            self.names.iter().map(|s| s.to_string()).collect()
        }
        fn assign_languages(&mut self, ids: &[LangId]) {
            self.got = ids.to_vec();
        }
    }

    #[test]
    fn languages_get_distinct_ids_and_resolve_by_name() {
        let _serial = SERIAL.lock().unwrap();
        let mut world = World::new(Config::default());
        let ids = world
            .register(Box::new(Fake {
                names: vec!["haxe"],
                got: vec![],
            }))
            .unwrap();
        let more = world
            .register(Box::new(Fake {
                names: vec!["zyn:lua", "zyn:dialogue"],
                got: vec![],
            }))
            .unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(more.len(), 2);
        assert_ne!(ids[0], LANG_CORE);
        assert!(ids[0] != more[0] && more[0] != more[1]);
        assert_eq!(world.language("haxe"), Some(ids[0]));
        assert_eq!(world.language("zyn:lua"), Some(more[0]));
        assert_eq!(world.languages().len(), 3);
        assert!(world.adapter_for(more[1]).is_some());
        assert!(world.adapter_for(9999).is_none());
        assert_eq!(language_name(ids[0]), "haxe");
        assert_eq!(language_name(more[1]), "zyn:dialogue");
        assert_eq!(language_id("zyn:dialogue"), Some(more[1]));
        assert_eq!(language_id("never registered"), None);
        assert_eq!(language_name(LANG_CORE), "core");
        assert_eq!(language_name(9999), "lang 9999");
    }

    #[test]
    fn a_taken_name_is_refused_and_nothing_is_registered() {
        let _serial = SERIAL.lock().unwrap();
        let mut world = World::new(Config::default());
        world
            .register(Box::new(Fake {
                names: vec!["wren"],
                got: vec![],
            }))
            .unwrap();
        let err = world
            .register(Box::new(Fake {
                names: vec!["other", "wren"],
                got: vec![],
            }))
            .unwrap_err();
        assert_eq!(err, RegisterError::NameTaken("wren".into()));
        assert_eq!(world.languages().len(), 1);
        assert_eq!(world.language("other"), None);
        assert_eq!(
            world
                .register(Box::new(Fake {
                    names: vec![],
                    got: vec![]
                }))
                .unwrap_err(),
            RegisterError::NoLanguages
        );
    }

    #[test]
    fn tick_with_no_tasks_returns_promptly() {
        let _serial = SERIAL.lock().unwrap();
        let mut world = World::new(Config::default());
        assert!(!world.tick(Some(std::time::Instant::now())));
    }
}
