//! The driver's handle: adapter registry, language table and namespace
//! table, the reload of a module with the event it raises, and the watch
//! on the sources that triggers one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, SystemTime};

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
    /// Load the module `module` of `lang` afresh in place: its classes
    /// keep their identity and get the new bodies, its interface is
    /// published again. On the world's thread, with whatever the adapter
    /// needs entered. `Err` when the language does not reload, or the
    /// reload failed.
    fn reload(&self, lang: LangId, module: &str) -> Result<(), String> {
        let _ = module;
        Err(format!("{} does not reload", language_name(lang)))
    }
    /// Take a module of `lang` from a bundle, or the source of one:
    /// `section.format` says what a module's bytes are, in the
    /// language's own terms, and the language refuses a format it does
    /// not read. Installed means the module loads from these bytes when
    /// the program first uses it, as it would from a file under a root,
    /// and its source, when the bundle carries it, is what its errors
    /// render from. `Err` when the language does not install, or the
    /// section is not its to read.
    fn install(&self, lang: LangId, section: &crate::bundle::Section) -> Result<(), String> {
        let _ = section;
        Err(format!("{} does not install modules", language_name(lang)))
    }
}

/// What a world tells its subscribers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A module was loaded afresh: its classes are the ones they were,
    /// with new bodies, and every call site fills again. With `error`,
    /// the load failed and the module is as it was.
    Reload {
        lang: LangId,
        module: String,
        error: Option<String>,
    },
}

/// The kind of event a subscriber asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Reload,
}

impl Event {
    pub fn kind(&self) -> EventKind {
        match self {
            Event::Reload { .. } => EventKind::Reload,
        }
    }
}

type Handler = Box<dyn FnMut(&Event)>;

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
    /// Names the world's threads: its source watch's.
    pub name: String,
    /// The namespaces imports are addressed through, beside the one every
    /// registered language gets under its own name. See `caribou::registry`.
    pub namespaces: Vec<Namespace>,
    /// Where a language that loads from source looks for a module: a
    /// module `game:hud` of such a language is `<root>/game/hud.<ext>`
    /// under the first root that has it. A project's class paths.
    pub roots: Vec<PathBuf>,
}

/// The driver's handle. One per OS thread; `new` initialises that thread's
/// scheduler and the process heap on first use. A clone is the same
/// world: what the reactor's handlers hold.
#[derive(Clone)]
pub struct World {
    inner: Rc<RefCell<Inner>>,
}

struct Inner {
    adapters: Vec<Rc<dyn Adapter>>,
    languages: Vec<Language>,
    by_name: HashMap<String, LangId>,
    handlers: Vec<(EventKind, Handler)>,
    /// Raised and not yet delivered: handlers run from `tick` and from
    /// the end of a reload, never from inside a collection or a switch.
    pending: Vec<Event>,
    /// The source watch, while one runs: its source's signal, and the
    /// flag that ends its thread.
    watch: Option<(sched::Signal, Arc<AtomicBool>)>,
    name: String,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some((signal, stop)) = &self.watch {
            stop.store(true, Ordering::Relaxed);
            sched::remove_source(signal);
        }
    }
}

static NEXT_LANG: AtomicU32 = AtomicU32::new(1);

/// `LangId` 0 is reserved: the core's own types.
pub const LANG_CORE: LangId = 0;

static ROOTS: RwLock<Vec<PathBuf>> = RwLock::new(Vec::new());

