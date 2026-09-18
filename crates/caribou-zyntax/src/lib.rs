//! Hosts Zyntax on the caribou core. The unit of work is a module as
//! every Zyntax frontend produces it through `zyntax_embed` (a `.zyn`
//! grammar, the Python frontend, ZynML): its typed AST, which names and
//! types what it declares, and its HIR, which the embed runtime compiles
//! and which says how each function is called. The module is published
//! to the world from both (`publish`), so a language reaching it calls
//! machine code by a signature, as it calls a plugin.
//!
//! A frontend is a language of the world (`Language`): it prepares the
//! embed runtime with what its programs link against and parses a
//! module's source into the typed AST, however it parses. A snapshot's
//! grammar (ZynML's), a bare `.zyn` grammar, or a parser of the
//! frontend's own (the Python frontend's) are all the same to the
//! adapter, which only ever sees typed ASTs and HIR. Its file extensions
//! name its modules under the roots, as Wren's `.wren` files name Wren's,
//! so `import "game:scorer" for Scorer` finds `game/scorer.zynml` through
//! the namespace both languages share. The loader parses, lowers to HIR
//! in the runtime's context (`TieredRuntime::lower_to_hir`), publishes
//! what the module declares, then compiles.
//! A frontend's plugins are Zyntax's own, `zrtl`, opened from a
//! directory.
//!
//! One embed runtime per language, on the thread that registered it.
//! Memory is Zyntax's own pool with no collector: the drop analysis
//! releases what it proves dead, the rest stays. The core-heap strategy
//! is the issue's next item.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use caribou::registry;
use caribou::world::{self, Adapter};
use caribou_abi::LangId;
use zyntax_embed::{
    LanguageGrammar, SNAPSHOT_EXTENSION, Snapshot, TieredConfig, TieredRuntime, TypedProgram,
};

mod dispatch;
pub mod publish;

pub use zyntax_embed;

/// A Zyntax frontend as the adapter registers it: a language of the
/// world. It gives the runtime what its programs link against, and it
/// parses a module's source into the typed AST, however it parses: a
/// grammar, a snapshot's grammar, or a parser of its own, as the Python
/// frontend's is.
pub trait Language {
    /// The language's name, in lower case: what a namespace lists.
    fn name(&self) -> &str;

    /// The file extensions of its modules under the roots, with the dot.
    fn extensions(&self) -> &[String];

    /// Give `runtime` what the language's programs link against: a
    /// snapshot, plugins, entry points. Once, before any module loads.
    fn prepare(&mut self, runtime: &mut TieredRuntime) -> Result<(), String>;

    /// The typed AST of a module's `source`, `file` naming it, for
    /// `runtime`: what a program links against types what it parses to.
    fn parse(&self, runtime: &TieredRuntime, source: &str, file: &str) -> Result<TypedProgram, String>;
}

/// A language from a `.zyn` grammar alone.
pub struct GrammarLanguage {
    name: String,
    grammar: LanguageGrammar,
    extensions: Vec<String>,
}

impl GrammarLanguage {
    pub fn new(grammar: LanguageGrammar) -> GrammarLanguage {
        GrammarLanguage {
            name: grammar.name().to_lowercase(),
            extensions: grammar.file_extensions().to_vec(),
            grammar,
        }
    }
}

impl Language for GrammarLanguage {
    fn name(&self) -> &str {
        &self.name
    }

    fn extensions(&self) -> &[String] {
        &self.extensions
    }

    fn prepare(&mut self, runtime: &mut TieredRuntime) -> Result<(), String> {
        runtime.register_grammar(&self.name, self.grammar.clone());
        Ok(())
    }

    fn parse(&self, runtime: &TieredRuntime, source: &str, file: &str) -> Result<TypedProgram, String> {
        self.grammar
            .parse_with_signatures(source, file, runtime.plugin_signatures())
            .map_err(|e| e.to_string())
    }
}

/// A language from its snapshot: the grammar and the library modules it
/// was built with, as the ZynML frontend ships them.
pub struct SnapshotLanguage {
    name: String,
    snapshot: Arc<Snapshot>,
    grammar: LanguageGrammar,
    extensions: Vec<String>,
}

impl SnapshotLanguage {
    pub fn new(bytes: &[u8]) -> Result<SnapshotLanguage, String> {
        let snapshot = Snapshot::load(bytes).map_err(|e| e.to_string())?;
        let grammar = snapshot
            .grammar_bytes()
            .ok_or("the snapshot carries no grammar; the language parses on its own")?;
        let grammar = LanguageGrammar::from_compiled_bytes(grammar).map_err(|e| e.to_string())?;
        Ok(SnapshotLanguage {
            name: snapshot.language().to_lowercase(),
            snapshot: Arc::new(snapshot),
            extensions: grammar.file_extensions().to_vec(),
            grammar,
        })
    }
}

