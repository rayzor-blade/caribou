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
//! Memory belongs to Zyntax's pool and its collector, separate from the
//! core heap. The adapter does not yet supply the shared ownership and
//! tracing needed for object, buffer, or enum values to cross.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use caribou::registry;
use caribou::world::{self, Adapter};
use caribou_abi::LangId;
use zyntax_embed::{
    ExportedSymbol, LanguageGrammar, ModuleArchitecture, SNAPSHOT_EXTENSION, Snapshot,
    SnapshotBuilder, TieredConfig, TieredRuntime, TypedProgram,
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

    /// How the language lays modules out as files, in Zyntax's terms:
    /// `game.tally` is `game/tally.py` or `game/tally/__init__.py` to
    /// Python, `game/Tally.hx` to a Haxe-like language. One entry per
    /// layout the language reads; the first that has a file wins.
    fn architectures(&self) -> Vec<ModuleArchitecture>;

    /// What a module exports, by the language's own convention. The
    /// default is every function, struct or class the module's own file
    /// declares public, which is what the frontend's typed AST says; a
    /// frontend with a rule of its own (`__all__`) overrides it.
    fn exports(&self, program: &TypedProgram) -> Vec<ExportedSymbol> {
        publish::declared_exports(program)
    }

    /// The methods an exported class exports, by the language's own
    /// convention, or `None` for every method the frontend declared. A
    /// constructor is published either way.
    fn exported_members(&self, _program: &TypedProgram, _class: &str) -> Option<Vec<String>> {
        None
    }

    /// Give `runtime` what the language's programs link against: a
    /// snapshot, plugins, entry points. Once, before any module loads.
    fn prepare(&mut self, runtime: &mut TieredRuntime) -> Result<(), String>;

    /// The typed AST of a module's `source`, `file` naming it, for
    /// `runtime`: what a program links against types what it parses to.
    /// A frontend that reads the modules this one imports itself reads
    /// them from `sources`.
    fn parse(
        &self,
        runtime: &TieredRuntime,
        source: &str,
        file: &str,
        sources: &Sources,
    ) -> Result<TypedProgram, String>;
}

/// Where a language's modules are read from: a bundle's staged sources
/// first, by path under a root, then the world's source roots.
pub struct Sources<'a> {
    staged: &'a Staged,
}

impl Sources<'_> {
    /// The source of the module with these path segments (`["game",
    /// "util"]`) under one of `architectures`' layouts, when there is
    /// one.
    pub fn module(
        &self,
        segments: &[String],
        architectures: &[ModuleArchitecture],
    ) -> Option<String> {
        source_of(segments, architectures, self.staged)
    }
}

/// A bundle's staged sources, by path under a root. Shared with the
/// runtime's import resolver, which outlives any borrow of the state.
type Staged = Mutex<HashMap<String, String>>;

/// The source of the module `segments` name, staged or in a root.
fn source_of(
    segments: &[String],
    architectures: &[ModuleArchitecture],
    staged: &Staged,
) -> Option<String> {
    let staged = staged.lock().unwrap();
    match find(segments, architectures, &staged)? {
        Found::Staged(name) => Some(staged[&name].clone()),
        Found::File(path) => std::fs::read_to_string(path).ok(),
    }
}

/// The resolver a language's runtime asks for a module one of its own
/// modules imports (`import util`, `import game.util`): a dotted path
/// names the module from the root; a bare name is a module of the
/// importing module's namespace, else of any namespace the world has.
/// The runtime's own snapshot modules are asked before it.
fn import_resolver(
    architectures: Vec<ModuleArchitecture>,
    staged: Arc<Staged>,
    importing: Arc<Mutex<Option<String>>>,
) -> zyntax_embed::ImportResolverCallback {
    Box::new(move |path: &str| {
        let candidates: Vec<Vec<String>> = if path.contains('.') {
            vec![path.split('.').map(str::to_owned).collect()]
        } else {
            let own = importing.lock().unwrap().clone();
            own.into_iter()
                .chain(registry::namespaces().into_iter().map(|ns| ns.name))
                .map(|ns| vec![ns, path.to_owned()])
                .collect()
        };
        Ok(candidates
            .iter()
            .find_map(|segments| source_of(segments, &architectures, &staged)))
    })
}

/// The layouts a grammar's `file_extensions` describe: a file per module
/// under the package's directories, one layout per extension.
fn by_extension(extensions: &[String]) -> Vec<ModuleArchitecture> {
    extensions
        .iter()
        .map(|ext| ModuleArchitecture::DotSeparatedPackages {
            extension: ext.trim_start_matches('.').to_owned(),
        })
        .collect()
}