/// The source roots of the world most recently created: `Config::roots`.
pub fn source_roots() -> Vec<PathBuf> {
    ROOTS.read().unwrap().clone()
}

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
        *ROOTS.write().unwrap() = config.roots;
        World {
            inner: Rc::new(RefCell::new(Inner {
                adapters: Vec::new(),
                languages: Vec::new(),
                by_name: HashMap::new(),
                handlers: Vec::new(),
                pending: Vec::new(),
                watch: None,
                name: config.name,
            })),
        }
    }

    /// Register an adapter and assign its languages. A name already taken by
    /// another adapter is an error: a symbol means what it means inside the
    /// language asking, and two adapters cannot both answer for one.
    pub fn register(&self, mut adapter: Box<dyn Adapter>) -> Result<Vec<LangId>, RegisterError> {
        let names = adapter.languages();
        if names.is_empty() {
            return Err(RegisterError::NoLanguages);
        }
        let mut inner = self.inner.borrow_mut();
        for name in &names {
            if inner.by_name.contains_key(name) {
                return Err(RegisterError::NameTaken(name.clone()));
            }
        }
        let index = inner.adapters.len();
        let ids: Vec<LangId> = names
            .iter()
            .map(|_| NEXT_LANG.fetch_add(1, Ordering::Relaxed))
            .collect();
        for (name, &id) in names.iter().zip(&ids) {
            inner.by_name.insert(name.clone(), id);
            LANG_NAMES.lock().unwrap().insert(id, name.clone());
            LANG_IDS.lock().unwrap().insert(name.clone(), id);
            inner.languages.push(Language {
                id,
                name: name.clone(),
                adapter: index,
            });
        }
        adapter.assign_languages(&ids);
        inner.adapters.push(Rc::from(adapter));
        Ok(ids)
    }

    pub fn language(&self, name: &str) -> Option<LangId> {
        self.inner.borrow().by_name.get(name).copied()
    }

    pub fn languages(&self) -> Vec<Language> {
        self.inner.borrow().languages.clone()
    }

    pub fn adapter_for(&self, lang: LangId) -> Option<Rc<dyn Adapter>> {
        let inner = self.inner.borrow();
        let entry = inner.languages.iter().find(|l| l.id == lang)?;
        inner.adapters.get(entry.adapter).cloned()
    }

    /// Run scheduler turns until nothing is ready or the deadline passes,
    /// then deliver the events raised meanwhile. Returns whether live
    /// tasks remain.
    pub fn tick(&self, deadline: Option<std::time::Instant>) -> bool {
        let live = sched::tick(deadline);
        self.deliver();
        live
    }

    /// Subscribe `handler` to events of `kind`. Handlers run on the
    /// world's thread, from `tick` and at the end of a reload.
    pub fn on(&self, kind: EventKind, handler: impl FnMut(&Event) + 'static) {
        self.inner
            .borrow_mut()
            .handlers
            .push((kind, Box::new(handler)));
    }

    /// Raise `event`, for the next delivery.
    pub fn raise(&self, event: Event) {
        self.inner.borrow_mut().pending.push(event);
    }

    /// Run the handlers for what is pending, with the world unborrowed,
    /// so a handler may subscribe, raise or reload.
    fn deliver(&self) {
        loop {
            let (events, mut handlers) = {
                let mut inner = self.inner.borrow_mut();
                if inner.pending.is_empty() {
                    return;
                }
                (
                    std::mem::take(&mut inner.pending),
                    std::mem::take(&mut inner.handlers),
                )
            };
            for event in &events {
                for (kind, handler) in &mut handlers {
                    if *kind == event.kind() {
                        handler(event);
                    }
                }
            }
            let mut inner = self.inner.borrow_mut();
            handlers.append(&mut inner.handlers);
            inner.handlers = handlers;
        }
    }

    /// Load the module `namespace:module` afresh, whichever language it
    /// is: the adapter re-runs it in place with its classes' identity
    /// kept, its interface is published again, every call site in every
    /// language fills again (the protocol's epoch), and subscribers hear
    /// `Event::Reload`. Objects of the module made before keep their
    /// classes and so their new bodies. A module nothing has loaded is
    /// an error, and so is a load that fails, after which the module is
    /// as it was.
    pub fn reload(&self, namespace: &str, module: &str) -> Result<(), String> {
        let (lang, module) = registry::resolve(namespace, module)
            .ok_or_else(|| format!("{namespace}:{module} is not loaded"))?;
        self.reload_module(lang, &module)
    }

    /// `reload` for a module already resolved to its language.
    pub fn reload_module(&self, lang: LangId, module: &str) -> Result<(), String> {
        let adapter = self
            .adapter_for(lang)
            .ok_or_else(|| format!("no adapter serves {}", language_name(lang)))?;
        // Between turns on this world; the worker worlds park between
        // theirs while the module's code and tables change.
        let result = sched::quiesce(|| {
            let result = adapter.reload(lang, module);
            if result.is_ok() {
                crate::protocol::bump_epoch();
            }
            result
        });
        self.raise(Event::Reload {
            lang,
            module: module.to_owned(),
            error: result.clone().err(),
        });
        self.deliver();
        result
    }

    /// Install every module a bundle carries but its entry, and every
    /// source, each through the adapter of its language; the entry is
    /// the driver's to load. The bundle's namespaces are the world's own
    /// (`Config`).
    pub fn install(&self, bundle: &crate::bundle::Bundle) -> Result<(), String> {
        use crate::bundle::SectionKind;
        let entry = &bundle.manifest.entry;
        for section in &bundle.sections {
            let installed = matches!(section.kind, SectionKind::Module | SectionKind::Source);
            let is_entry = section.kind == SectionKind::Module
                && section.lang == entry.lang
                && section.name == entry.module;
            if !installed || is_entry {
                continue;
            }
            let lang = self
                .language(&section.lang)
                .ok_or_else(|| format!("no language `{}` is registered", section.lang))?;
            let adapter = self
                .adapter_for(lang)
                .ok_or_else(|| format!("no adapter serves {}", section.lang))?;
            adapter
                .install(lang, section)
                .map_err(|e| format!("{}:{}: {e}", section.lang, section.name))?;
        }
        Ok(())
    }

    /// Watch the files the loaded modules came from, and reload a module
    /// when its file changes: the reload runs on this world's thread
    /// between scheduler turns, wherever the program is idle, and
    /// subscribers hear of it as of any reload. A module loaded later is
    /// watched from then on. The watch ends with the world.
    pub fn watch_sources(&self) {
        if self.inner.borrow().watch.is_some() {
            return;
        }
        let changed: Arc<Mutex<Vec<(LangId, String)>>> = Arc::new(Mutex::new(Vec::new()));
        // Weak, so the source does not keep the world: the driver's handle
        // does, and the source goes with it.
        let world = Rc::downgrade(&self.inner);
        let queue = Arc::clone(&changed);
        let signal = sched::add_source(move || {
            let Some(inner) = Weak::upgrade(&world) else {
                return;
            };
            let world = World { inner };
            let modules = std::mem::take(&mut *queue.lock().unwrap());
            for (lang, module) in modules {
                // Reported through the event; the watch has no caller.
                let _ = world.reload_module(lang, &module);
            }
        });
        let stop = Arc::new(AtomicBool::new(false));
        let name = self.inner.borrow().name.clone();
        let name = if name.is_empty() { "world" } else { &name };
        std::thread::Builder::new()
            .name(format!("{name}-watch"))
            .spawn({
                let signal = signal.clone();
                let stop = Arc::clone(&stop);
                move || watch_sources(&signal, &changed, &stop)
            })
            .expect("a watch thread");
        self.inner.borrow_mut().watch = Some((signal, stop));
    }
}

