//! Relocatable objects contributed by resident language frontends to an
//! ahead-of-time build. Ash owns the final link; each frontend owns how its
//! source becomes an object and how its exported modules are described.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use ash_core::native_lib::{HostLink, Word};
use caribou::describe::ModuleDesc;
use caribou::registry::TypeRef;
use wren_lift::codegen::aot::{AotBundleMeta, AotModule, walk_imports};
use wren_lift::codegen::llvm_aot::{
    AotBuild, AotEntry, LlvmTarget, compile_modules_to_llvm_object_with,
};

/// A frontend that can contribute relocatable objects to an AOT build.
pub trait Emitter {
    fn language(&self) -> &str;

    /// Compile the modules under `sources`. `haxe` is the program's Haxe
    /// classes, which a module may import.
    fn emit(
        &self,
        sources: &[PathBuf],
        haxe: &[ModuleDesc],
        triple: &str,
        out_dir: &Path,
    ) -> Result<Vec<Artifact>>;
}

/// The caller-side ABI adapter for one frontend's compiled exports.
pub trait Linker: Send + Sync {
    fn host_link(
        &self,
        artifact: &Artifact,
        member: &caribou_ash::link::Member,
    ) -> Option<HostLink>;
}

/// One language frontend's relocatable object and the modules it exports.
pub struct Artifact {
    pub language: String,
    pub object: PathBuf,
    pub modules: Vec<(String, ModuleDesc)>,
    /// The Haxe members the object calls, for Ash to export.
    pub exports: Vec<ash_core::host_export::HostExport>,
    linker: Box<dyn Linker>,
}

impl Artifact {
    pub fn new(
        language: impl Into<String>,
        object: PathBuf,
        modules: Vec<(String, ModuleDesc)>,
        linker: Box<dyn Linker>,
    ) -> Self {
        Self {
            language: language.into(),
            object,
            modules,
            exports: Vec::new(),
            linker,
        }
    }

    /// A module addressed by a Caribou namespace and language module name.
    pub fn module(&self, namespace: &str, module: &str) -> Option<(&str, &ModuleDesc)> {
        let nested = format!("{namespace}/{module}");
        self.modules
            .iter()
            .find(|(name, _)| *name == nested || (namespace == self.language && name == module))
            .map(|(name, desc)| (name.as_str(), desc))
    }
}

/// All language objects that Ash will link into one program.
pub struct Artifacts {
    items: Vec<Artifact>,
}

impl Artifacts {
    /// Compile every requested language frontend that can emit a relocatable
    /// object for `triple`.
    pub fn build(
        declared: &[String],
        sources: &[PathBuf],
        haxe: &[ModuleDesc],
        triple: &str,
        out_dir: &Path,
    ) -> Result<Self> {
        Self::build_with(declared, sources, haxe, triple, out_dir, &[&WrenEmitter])
    }

    /// Compile with the frontend emitters available in this driver.
    pub fn build_with(
        declared: &[String],
        sources: &[PathBuf],
        haxe: &[ModuleDesc],
        triple: &str,
        out_dir: &Path,
        emitters: &[&dyn Emitter],
    ) -> Result<Self> {
        let unsupported: Vec<&str> = declared
            .iter()
            .map(String::as_str)
            .filter(|language| {
                *language != "haxe"
                    && !emitters
                        .iter()
                        .any(|emitter| emitter.language() == *language)
            })
            .collect();
        if !unsupported.is_empty() {
            bail!(
                "no relocatable AOT object emitter for {}: Zyntax frontends currently emit runtime modules rather than objects for Ash's linker",
                unsupported.join(", ")
            );
        }

        let mut items = Vec::new();
        for emitter in emitters {
            if declared.is_empty()
                || declared
                    .iter()
                    .any(|language| language == emitter.language())
            {
                items.extend(emitter.emit(sources, haxe, triple, out_dir)?);
            }
        }
        Ok(Self { items })
    }

    /// Objects passed straight to Ash's final linker.
    pub fn objects(&self) -> Vec<PathBuf> {
        self.items
            .iter()
            .map(|artifact| artifact.object.clone())
            .collect()
    }

    /// The Haxe members the objects call, for Ash to export.
    pub fn exports(&self) -> Vec<ash_core::host_export::HostExport> {
        self.items
            .iter()
            .flat_map(|artifact| artifact.exports.iter().cloned())
            .collect()
    }

    /// The first frontend artifact that can bind `member` for Ash.
    pub fn host_link(&self, member: &caribou_ash::link::Member) -> Option<HostLink> {
        self.items
            .iter()
            .find_map(|artifact| artifact.linker.host_link(artifact, member))
    }
}

