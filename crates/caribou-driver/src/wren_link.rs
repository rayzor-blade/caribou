//! A project's Wren modules compiled ahead of time by WrenLift, as one
//! library object a program links: its static constructor registers the
//! modules' bodies, which the runtime runs at program start, and each
//! `#export` member is a symbol by caribou's link rule, the module being
//! its path under the sources (`bench/tally`).

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use caribou::describe::ModuleDesc;
use wren_lift::codegen::aot::{AotBundleMeta, AotModule, walk_imports};
use wren_lift::codegen::llvm_aot::{AotEntry, LlvmTarget, compile_modules_to_llvm_object_as};

/// The object, and the modules in it a program can call, described.
pub struct Library {
    pub object: PathBuf,
    pub modules: Vec<(String, ModuleDesc)>,
}

impl Library {
    /// The module a Haxe face names by its namespace and module: nested
    /// under the namespace's directory, or a file at a source root, whose
    /// namespace is the language's own.
    pub fn module(&self, namespace: &str, module: &str) -> Option<(&str, &ModuleDesc)> {
        let nested = format!("{namespace}/{module}");
        self.modules
            .iter()
            .find(|(name, _)| *name == nested || (namespace == "wren" && name == module))
            .map(|(name, desc)| (name.as_str(), desc))
    }
}

/// Compile the Wren modules under `sources` for `triple` into `out`, or
/// `None` when there are none.
pub fn build(sources: &[PathBuf], triple: &str, out: &Path) -> Result<Option<Library>> {
    let mut found = Vec::new();
    for root in sources.iter().filter(|r| r.is_dir()) {
        crate::bundle::wren_modules(root, root, &mut found)?;
    }
    if found.is_empty() {
        return Ok(None);
    }
    found.sort();
    // Each module with what it imports, dependencies first; a module both
    // imported and found keeps the name it is found under.
    let mut modules: Vec<AotModule> = Vec::new();
    let mut bundle = AotBundleMeta::default();
    let mut described = Vec::new();
    for (name, path) in &found {
        let walk = walk_imports(path).map_err(|e| anyhow!("{}: {e:?}", path.display()))?;
        let last = walk.modules.len().saturating_sub(1);
        for (i, mut module) in walk.modules.into_iter().enumerate() {
            if i == last {
                module.request_name = name.clone();
            }
            match modules.iter_mut().find(|m| m.name == module.name) {
                Some(seen) if i == last => seen.request_name = name.clone(),
                Some(_) => {}
                None => modules.push(module),
            }
        }
        bundle.native_search_paths.extend(walk.bundle.native_search_paths);
        bundle.native_libs.extend(walk.bundle.native_libs);
        let source = std::fs::read_to_string(path)?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("module");
        let desc = caribou_wren::describe::describe_source(stem, &source)
            .map_err(|e| anyhow!("{}: {e}", path.display()))?;
        described.push((name.clone(), desc));
    }
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    compile_modules_to_llvm_object_as(
        &modules,
        &bundle,
        &LlvmTarget::new(triple, None, None),
        AotEntry::Library,
        out,
    )
    .map_err(|e| anyhow!("compiling the Wren modules: {e:?}"))?;
    Ok(Some(Library {
        object: out.to_path_buf(),
        modules: described,
    }))
}
