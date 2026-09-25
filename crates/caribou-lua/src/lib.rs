//! The Lua frontend of Zyntax (`zyntax_lua`, the `zylua` command's
//! parser) as a language of the world. Lua parses on its own, so it needs
//! no grammar file under a root: the driver registers it when a root
//! contains a `.lua` file.
//!
//! A Lua module is a chunk, and loading it runs the chunk once, as
//! `require` does. The chunk reaches other languages' modules with
//! `require`: `require("haxe.ScaleValues")` is the world's
//! `haxe:ScaleValues`, and what it gives is used as Lua uses userdata (see
//! `caribou_zyntax`'s foreign objects). What a module gives other
//! languages is the table its chunk returns, which nothing publishes yet
//! (git-bug 64448a6c563dea6d3c1e3539e791176879fc0587f56919cff7c9fe627f7cfda4).

use std::path::Path;

use caribou_zyntax::zyntax_embed::{
    Collector, ExportedSymbol, ModuleArchitecture, TieredRuntime, TypedProgram,
};
use caribou_zyntax::{Language, Sources};

/// The frontend: name `lua`, modules laid out as Lua's `package.path`
/// lays them out.
pub struct Lua;

impl Lua {
    pub fn new() -> Lua {
        Lua
    }

    /// Whether `roots` hold a Lua module anywhere below them.
    pub fn present_in(roots: &[impl AsRef<Path>]) -> bool {
        caribou_zyntax::present_in(roots, "lua")
    }
}

impl Default for Lua {
    fn default() -> Lua {
        Lua::new()
    }
}

impl Language for Lua {
    fn name(&self) -> &str {
        "lua"
    }

    /// `?.lua` and `?/init.lua`: `game.util` is `game/util.lua`, or the
    /// package's `game/util/init.lua`.
    fn architectures(&self) -> Vec<ModuleArchitecture> {
        vec![ModuleArchitecture::PythonStyle {
            extension: "lua".to_owned(),
            init_file_name: "init.lua".to_owned(),
        }]
    }

    /// None of the functions a chunk compiles to: a module's value is
    /// what the chunk returns.
    fn exports(&self, _program: &TypedProgram) -> Vec<ExportedSymbol> {
        Vec::new()
    }

    /// The chunk, which loading the module runs.
    fn entry(&self) -> Option<&str> {
        Some(zyntax_lua::ENTRY)
    }

    /// What `zylua` gives its runtime: the library snapshot, the plugins
    /// the library calls, the entry point. Zyntax's own collector stays
    /// off under the core (see `caribou_zyntax`). `load` and a `require`
    /// of a file compile into this runtime, which keeps its address.
    fn prepare(&mut self, runtime: &mut TieredRuntime) -> Result<(), String> {
        zyntax_lua::register_runtime(runtime).map_err(|e| e.to_string())?;
        runtime.set_collector(Collector::None);
        zyntax_lua::set_runtime(runtime);
        Ok(())
    }

    fn parse(
        &self,
        _runtime: &TieredRuntime,
        source: &str,
        file: &str,
        _sources: &Sources,
    ) -> Result<TypedProgram, String> {
        zyntax_lua::parse_program(source, file).map_err(|e| e.render(file, source, false))
    }
}