struct WrenEmitter;

impl Emitter for WrenEmitter {
    fn language(&self) -> &str {
        "wren"
    }

    fn emit(
        &self,
        sources: &[PathBuf],
        haxe: &[ModuleDesc],
        triple: &str,
        out_dir: &Path,
    ) -> Result<Vec<Artifact>> {
        Ok(build_wren(sources, haxe, triple, &out_dir.join("wren.o"))?
            .into_iter()
            .collect())
    }
}

/// Compile the Wren modules under `sources` for `triple` into `out`, or
/// `None` when there are none.
fn build_wren(
    sources: &[PathBuf],
    haxe: &[ModuleDesc],
    triple: &str,
    out: &Path,
) -> Result<Option<Artifact>> {
    let mut found = Vec::new();
    for root in sources.iter().filter(|root| root.is_dir()) {
        crate::bundle::wren_modules(root, root, &mut found)?;
    }
    if found.is_empty() {
        return Ok(None);
    }
    found.sort();

    let mut modules: Vec<AotModule> = Vec::new();
    let mut bundle = AotBundleMeta::default();
    let mut described = Vec::new();
    for (name, path) in &found {
        let walk = walk_imports(path).map_err(|error| anyhow!("{}: {error:?}", path.display()))?;
        for module in walk.modules {
            if !modules.iter().any(|seen| seen.name == module.name) {
                modules.push(module);
            }
        }
        bundle
            .native_search_paths
            .extend(walk.bundle.native_search_paths);
        bundle.native_libs.extend(walk.bundle.native_libs);
        let source = std::fs::read_to_string(path)?;
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("module");
        let desc = caribou_wren::describe::describe_source(stem, &source)
            .map_err(|error| anyhow!("{}: {error}", path.display()))?;
        described.push((name.clone(), desc));
    }
    name_imports(&mut modules, &found);
    let (foreign, exports) = crate::foreign::plan(&mut modules, haxe);
    let modules = dependencies_first(modules);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let target = LlvmTarget::new(triple, None, None);
    compile_modules_to_llvm_object_with(
        &modules,
        &bundle,
        &target,
        // On wasm a compiled fiber gets a stack of its own, as the driver
        // links every wasm program with Ash's fiber transform.
        &AotBuild {
            entry: AotEntry::Library,
            fibers: target.is_wasm(),
            foreign: &foreign,
            ..AotBuild::default()
        },
        out,
    )
    .map_err(|error| anyhow!("compiling the Wren modules: {error:?}"))?;
    let mut artifact = Artifact::new("wren", out.to_path_buf(), described, Box::new(WrenLinker));
    artifact.exports = exports;
    Ok(Some(artifact))
}

/// Give each module under the roots its name there, and each of its
/// imports the module it names by the program's rules
/// (`caribou_wren::project::imported`), so every spelling of one file
/// binds to that one module. An import of another language's module, or
/// of a file outside the roots, keeps its spelling.
fn name_imports(modules: &mut [AotModule], found: &[(String, PathBuf)]) {
    let names: HashSet<&str> = found.iter().map(|(name, _)| name.as_str()).collect();
    let by_path: HashMap<PathBuf, &str> = found
        .iter()
        .filter_map(|(name, path)| Some((std::fs::canonicalize(path).ok()?, name.as_str())))
        .collect();
    for module in modules.iter_mut() {
        let Some(&own) = by_path.get(Path::new(&module.name)) else {
            continue;
        };
        module.request_name = own.to_owned();
        module.aliases.clear();
        for source in module.module_var_sources.iter_mut().flatten() {
            let name =
                caribou_wren::project::imported(&source.module, own, |name| names.contains(name));
            if names.contains(name.as_str()) {
                source.module = name;
            }
        }
    }
}

/// `modules` with each after the modules it imports: WrenLift binds an
/// import only to a module before its importer. A cycle keeps the order
/// its modules were found in.
fn dependencies_first(modules: Vec<AotModule>) -> Vec<AotModule> {
    let index: HashMap<&str, usize> = modules
        .iter()
        .enumerate()
        .flat_map(|(at, module)| {
            std::iter::once(module.request_name.as_str())
                .chain(module.aliases.iter().map(String::as_str))
                .map(move |name| (name, at))
        })
        .collect();
    let imports: Vec<Vec<usize>> = modules
        .iter()
        .map(|module| {
            module
                .module_var_sources
                .iter()
                .flatten()
                .filter_map(|source| index.get(source.module.as_str()).copied())
                .collect()
        })
        .collect();
    fn visit(at: usize, imports: &[Vec<usize>], seen: &mut [bool], order: &mut Vec<usize>) {
        if std::mem::replace(&mut seen[at], true) {
            return;
        }
        for &import in &imports[at] {
            visit(import, imports, seen, order);
        }
        order.push(at);
    }
    let mut seen = vec![false; modules.len()];
    let mut order = Vec::with_capacity(modules.len());
    for at in 0..modules.len() {
        visit(at, &imports, &mut seen, &mut order);
    }
    let mut slots: Vec<Option<AotModule>> = modules.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|at| slots[at].take())
        .collect()
}