/// How often the sources are looked at.
const WATCH_INTERVAL: Duration = Duration::from_millis(100);

/// What a file was last seen as: its modification time and length.
type Seen = Option<(SystemTime, u64)>;

fn seen(path: &std::path::Path) -> Seen {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// The watch thread: look at every loaded module's file on an interval,
/// queue the modules whose files changed and raise the signal, until the
/// world is gone.
fn watch_sources(
    signal: &sched::Signal,
    changed: &Mutex<Vec<(LangId, String)>>,
    stop: &AtomicBool,
) {
    let mut last: HashMap<PathBuf, Seen> = HashMap::new();
    loop {
        std::thread::sleep(WATCH_INTERVAL);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let mut any = false;
        for (lang, module, path) in registry::sources() {
            let now = seen(&path);
            match last.get(&path) {
                // Seen for the first time as it is: nothing to reload.
                None => {
                    last.insert(path, now);
                }
                Some(before) if *before != now => {
                    last.insert(path, now);
                    changed.lock().unwrap().push((lang, module));
                    any = true;
                }
                Some(_) => {}
            }
        }
        if any && !signal.raise() {
            return;
        }
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
        let world = World::new(Config::default());
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
        let world = World::new(Config::default());
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
        let world = World::new(Config::default());
        assert!(!world.tick(Some(std::time::Instant::now())));
    }
}
