//! Wren modules from a project's sources: the loader the registry asks
//! when a program first uses a module nothing has published.
//!
//! A module `game:hud` is the file `game/hud.wren` under the first of the
//! world's source roots (`caribou::world::source_roots`) that has it, or
//! the source a bundle staged under that name (`stage`), which comes
//! first. It is loaded into the VM entered on this thread under the path
//! spelling, `game/hud`, which the registry resolves for `game:hud`, and
//! published as `publish.rs` publishes any module. Loading runs the
//! module's top level, so it happens on first use, once the program that
//! uses it is running. A module's own plain imports are served the same
//! way: `import "helper"` from `game/hud` is `game/helper` when there is
//! one, else `helper` at a root.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use caribou::registry;
use caribou::world;
use wren_lift::runtime::engine::InterpretResult;
use wren_lift::runtime::vm::VM;

use crate::proto::current_vm;
use crate::publish::publish_module;

/// Sources a bundle staged, by module name.
static STAGED: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Keep `source` as the module `name`'s, to load when the program first
/// uses it; replaces a file under a root of the same name.
pub fn stage(name: &str, source: String) {
    STAGED.lock().unwrap().insert(name.to_owned(), source);
}

/// Where a module's source is.
enum Found {
    Staged(String),
    File(PathBuf),
}

fn locate(name: &str) -> Option<Found> {
    if let Some(source) = STAGED.lock().unwrap().get(name) {
        return Some(Found::Staged(source.clone()));
    }
    find(name).map(Found::File)
}

/// The file for module `name` (`game/hud`), under the first root that has
/// it.
pub fn find(name: &str) -> Option<PathBuf> {
    world::source_roots()
        .into_iter()
        .map(|root| root.join(format!("{name}.wren")))
        .find(|path| path.is_file())
}

/// Whether a module `name` has a source, staged or under a root.
pub fn exists(name: &str) -> bool {
    STAGED.lock().unwrap().contains_key(name) || find(name).is_some()
}

/// The registry's loader for Wren: load and publish `namespace:module`
/// from the project when it is there.
pub fn load(namespace: &str, module: &str) -> Result<bool, String> {
    let name = format!("{namespace}/{module}");
    let Some(found) = locate(&name) else {
        return Ok(false);
    };
    let vm = current_vm();
    if vm.is_null() {
        return Err(format!(
            "`{name}` cannot load: no Wren VM is entered on this thread"
        ));
    }
    let vm = unsafe { &mut *vm };
    let (source, path) = match found {
        Found::Staged(source) => (source, None),
        Found::File(path) => {
            let source = std::fs::read_to_string(&path)
                .map_err(|e| format!("`{name}`: cannot read {}: {e}", path.display()))?;
            (source, Some(path))
        }
    };
    if !vm.engine.modules.contains_key(&name)
        && vm.interpret(&name, &source) != InterpretResult::Success
    {
        return Err(format!("`{name}` did not load"));
    }
    publish_module(vm, &name).map_err(|e| format!("`{name}`: {e}"))?;
    // Where it came from, for a source watch; a staged module has no
    // file to watch.
    if let Some(path) = path {
        registry::set_source(crate::lang(), &name, path);
    }
    Ok(true)
}

/// Load the module `name` afresh from the roots, in place: wren_lift
/// re-runs it with its classes' identity kept and its compiled bodies
/// dropped, the registry gets its interface again, and the program's
/// `Hatch.onReload` callbacks hear of it. The module must be loaded.
pub fn reload(vm: &mut VM, name: &str) -> Result<(), String> {
    if !vm.engine.modules.contains_key(name) {
        return Err(format!("`{name}` is not loaded"));
    }
    vm.reload_module(name)?;
    publish_module(vm, name).map_err(|e| format!("`{name}`: {e}"))?;
    vm.notify_reloaded(name);
    Ok(())
}

/// A plain import's module name: `name` beside the importer when a
/// source is there, else `name` itself.
pub fn relative(name: &str, from: &str) -> String {
    if let Some((dir, _)) = from.rsplit_once('/') {
        let beside = format!("{dir}/{name}");
        if exists(&beside) {
            return beside;
        }
    }
    name.to_owned()
}

/// The source of a plain module name: staged, or from the roots.
pub fn source(name: &str) -> Option<String> {
    match locate(name)? {
        Found::Staged(source) => Some(source),
        Found::File(path) => std::fs::read_to_string(path).ok(),
    }
}