impl Language for SnapshotLanguage {
    fn name(&self) -> &str {
        &self.name
    }

    fn extensions(&self) -> &[String] {
        &self.extensions
    }

    /// The snapshot's grammar is the one the runtime parses with too,
    /// registered under the language by `install_snapshot`.
    fn prepare(&mut self, runtime: &mut TieredRuntime) -> Result<(), String> {
        runtime
            .install_snapshot(Arc::clone(&self.snapshot))
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn parse(&self, runtime: &TieredRuntime, source: &str, file: &str) -> Result<TypedProgram, String> {
        self.grammar
            .parse_with_signatures(source, file, runtime.plugin_signatures())
            .map_err(|e| e.to_string())
    }
}

/// A language the adapter registers, with what the driver found for it.
pub struct Frontend {
    language: Box<dyn Language>,
    plugin_dir: Option<PathBuf>,
}

impl Frontend {
    pub fn new(language: Box<dyn Language>) -> Frontend {
        Frontend {
            language,
            plugin_dir: None,
        }
    }

    /// A language from its snapshot.
    pub fn snapshot(bytes: &[u8]) -> Result<Frontend, String> {
        Ok(Frontend::new(Box::new(SnapshotLanguage::new(bytes)?)))
    }

    /// A language from its grammar alone.
    pub fn grammar(grammar: LanguageGrammar) -> Frontend {
        Frontend::new(Box::new(GrammarLanguage::new(grammar)))
    }

    /// The frontend in `path`: a `.zsnap` snapshot or a `.zyn` grammar.
    pub fn file(path: &std::path::Path) -> Result<Frontend, String> {
        let at = |e: String| format!("{}: {e}", path.display());
        if path.extension().is_some_and(|e| e == SNAPSHOT_EXTENSION) {
            let bytes = std::fs::read(path).map_err(|e| at(e.to_string()))?;
            return Frontend::snapshot(&bytes).map_err(at);
        }
        let grammar = LanguageGrammar::compile_zyn_file(path).map_err(|e| at(e.to_string()))?;
        Ok(Frontend::grammar(grammar))
    }

    /// Whether `path` names a frontend file.
    pub fn is_frontend_file(path: &std::path::Path) -> bool {
        path.extension()
            .is_some_and(|e| e == SNAPSHOT_EXTENSION || e == "zyn")
    }

