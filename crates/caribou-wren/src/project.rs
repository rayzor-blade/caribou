//! Wren modules from a project's sources: the loader the registry asks
//! when a program first uses a module nothing has published.
//!
//! A module `game:hud` is the file `game/hud.wren` under the first of the
//! world's source roots (`caribou::world::source_roots`) that has it, or
//! what a bundle staged under that name (`stage`), which comes first:
//! its source, or its compiled form, the `wlbc` wren_lift's serializer
//! writes (`compile`, `WLBC`). It is loaded into the VM entered on this
//! thread under the path spelling, `game/hud`, which the registry
//! resolves for `game:hud`, and published as `publish.rs` publishes any
//! module. Loading runs the module's top level, so it happens on first
//! use, once the program that uses it is running. A module's own plain
//! imports are served the same way: `import "helper"` from `game/hud` is
//! `game/helper` when there is one, else `helper` at a root, and
//! `import "./helper"` and `import "../game/helper"` are `game/helper`
//! (`resolve`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use caribou::registry;
use caribou::world;
use wren_lift::runtime::engine::InterpretResult;
use wren_lift::runtime::vm::{CompiledModule, VM, VMConfig};

use crate::proto::current_vm;
use crate::publish::publish_module;

/// What a bundle staged for a module.
#[derive(Clone)]
pub enum Staged {
    Source(String),
    /// The compiled form, as `compile` writes it.
    Wlbc(Vec<u8>),
}

/// What a bundle staged, by module name.
static STAGED: LazyLock<Mutex<HashMap<String, Staged>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The text a staged compiled module was built from, by module name:
/// what its runtime errors render their line from.
static TEXT: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Keep `module` as the module `name`'s, to load when the program first
/// uses it; replaces a file under a root of the same name.
pub fn stage(name: &str, module: Staged) {
    STAGED.lock().unwrap().insert(name.to_owned(), module);
}

/// Keep `text` as the source the module `name` was compiled from.
pub fn stage_text(name: &str, text: String) {
    TEXT.lock().unwrap().insert(name.to_owned(), text);
}

fn text(name: &str) -> Option<String> {
    TEXT.lock().unwrap().get(name).cloned()
}

/// The format a compiled module is written as in a bundle: wren_lift's
/// `wlbc`, at the version this build reads.
pub static WLBC: LazyLock<String> =
    LazyLock::new(|| format!("wlbc@{}", wren_lift::serialize::VERSION));

/// Compile the modules `sources`, `(name, source)` pairs, to their
/// `wlbc` form, in the order given: a module's classes are known to
/// the modules compiled after it, so a module comes after what it
/// imports (`import_order`). A module that does not compile is an
/// error naming it; its diagnostics go where wren_lift reports them.
pub fn compile(sources: &[(String, String)]) -> Result<Vec<(String, Vec<u8>)>, String> {
    // A VM to compile on. Its heap seals wren_lift's seam, so a build
    // that shares its process with a session installs the seams first
    // (`caribou_driver::bundle::build`).
    let mut vm = VM::new(VMConfig::default());
    let mut out = Vec::with_capacity(sources.len());
    for (name, source) in sources {
        let bytes = vm
            .compile_source_to_blob(source)
            .map_err(|_| format!("`{name}` does not compile"))?;
        out.push((name.clone(), bytes));
    }
    Ok(out)
}

/// `names` ordered so a module comes after the modules of the set it
/// imports (`imported`); ties in the order given. A cycle leaves the
/// modules in it in that order.
pub fn import_order(sources: &[(String, String)]) -> Vec<String> {
    let names: Vec<&str> = sources.iter().map(|(n, _)| n.as_str()).collect();
    let present = |name: &str| names.contains(&name);
    let deps: Vec<Vec<String>> = sources
        .iter()
        .map(|(name, source)| {
            imports(source)
                .into_iter()
                .map(|import| imported(&import, name, present))
                .filter(|dep| present(dep) && dep != name)
                .collect()
        })
        .collect();
    let mut done: Vec<String> = Vec::with_capacity(names.len());
    let mut left: Vec<usize> = (0..names.len()).collect();
    while !left.is_empty() {
        let ready = left
            .iter()
            .position(|&i| deps[i].iter().all(|d| done.iter().any(|x| x == d)))
            .unwrap_or(0);
        done.push(names[left.remove(ready)].to_owned());
    }
    done
}

/// The imports of `source`, as written.
fn imports(source: &str) -> Vec<String> {
    use wren_lift::ast::Stmt;
    let parsed = wren_lift::parse::parser::parse(source);
    parsed
        .module
        .iter()
        .filter_map(|(stmt, _)| match stmt {
            Stmt::Import { module, .. } => Some(module.0.clone()),
            _ => None,
        })
        .collect()
}

/// Where a module is.
enum Found {
    Staged(Staged),
    File(PathBuf),
}

