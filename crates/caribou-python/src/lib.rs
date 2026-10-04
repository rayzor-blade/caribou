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

use caribou_zyntax::zyntax_embed::{
    ExportedSymbol, ModuleArchitecture, TieredRuntime, TypedProgram,
};
use caribou_zyntax::{Language, Sources};

/// The frontend: name `python`, modules laid out as Python lays them out.
pub struct Python;

impl Python {
    pub fn new() -> Python {
        Python
    }

    /// Whether `roots` hold a Python module anywhere below them.
    pub fn present_in(roots: &[impl AsRef<Path>]) -> bool {
        caribou_zyntax::present_in(roots, "py")
    }
}

impl Default for Python {
    fn default() -> Python {
        Python::new()
    }
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

    /// A module's statements, which importing it runs.
    fn entry(&self) -> Option<&str> {
        Some(zyntax_python::ENTRY)
    }

    /// Each module's own description of what it raised: the exception's
    /// type and text.
    fn describe(&self) -> Option<&str> {
        Some(zyntax_python::DESCRIBE)
    }

    /// What `zypy` gives its runtime: the library snapshot, the plugins
    /// the library calls, the entry point. Its heap is the core's (see
    /// `caribou_zyntax`).
    fn prepare(&mut self, runtime: &mut TieredRuntime) -> Result<(), String> {
        zyntax_python::register_runtime(runtime).map_err(|e| e.to_string())
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
        // Imported, not run as a program: an exception its body does not
        // catch fails the import instead of ending the process.
        zyntax_python::parse_module_with_host(
            source,
            file,
            &module_source,
            &caribou_zyntax::host::module,
        )
        .map_err(|e| e.render(file, source, false))
    }
}