/// A language from a `.zyn` grammar alone.
pub struct GrammarLanguage {
    name: String,
    grammar: LanguageGrammar,
    architectures: Vec<ModuleArchitecture>,
}

impl GrammarLanguage {
    pub fn new(grammar: LanguageGrammar) -> GrammarLanguage {
        GrammarLanguage {
            name: grammar.name().to_lowercase(),
            architectures: by_extension(grammar.file_extensions()),
            grammar,
        }
    }
}

impl Language for GrammarLanguage {
    fn name(&self) -> &str {
        &self.name
    }

    fn architectures(&self) -> Vec<ModuleArchitecture> {
        self.architectures.clone()
    }

    fn prepare(&mut self, runtime: &mut TieredRuntime) -> Result<(), String> {
        runtime.register_grammar(&self.name, self.grammar.clone());
        Ok(())
    }

    fn parse(
        &self,
        runtime: &TieredRuntime,
        source: &str,
        file: &str,
        _sources: &Sources,
    ) -> Result<TypedProgram, String> {
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
    architectures: Vec<ModuleArchitecture>,
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
            architectures: by_extension(grammar.file_extensions()),
            grammar,
        })
    }
}

impl Language for SnapshotLanguage {
    fn name(&self) -> &str {
        &self.name
    }

    fn architectures(&self) -> Vec<ModuleArchitecture> {
        self.architectures.clone()
    }

