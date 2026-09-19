//! The Python frontend of Zyntax (`zyntax_python`, the `zypy` command's
//! parser) as a language of the world. Python parses on its own, with
//! ruff's parser, so it needs no grammar file under a root: the driver
//! registers it when a root contains a `.py` file. A module's functions
//! and classes publish as any Zyntax module's do (see `caribou_zyntax`),
//! and `import "game:scorer" for Scorer` in Wren or `import game.scorer.Scorer`
//! in Haxe reaches `game/scorer.py`.
//!
//! A Python module may import other Python modules of the project by
//! their dotted name (`import game.util`); the adapter's `Sources` finds
//! them, staged from a bundle or under the world's source roots.

use std::path::Path;

use caribou_zyntax::{Language, Sources};
use caribou_zyntax::zyntax_embed::{
    Collector, ExportedSymbol, ModuleArchitecture, TieredRuntime, TypedProgram,
};

/// The frontend: name `python`, modules laid out as Python lays them out.
pub struct Python;

impl Python {
    pub fn new() -> Python {
        Python
    }

    /// Whether `roots` hold a Python module anywhere below them.
    pub fn present_in(roots: &[impl AsRef<Path>]) -> bool {
        roots.iter().any(|root| has_py(root.as_ref()))
    }
}

impl Default for Python {
    fn default() -> Python {
        Python::new()
    }
}

fn has_py(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        if path.is_dir() {
            if has_py(&path) {
                return true;
            }
        } else if path.extension().is_some_and(|e| e == "py") {
            return true;
        }
    }
    false
}

impl Language for Python {
    fn name(&self) -> &str {
        "python"
    }

    /// Python's own layout: `game.tally` is `game/tally.py`, or the
    /// package's `game/tally/__init__.py`.
    fn architectures(&self) -> Vec<ModuleArchitecture> {
        vec![ModuleArchitecture::PythonStyle {
            extension: "py".to_owned(),
            init_file_name: "__init__.py".to_owned(),
        }]
    }

    /// Python's own rule, from the frontend: the module's top-level
    /// `def`s and `class`es, `__all__` when it names them, else every
    /// name without a leading underscore.
    fn exports(&self, program: &TypedProgram) -> Vec<ExportedSymbol> {
        program
            .source_files
            .first()
            .and_then(|file| zyntax_python::exports(&file.content).ok())
            .unwrap_or_default()
    }

    /// The class's `def`s without a leading underscore, from the
    /// frontend; the functions the frontend adds to a class are not
    /// among them.
    fn exported_members(&self, program: &TypedProgram, class: &str) -> Option<Vec<String>> {
        program
            .source_files
            .first()
            .and_then(|file| zyntax_python::class_exports(&file.content, class).ok())
            .flatten()
    }

    /// What `zypy` gives its runtime: the library snapshot, the plugins
    /// the library calls, the entry point. Zyntax's own collector stays
    /// off under the core (see `caribou_zyntax`).
    fn prepare(&mut self, runtime: &mut TieredRuntime) -> Result<(), String> {
        zyntax_python::register_runtime(runtime).map_err(|e| e.to_string())?;
        runtime.set_collector(Collector::None);
        Ok(())
    }

    /// The modules this one imports (`import game.util`) come from
    /// `sources`, by Python's own layout.
    fn parse(
        &self,
        _runtime: &TieredRuntime,
        source: &str,
        file: &str,
        sources: &Sources,
    ) -> Result<TypedProgram, String> {
        let architectures = self.architectures();
        let module_source = |name: &str| {
            let segments: Vec<String> = name.split('.').map(str::to_owned).collect();
            sources.module(&segments, &architectures)
        };
        zyntax_python::parse_program_with(source, file, &module_source)
            .map_err(|e| e.render(file, source, false))
    }
}