fn locate(name: &str) -> Option<Found> {
    if let Some(staged) = STAGED.lock().unwrap().get(name) {
        return Some(Found::Staged(staged.clone()));
    }
    find(name).map(Found::File)
}

/// The compiled form of a plain module name, when a bundle staged one,
/// with its source when the bundle carried it: wren_lift's loader for
/// a module's own imports.
pub fn bytecode(name: &str) -> Option<CompiledModule> {
    match STAGED.lock().unwrap().get(name)? {
        Staged::Wlbc(bytes) => Some(CompiledModule {
            bytes: bytes.clone(),
            source: text(name),
        }),
        Staged::Source(_) => None,
    }
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
    let name = in_namespace(namespace, module);
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
    let (staged, path) = match found {
        Found::Staged(staged) => (staged, None),
        Found::File(path) => {
            let source = std::fs::read_to_string(&path)
                .map_err(|e| format!("`{name}`: cannot read {}: {e}", path.display()))?;
            (Staged::Source(source), Some(path))
        }
    };
    if !vm.engine.modules.contains_key(&name) {
        let loaded = match &staged {
            Staged::Source(source) => vm.interpret(&name, source),
            Staged::Wlbc(bytes) => {
                vm.interpret_bytecode_with_source(&name, bytes, text(&name).as_deref())
            }
        };
        if loaded != InterpretResult::Success {
            return Err(format!("`{name}` did not load"));
        }
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

/// The module `namespace:module` names: under Wren's own namespace a
/// root's module by itself (`wren:scale` is `scale`, the module
/// `import "scale"` names), under any other its path in the namespace
/// (`game:hud` is `game/hud`).
pub fn in_namespace(namespace: &str, module: &str) -> String {
    if namespace == crate::LANGUAGE {
        module.to_owned()
    } else {
        format!("{namespace}/{module}")
    }
}

/// The Wren module `import` names from the module `from`, when it is one
/// of the project's: `ns:module` by `in_namespace`, anything else by
/// `resolve`. A scoped name (`@hatch:fmt`) keeps its spelling.
pub fn imported(import: &str, from: &str, exists: impl Fn(&str) -> bool) -> String {
    match import.split_once(':') {
        Some((ns, module)) if !ns.is_empty() && !ns.starts_with('@') => in_namespace(ns, module),
        _ => resolve(import, from, exists),
    }
}

/// The module a plain import names from the module `from`, by Wren's
/// rules: a path (`./x`, `../x`) from the importer's directory, a bare
/// name beside the importer when `exists` has a module there, else from
/// the roots. One file is one module, whichever of these reaches it. A
/// path that climbs above the roots keeps its spelling.
pub fn resolve(name: &str, from: &str, exists: impl Fn(&str) -> bool) -> String {
    let dir = from.rsplit_once('/').map_or("", |(dir, _)| dir);
    if name.starts_with("./") || name.starts_with("../") {
        let mut parts: Vec<&str> = dir.split('/').filter(|part| !part.is_empty()).collect();
        for part in name.strip_suffix(".wren").unwrap_or(name).split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return name.to_owned();
                    }
                }
                part => parts.push(part),
            }
        }
        return parts.join("/");
    }
    if !dir.is_empty() {
        let beside = format!("{dir}/{name}");
        if exists(&beside) {
            return beside;
        }
    }
    name.to_owned()
}

/// The source of a plain module name: staged, or from the roots. A
/// module staged compiled has none; `bytecode` has it.
pub fn source(name: &str) -> Option<String> {
    match locate(name)? {
        Found::Staged(Staged::Source(source)) => Some(source),
        Found::Staged(Staged::Wlbc(_)) => None,
        Found::File(path) => std::fs::read_to_string(path).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_a_file_names_one_module() {
        let exists = |name: &str| name == "game/b";
        assert_eq!(resolve("./b", "game/a", exists), "game/b");
        assert_eq!(resolve("./b.wren", "game/a", exists), "game/b");
        assert_eq!(resolve("../game/b", "game/a", exists), "game/b");
        assert_eq!(resolve("b", "game/a", exists), "game/b");
        assert_eq!(resolve("game/b", "game/a", exists), "game/b");
        assert_eq!(resolve("./scale", "main", exists), "scale");
        assert_eq!(resolve("c", "game/a", exists), "c");
        assert_eq!(resolve("../../c", "game/a", exists), "../../c");
        assert_eq!(in_namespace("game", "b"), "game/b");
        assert_eq!(in_namespace(crate::LANGUAGE, "scale"), "scale");
        assert_eq!(imported("game:b", "game/a", exists), "game/b");
        assert_eq!(imported("@hatch:fmt", "game/a", exists), "@hatch:fmt");
        let sources = [
            ("game/a".to_owned(), "import \"../game/b\" for B".to_owned()),
            ("game/b".to_owned(), "import \"game:c\" for C".to_owned()),
            ("game/c".to_owned(), String::new()),
        ];
        assert_eq!(import_order(&sources), ["game/c", "game/b", "game/a"]);
    }
}