    /// The snapshot's grammar is the one the runtime parses with too,
    /// registered under the language by `install_snapshot`.
    fn prepare(&mut self, runtime: &mut TieredRuntime) -> Result<(), String> {
        runtime
            .install_snapshot(Arc::clone(&self.snapshot))
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn parse(
        &self,
        runtime: &TieredRuntime,
        source: &str,
        file: &str,
        _sources: &Sources,
    ) -> Result<TypedProgram, String> {
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

    /// The frontend in `path` as snapshot bytes, the form a bundle
    /// carries: a `.zsnap` as it is, a `.zyn` compiled and wrapped in a
    /// snapshot of its grammar alone.
    pub fn snapshot_bytes(path: &std::path::Path) -> Result<Vec<u8>, String> {
        let at = |e: String| format!("{}: {e}", path.display());
        if path.extension().is_some_and(|e| e == SNAPSHOT_EXTENSION) {
            return std::fs::read(path).map_err(|e| at(e.to_string()));
        }
        let grammar = LanguageGrammar::compile_zyn_file(path).map_err(|e| at(e.to_string()))?;
        let compiled = grammar.to_compiled_bytes().map_err(|e| at(e.to_string()))?;
        SnapshotBuilder::new(grammar.name())
            .grammar(compiled)
            .encode()
            .map_err(|e| at(e.to_string()))
    }

    /// The module files of this language under `root`, as `(path under
    /// the root, path)`: every file one of its layouts reads, in walk
    /// order. The name is what a bundle stages the module under and
    /// what `find` asks for.
    pub fn modules_in(&self, root: &std::path::Path) -> Vec<(String, PathBuf)> {
        let architectures = self.language.architectures();
        walk(root)
            .into_iter()
            .filter(|file| {
                architectures
                    .iter()
                    .any(|arch| module_of(arch, root, file).is_some())
            })
            .filter_map(|file| {
                let name = staged_name(file.strip_prefix(root).ok()?);
                Some((name, file))
            })
            .collect()
    }

    /// Whether `path` names a frontend file. A hidden one is not: a
    /// `._name.zsnap` is the sidecar a macOS archive leaves beside the file.
    pub fn is_frontend_file(path: &std::path::Path) -> bool {
        !path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            && path
                .extension()
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

    /// The runtime for this language, its plugins opened, the language
    /// prepared on it and its imports resolved from the world's sources.
    fn bring_up(&mut self) -> Result<State, String> {
        let mut runtime = TieredRuntime::new(TieredConfig::default()).map_err(|e| e.to_string())?;
        if let Some(dir) = &self.plugin_dir
            && dir.is_dir()
        {
            runtime
                .load_plugins_from_directory(dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        self.language.prepare(&mut runtime)?;
        let staged = Arc::new(Mutex::new(HashMap::new()));
        let importing = Arc::new(Mutex::new(None));
        runtime.add_import_resolver(import_resolver(
            self.language.architectures(),
            Arc::clone(&staged),
            Arc::clone(&importing),
        ));
        Ok(State {
            language: std::mem::replace(&mut self.language, Box::new(Unprepared)),
            runtime,
            staged,
            importing,
            current: None,
        })
    }
}

/// What a `Frontend` holds once its language moved into its `State`.
struct Unprepared;

impl Language for Unprepared {
    fn name(&self) -> &str {
        ""
    }
    fn architectures(&self) -> Vec<ModuleArchitecture> {
        Vec::new()
    }
    fn prepare(&mut self, _runtime: &mut TieredRuntime) -> Result<(), String> {
        Err("the frontend's language is already registered".to_owned())
    }
    fn parse(
        &self,
        _runtime: &TieredRuntime,
        _source: &str,
        _file: &str,
        _sources: &Sources,
    ) -> Result<TypedProgram, String> {
        Err("the frontend's language is already registered".to_owned())
    }
}

/// A language's runtime and frontend, on the thread that registered it,
/// and the module sources a bundle staged for it, by path under a root.
struct State {
    language: Box<dyn Language>,
    runtime: TieredRuntime,
    staged: Arc<Staged>,
    /// The namespace of the module being parsed and lowered, for the
    /// import resolver's bare names.
    importing: Arc<Mutex<Option<String>>>,
    /// The module the runtime compiled last: the one its reload diffs
    /// an edit against.
    current: Option<String>,
}

/// The form a bundle carries a module of these languages in.
pub const SOURCE: &str = "source";

/// A path under a root as a staged module's name: `/`-separated.
fn staged_name(rel: &std::path::Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

thread_local! {
    static STATES: RefCell<HashMap<LangId, State>> = RefCell::new(HashMap::new());
}

/// The adapter: each frontend a language of the world.
pub struct Runtime {
    frontends: Vec<Frontend>,
    /// The languages brought up, whose states go with the adapter.
    langs: Vec<LangId>,
}

impl Runtime {
    pub fn new(frontends: Vec<Frontend>) -> Runtime {
        Runtime {
            frontends,
            langs: Vec::new(),
        }
    }
}

/// The states go with the world, on its thread and while the process's
/// other threads are alive: a runtime joins its worker as it drops, and
/// Windows runs a thread-local's destructor only once the process has
/// killed every other thread.
impl Drop for Runtime {
    fn drop(&mut self) {
        STATES.with(|states| {
            let mut states = states.borrow_mut();
            for lang in &self.langs {
                states.remove(lang);
            }
        });
    }
}

impl Adapter for Runtime {
    fn languages(&self) -> Vec<String> {
        self.frontends.iter().map(|f| f.name().to_owned()).collect()
    }

    fn assign_languages(&mut self, ids: &[LangId]) {
        for (mut frontend, &lang) in std::mem::take(&mut self.frontends).into_iter().zip(ids) {
            let state = match frontend.bring_up() {
                Ok(state) => state,
                Err(e) => {
                    eprintln!("caribou: zyntax {}: {e}", frontend.name());
                    continue;
                }
            };
            STATES.with(|s| s.borrow_mut().insert(lang, state));
            self.langs.push(lang);
            caribou::bridge::set_typed_dispatch(lang, dispatch::dispatch);
            registry::set_loader(lang, Arc::new(move |ns, module| load(lang, ns, module)));
        }
    }

    fn reload(&self, lang: LangId, module: &str) -> Result<(), String> {
        reload(lang, module)
    }

    /// A bundle's module of one of these languages: its source, staged
    /// under its path, for the loader to find before any root.
    fn install(&self, lang: LangId, section: &caribou::bundle::Section) -> Result<(), String> {
        use caribou::bundle::SectionKind;
        if section.kind != SectionKind::Module || section.format != SOURCE {
            return Err(format!(
                "a {} module comes as {SOURCE}, not `{}`",
                world::language_name(lang),
                section.format
            ));
        }
        let text = String::from_utf8(section.data.clone())
            .map_err(|e| format!("the source is not UTF-8: {e}"))?;
        STATES.with(|states| {
            let mut states = states.borrow_mut();
            let state = states
                .get_mut(&lang)
                .ok_or("its Zyntax runtime is not on this thread")?;
            state
                .staged
                .lock()
                .unwrap()
                .insert(section.name.clone(), text);
            Ok(())
        })
    }
}

/// Where a module's source was found.
enum Found {
    /// Staged from a bundle, under this name.
    Staged(String),
    File(PathBuf),
}

/// The source of the module with these path segments (`["game",
/// "scorer"]`) of a language with these layouts: staged under one of
/// the paths a layout gives it, else the file under the first root that
/// has one.
fn find(
    segments: &[String],
    architectures: &[ModuleArchitecture],
    staged: &HashMap<String, String>,
) -> Option<Found> {
    if !staged.is_empty() {
        let found = architectures
            .iter()
            .flat_map(|arch| arch.module_to_paths(segments, &PathBuf::new()))
            .map(|path| staged_name(&path))
            .find(|name| staged.contains_key(name));
        if let Some(name) = found {
            return Some(Found::Staged(name));
        }
    }
    world::source_roots().into_iter().find_map(|root| {
        architectures
            .iter()
            .flat_map(|arch| arch.module_to_paths(segments, &root))
            .find(|path| path.is_file())
            .map(Found::File)
    })
}

/// The module a file under `root` is, by a layout: its path segments
/// without the extension, or the directory's for a package's own file
/// (`__init__.py`, `mod.rs`, `index.js`). `None` for a file the layout
/// does not read.
fn module_of(
    arch: &ModuleArchitecture,
    root: &std::path::Path,
    file: &std::path::Path,
) -> Option<Vec<String>> {
    let rel = file.strip_prefix(root).ok()?;
    let mut segments: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let last = segments.pop()?;
    let (own_file, extensions): (Option<&str>, Vec<String>) = match arch {
        ModuleArchitecture::DotSeparatedPackages { extension } => (None, vec![extension.clone()]),
        ModuleArchitecture::RustStyle {
            extension,
            mod_file_name,
        } => (Some(mod_file_name), vec![extension.clone()]),
        ModuleArchitecture::PythonStyle {
            extension,
            init_file_name,
        } => (Some(init_file_name), vec![extension.clone()]),
        ModuleArchitecture::NodeStyle { extensions, .. } => (
            None,
            extensions
                .iter()
                .map(|e| e.trim_start_matches('.').to_owned())
                .collect(),
        ),
        _ => return None,
    };
    if own_file == Some(last.as_str()) {
        return (!segments.is_empty()).then_some(segments);
    }
    let stem = extensions
        .iter()
        .find_map(|ext| last.strip_suffix(&format!(".{ext}")))?;
    segments.push(stem.to_owned());
    Some(segments)
}

/// A module's source, parsed and lowered in its language's runtime,
/// with what its interface publishes.
struct Parsed {
    program: TypedProgram,
    hir: zyntax_embed::HirModule,
    declared: publish::Declared,
    /// The file it came from, when it did.
    file: Option<PathBuf>,
}

impl State {
    /// Find, parse and lower module `name` (`game/scorer`); `None` when
    /// no layout of the language has it. Lowered from a copy of the
    /// program: the declarations type the interface, the HIR is what
    /// runs.
    fn parse(&self, name: &str) -> Result<Option<Parsed>, String> {
        let segments: Vec<String> = name.split('/').map(str::to_owned).collect();
        let staged = self.staged.lock().unwrap();
        let Some(found) = find(&segments, &self.language.architectures(), &staged) else {
            return Ok(None);
        };
        let (source, file, path) = match found {
            Found::Staged(key) => (staged[&key].clone(), key, None),
            Found::File(path) => (
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("`{name}`: cannot read {}: {e}", path.display()))?,
                path.to_string_lossy().into_owned(),
                Some(path),
            ),
        };
        drop(staged);
        let sources = Sources {
            staged: &self.staged,
        };
        // The importing module's namespace, for the resolver, while its
        // imports are read: at the parse for a frontend that reads them
        // itself, at the lowering for one the runtime reads them for.
        *self.importing.lock().unwrap() = segments.first().cloned();
        let lowered = self
            .language
            .parse(&self.runtime, &source, &file, &sources)
            .and_then(|program| {
                self.runtime
                    .lower_to_hir(program.clone())
                    .map(|hir| (program, hir))
                    .map_err(|e| e.to_string())
            });
        *self.importing.lock().unwrap() = None;
        let (program, hir) = lowered.map_err(|e| format!("`{name}`: {e}"))?;
        let exports = self.language.exports(&program);
        let mut declared = publish::declared(&program, &exports, &hir, self.language.name());
        for class in &mut declared.classes {
            if let Some(members) = self.language.exported_members(&program, &class.name) {
                class.methods.retain(|m| members.contains(&m.name));
            }
        }
        Ok(Some(Parsed {
            program,
            hir,
            declared,
            file: path,
        }))
    }

    /// Publish module `name`'s interface from `declared`, with the
    /// runtime's current code behind each symbol.
    fn publish(&self, lang: LangId, name: &str, declared: publish::Declared) -> Result<(), String> {
        let iface = publish::interface(lang, self.language.name(), name, declared, &|symbol| {
            self.runtime.function_pointer(symbol)
        });
        registry::publish(iface).map_err(|e| format!("`{name}`: {e}"))
    }
}

/// The registry's loader for a grammar language: parse, lower, compile,
/// publish from the HIR.
fn load(lang: LangId, namespace: &str, module: &str) -> Result<bool, String> {
    let name = format!("{namespace}/{module}");
    STATES.with(|states| {
        let mut states = states.borrow_mut();
        let Some(state) = states.get_mut(&lang) else {
            return Err(format!(
                "`{name}` cannot load: its Zyntax runtime is not on this thread"
            ));
        };
        let Some(parsed) = state.parse(&name)? else {
            return Ok(false);
        };
        state
            .runtime
            .compile_module(parsed.hir)
            .map_err(|e| format!("`{name}`: {e}"))?;
        state.current = Some(name.clone());
        state.publish(lang, &name, parsed.declared)?;
        // A file the world watches: an edit reloads the module.
        if let Some(path) = parsed.file {
            registry::set_source(lang, &name, path);
        }
        Ok(true)
    })
}

/// Load module `name` again from its source, over the running one: the
/// runtime swaps the functions whose code changed and keeps the rest,
/// and the interface publishes again with the code now behind each
/// symbol. A function that fails to compile keeps its old code and
/// fails the reload. The runtime diffs an edit against the module it
/// compiled last, so only that module reloads; see git-bug
/// 5bcf0d678e3b10197a5554df0fea80caaac84522e7921e6aabca4449011fdc98.
fn reload(lang: LangId, name: &str) -> Result<(), String> {
    STATES.with(|states| {
        let mut states = states.borrow_mut();
        let Some(state) = states.get_mut(&lang) else {
            return Err(format!(
                "`{name}` cannot reload: its Zyntax runtime is not on this thread"
            ));
        };
        match &state.current {
            Some(current) if current == name => {}
            Some(current) => {
                return Err(format!(
                    "`{name}` cannot reload: its runtime compiled `{current}` after it, and reloads only the last module it compiled"
                ));
            }
            None => return Err(format!("`{name}` is not loaded")),
        }
        let Some(parsed) = state.parse(name)? else {
            return Err(format!("`{name}` has no source to reload from"));
        };
        let report = state
            .runtime
            .reload_typed_program(parsed.program)
            .map_err(|e| format!("`{name}`: {e}"))?;
        if let Some((function, error)) = report.failed.first() {
            return Err(format!("`{name}`: {function}: {error}"));
        }
        state.publish(lang, name, parsed.declared)
    })
}

/// The modules of the frontends under `root` as data, for a build step:
/// the frontend files under the root plus `others` (languages that parse
/// on their own, which the caller knows to add), every file under `root`
/// with one of their extensions, each loaded into a world of this
/// thread's as running it would and described from what it published,
/// with its path. A module directly under the root has no namespace and
/// is not a module of the world.
pub fn describe(
    root: &std::path::Path,
    others: Vec<Frontend>,
) -> Result<Vec<caribou::describe::ModuleDesc>, String> {
    let mut frontends = Frontend::files_in(&[root])
        .iter()
        .map(|file| Frontend::file(file))
        .collect::<Result<Vec<_>, _>>()?;
    frontends.extend(others);
    if frontends.is_empty() {
        return Ok(Vec::new());
    }
    let names: Vec<String> = frontends.iter().map(|f| f.name().to_owned()).collect();
    let architectures: Vec<ModuleArchitecture> = frontends
        .iter()
        .flat_map(|f| f.language.architectures())
        .collect();
    // Every module by namespace and name, from the files each layout
    // reads.
    let mut modules: Vec<(String, String, PathBuf)> = Vec::new();
    let mut namespaces: Vec<String> = Vec::new();
    for entry in walk(root) {
        let Some(segments) = architectures
            .iter()
            .find_map(|arch| module_of(arch, root, &entry))
        else {
            continue;
        };
        let [namespace, rest @ ..] = segments.as_slice() else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        if !namespaces.contains(namespace) {
            namespaces.push(namespace.clone());
        }
        modules.push((namespace.clone(), rest.join("/"), entry.clone()));
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
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
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

#[cfg(test)]
mod tests {
    use super::Frontend;
    use std::path::Path;

    #[test]
    fn a_hidden_file_is_not_a_frontend() {
        assert!(Frontend::is_frontend_file(Path::new("src/zynml.zsnap")));
        assert!(Frontend::is_frontend_file(Path::new("src/lang.zyn")));
        assert!(!Frontend::is_frontend_file(Path::new("src/._zynml.zsnap")));
        assert!(!Frontend::is_frontend_file(Path::new("src/.zynml.zsnap")));
        assert!(!Frontend::is_frontend_file(Path::new("src/zynml.txt")));
    }
}