    /// The frontend files directly under each of `roots`, in name order.
    pub fn files_in(roots: &[impl AsRef<std::path::Path>]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            let mut found: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_file() && Frontend::is_frontend_file(p))
                .collect();
            found.sort();
            out.extend(found);
        }
        out
    }

    /// A directory of `.zrtl` plugins to open for this language.
    pub fn with_plugin_dir(mut self, dir: PathBuf) -> Frontend {
        self.plugin_dir = Some(dir);
        self
    }

    pub fn name(&self) -> &str {
        self.language.name()
    }

    /// The runtime for this language, its plugins opened and the
    /// language prepared on it.
    fn bring_up(&mut self) -> Result<TieredRuntime, String> {
        let mut runtime = TieredRuntime::new(TieredConfig::default()).map_err(|e| e.to_string())?;
        if let Some(dir) = &self.plugin_dir
            && dir.is_dir()
        {
            runtime
                .load_plugins_from_directory(dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        self.language.prepare(&mut runtime)?;
        Ok(runtime)
    }
}

/// A language's runtime and frontend, on the thread that registered it.
struct State {
    language: Box<dyn Language>,
    runtime: TieredRuntime,
}

thread_local! {
    static STATES: RefCell<HashMap<LangId, State>> = RefCell::new(HashMap::new());
}

/// The adapter: each frontend a language of the world.
pub struct Runtime {
    frontends: Vec<Frontend>,
}

impl Runtime {
    pub fn new(frontends: Vec<Frontend>) -> Runtime {
        Runtime { frontends }
    }
}

impl Adapter for Runtime {
    fn languages(&self) -> Vec<String> {
        self.frontends.iter().map(|f| f.name().to_owned()).collect()
    }

    fn assign_languages(&mut self, ids: &[LangId]) {
        for (mut frontend, &lang) in std::mem::take(&mut self.frontends).into_iter().zip(ids) {
            let runtime = match frontend.bring_up() {
                Ok(runtime) => runtime,
                Err(e) => {
                    eprintln!("caribou: zyntax {}: {e}", frontend.name());
                    continue;
                }
            };
            STATES.with(|s| {
                s.borrow_mut().insert(
                    lang,
                    State {
                        language: frontend.language,
                        runtime,
                    },
                )
            });
            caribou::bridge::set_typed_dispatch(lang, dispatch::dispatch);
            registry::set_loader(lang, Arc::new(move |ns, module| load(lang, ns, module)));
        }
    }
}

/// The file for module `name` (`game/scorer`) of the language whose
/// grammar reads `extensions`, under the first root that has one.
fn find(name: &str, extensions: &[String]) -> Option<PathBuf> {
    world::source_roots().into_iter().find_map(|root| {
        extensions
            .iter()
            .map(|ext| root.join(format!("{name}{ext}")))
            .find(|path| path.is_file())
    })
}

/// The registry's loader for a grammar language: parse, lower, publish
/// from the HIR, compile.
fn load(lang: LangId, namespace: &str, module: &str) -> Result<bool, String> {
    let name = format!("{namespace}/{module}");
    STATES.with(|states| {
        let mut states = states.borrow_mut();
        let Some(state) = states.get_mut(&lang) else {
            return Err(format!(
                "`{name}` cannot load: its Zyntax runtime is not on this thread"
            ));
        };
        let Some(path) = find(&name, state.language.extensions()) else {
            return Ok(false);
        };
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("`{name}`: cannot read {}: {e}", path.display()))?;
        let program = state
            .language
            .parse(&state.runtime, &source, &path.to_string_lossy())
            .map_err(|e| format!("`{name}`: {e}"))?;
        // Lowered from a copy: the declarations type the interface, the
        // HIR is what runs.
        let hir = state
            .runtime
            .lower_to_hir(program.clone())
            .map_err(|e| format!("`{name}`: {e}"))?;
        let declared = publish::declared(&program, &hir, state.language.name());
        state
            .runtime
            .compile_module(hir)
            .map_err(|e| format!("`{name}`: {e}"))?;
        let iface = publish::interface(
            lang,
            state.language.name(),
            &name,
            module,
            declared,
            &|symbol| state.runtime.function_pointer(symbol),
        );
        registry::publish(iface).map_err(|e| format!("`{name}`: {e}"))?;
        Ok(true)
    })
}

/// The modules of the frontends under `root` as data, for a build step:
/// every file under `root` with one of their extensions, each loaded into
/// a world of this thread's as running it would and described from what
/// it published, with its path. A module directly under the root has no
/// namespace and is not a module of the world.
pub fn describe(root: &std::path::Path) -> Result<Vec<caribou::describe::ModuleDesc>, String> {
    let frontends = Frontend::files_in(&[root])
        .iter()
        .map(|file| Frontend::file(file))
        .collect::<Result<Vec<_>, _>>()?;
    if frontends.is_empty() {
        return Ok(Vec::new());
    }
    let names: Vec<String> = frontends.iter().map(|f| f.name().to_owned()).collect();
    let extensions: Vec<String> = frontends
        .iter()
        .flat_map(|f| f.language.extensions().iter().cloned())
        .collect();
    // Every module by namespace and name, from the files.
    let mut modules: Vec<(String, String, PathBuf)> = Vec::new();
    let mut namespaces: Vec<String> = Vec::new();
    for entry in walk(root) {
        let Ok(rel) = entry.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let Some(ext) = extensions.iter().find(|e| rel.ends_with(e.as_str())) else {
            continue;
        };
        let stem = &rel[..rel.len() - ext.len()];
        let Some((namespace, module)) = stem.split_once('/') else {
            continue;
        };
        if !namespaces.contains(&namespace.to_owned()) {
            namespaces.push(namespace.to_owned());
        }
        modules.push((namespace.to_owned(), module.to_owned(), entry.clone()));
    }
    let world = world::World::new(world::Config {
        namespaces: namespaces
            .iter()
            .map(|name| caribou::registry::Namespace {
                name: name.clone(),
                langs: names.clone(),
                modules: None,
            })
            .collect(),
        roots: vec![root.to_owned()],
        ..world::Config::default()
    });
    let ids = world
        .register(Box::new(Runtime::new(frontends)))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (namespace, module, path) in modules {
        let Some(iface) = registry::lookup_or_load(&namespace, &module)? else {
            continue;
        };
        let lang = ids
            .iter()
            .find(|&&id| id == iface.lang)
            .map(|&id| world::language_name(id))
            .unwrap_or_default();
        let mut desc = caribou::describe::ModuleDesc::of(&iface, &lang);
        desc.path = Some(path.to_string_lossy().into_owned());
        out.push(desc);
    }
    Ok(out)
}

/// Every file under `dir`, depth first, hidden entries left out.
fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut entries: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
