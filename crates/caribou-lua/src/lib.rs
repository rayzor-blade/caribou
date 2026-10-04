//! The Lua frontend of Zyntax (`zyntax_lua`, the `zylua` command's
//! parser) as a language of the world. Lua parses on its own, so it needs
//! no grammar file under a root: the driver registers it when a root
//! contains a `.lua` file.
//!
//! A Lua module is a chunk, and loading it runs the chunk once, as
//! `require` does: with Lua embedded as a C host embeds it (see
//! [`host`]), the chunk loaded with Lua's own `load` and run with a
//! protected call, so an error in it is the load's. The chunk reaches
//! other languages' modules with `require`: `require("haxe.ScaleValues")`
//! is the world's `haxe:ScaleValues`, used as Lua uses userdata (see
//! `caribou_zyntax`'s foreign objects). What the module gives other
//! languages is the table its chunk returns: its functions are the
//! module's, and a Lua table or function that crosses is a core object
//! that answers as Lua would.

mod host;

use std::path::Path;

use caribou_abi::LangId;
use caribou_zyntax::zyntax_embed::{
    ExportedSymbol, ModuleArchitecture, TieredRuntime, TypedProgram,
};
use caribou_zyntax::{Language, RunModule, Sources};

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

    /// None of the functions a chunk compiles to: what a module exports
    /// is its value's (see [`Language::run_module`]).
    fn exports(&self, _program: &TypedProgram) -> Vec<ExportedSymbol> {
        Vec::new()
    }

    /// The chunk, run with Lua's own `load` and a protected call.
    fn run_module(
        &self,
        name: &str,
        source: &str,
        file: &str,
    ) -> Option<Result<RunModule, String>> {
        Some(host::run_module(name, source, file))
    }

    /// The chunk's exports, as its types know them.
    fn describe_module(
        &self,
        name: &str,
        source: &str,
        file: &str,
    ) -> Option<Result<RunModule, String>> {
        Some(host::describe_module(name, source, file))
    }

    /// Its classes' instances report the names the classes go by.
    fn published(&self, iface: &caribou::registry::Interface) -> Result<(), String> {
        host::published(iface)
    }

    fn assigned(&self, lang: LangId) {
        host::assign(lang);
    }

    /// What `zylua` gives its runtime: the library snapshot and the
    /// plugins the library calls; its heap is the core's (see
    /// `caribou_zyntax`). Then the state opens for the host: chunks
    /// compile into this runtime, which keeps its address.
    fn prepare(&mut self, runtime: &mut TieredRuntime) -> Result<(), String> {
        zyntax_lua::register_runtime(runtime).map_err(|e| e.to_string())?;
        host::open(runtime)
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
