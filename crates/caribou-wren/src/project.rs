//! Wren modules from a project's sources: the loader the registry asks
//! when a program first uses a module nothing has published.
//!
//! A module `game:hud` is the file `game/hud.wren` under the first of the
//! world's source roots (`caribou::world::source_roots`) that has it. It
//! is loaded into the VM entered on this thread under the path spelling,
//! `game/hud`, which the registry resolves for `game:hud`, and published
//! as `publish.rs` publishes any module. Loading runs the module's top
//! level, so it happens on first use, once the program that uses it is
//! running. A module's own plain imports are served from the same roots:
//! `import "helper"` from `game/hud` is `game/helper.wren` when there is
//! one, else `helper.wren` at a root.

use std::path::PathBuf;

use caribou::world;
use wren_lift::runtime::engine::InterpretResult;
use wren_lift::runtime::vm::VM;

use crate::proto::current_vm;
use crate::publish::publish_module;

/// The file for module `name` (`game/hud`), under the first root that has
/// it.
pub fn find(name: &str) -> Option<PathBuf> {
    world::source_roots()
        .into_iter()
        .map(|root| root.join(format!("{name}.wren")))
        .find(|path| path.is_file())
}

/// The registry's loader for Wren: load and publish `namespace:module`
/// from the project when it is there.
pub fn load(namespace: &str, module: &str) -> Result<bool, String> {
    let name = format!("{namespace}/{module}");
    let Some(path) = find(&name) else {
        return Ok(false);
    };
    let vm = current_vm();
    if vm.is_null() {
        return Err(format!(
            "`{name}` cannot load: no Wren VM is entered on this thread"
        ));
    }
    let vm = unsafe { &mut *vm };
    load_into(vm, &name, &path)?;
    Ok(true)
}

/// Load `path` as module `name` into `vm`, unless it is loaded, and
/// publish it.
pub fn load_into(vm: &mut VM, name: &str, path: &PathBuf) -> Result<(), String> {
    if !vm.engine.modules.contains_key(name) {
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("`{name}`: cannot read {}: {e}", path.display()))?;
        if vm.interpret(name, &source) != InterpretResult::Success {
            return Err(format!("`{name}` did not load: {}", path.display()));
        }
    }
    publish_module(vm, name).map_err(|e| format!("`{name}`: {e}"))?;
    Ok(())
}

/// A plain import's module name: `name` beside the importer when a file
/// is there, else `name` itself.
pub fn relative(name: &str, from: &str) -> String {
    if let Some((dir, _)) = from.rsplit_once('/') {
        let beside = format!("{dir}/{name}");
        if find(&beside).is_some() {
            return beside;
        }
    }
    name.to_owned()
}

/// The source of a plain module name, from the roots.
pub fn source(name: &str) -> Option<String> {
    let path = find(name)?;
    std::fs::read_to_string(path).ok()
}