struct WrenLinker;

impl Linker for WrenLinker {
    fn host_link(&self, wren: &Artifact, member: &caribou_ash::link::Member) -> Option<HostLink> {
        use caribou::link::Kind;
        let (module, desc) = wren.module(&member.namespace, &member.module)?;
        let described = desc
            .classes
            .iter()
            .find(|class| class.name == member.class)?
            .members
            .iter()
            .find(|described| {
                described.exported
                    && Kind::from(described.kind) == member.kind
                    && described.name == member.name
                    && described.params.len() == member.arity
            })?;
        let receiver = matches!(member.kind, Kind::Method | Kind::Getter | Kind::Setter);
        let mut arg_casts = Vec::with_capacity(member.arity + usize::from(receiver));
        if receiver {
            arg_casts.push(Some(FACE.0.to_owned()));
        }
        for param in &described.params {
            arg_casts.push(Some(wren_casts(&param.ty, desc)?.0.to_owned()));
        }
        let (ret_cast, init) = match (&described.ret, member.kind) {
            (_, Kind::Constructor) => (None, Some("caribou_wren_bind_face".to_owned())),
            (TypeRef::Void | TypeRef::Dyn, _) => (None, None),
            (ty, _) => (Some(wren_casts(ty, desc)?.1.to_owned()), None),
        };
        Some(HostLink {
            symbol: caribou::link::symbol(
                "wren",
                module,
                &member.class,
                member.kind,
                &member.name,
                member.arity,
            ),
            params: vec![Word::I64; arg_casts.len()],
            ret: Some(Word::I64),
            arg_casts,
            ret_cast,
            after: Some("caribou_wren_raise_pending".to_owned()),
            init,
            library: None,
        })
    }
}

const FACE: (&str, &str) = ("caribou_wren_from_haxe_face", "caribou_wren_to_haxe_face");

fn wren_casts(
    ty: &TypeRef,
    desc: &caribou::describe::ModuleDesc,
) -> Option<(&'static str, &'static str)> {
    Some(match ty {
        // Ash converts a NaN-boxed number inline.
        TypeRef::Float => ("ash:box_f64", "ash:unbox_f64"),
        TypeRef::Int => ("caribou_wren_from_int", "caribou_wren_to_int"),
        TypeRef::Bool => ("caribou_wren_from_bool", "caribou_wren_to_bool"),
        TypeRef::Str => (
            "caribou_wren_from_haxe_string",
            "caribou_wren_to_haxe_string",
        ),
        TypeRef::Buffer => ("caribou_wren_from_haxe_bytes", "caribou_wren_to_haxe_bytes"),
        TypeRef::Function { .. } => (
            "caribou_wren_from_haxe_function",
            "caribou_wren_to_haxe_function",
        ),
        TypeRef::Object(name)
            if desc
                .classes
                .iter()
                .any(|class| &class.type_name == name || &class.name == name) =>
        {
            FACE
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_frontend_that_cannot_contribute_an_object() {
        let error = Artifacts::build(
            &["haxe".to_owned(), "python".to_owned()],
            &[],
            &[],
            "wasm32-wasip1",
            Path::new("unused"),
        )
        .err()
        .expect("Python does not have a relocatable AOT emitter yet");
        assert!(error.to_string().contains("python"));
    }

    struct EmptyEmitter;

    impl Emitter for EmptyEmitter {
        fn language(&self) -> &str {
            "python"
        }

        fn emit(
            &self,
            _: &[PathBuf],
            _: &[ModuleDesc],
            _: &str,
            _: &Path,
        ) -> Result<Vec<Artifact>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn accepts_any_registered_frontend_emitter() {
        Artifacts::build_with(
            &["haxe".to_owned(), "python".to_owned()],
            &[],
            &[],
            "wasm32-wasip1",
            Path::new("unused"),
            &[&EmptyEmitter],
        )
        .expect("a registered frontend participates in the common object pipeline");
    }
}
