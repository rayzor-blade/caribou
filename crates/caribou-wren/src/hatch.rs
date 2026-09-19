//! Hatch packages under caribou: what a project's `hatchfile` depends on,
//! resolved as `hatch` resolves it, and staged in the VM as wren_lift
//! stages them, so `import "@hatch:noise"` in a project's Wren module
//! finds the package, its plugin included, without the package or the
//! plugin knowing about caribou. A package's native library is
//! wren_lift's to open: it resolves the `wlift_plugin_*` symbols against
//! this process, which exports them for that.
//!
//! A bundle carries each package whole, as a module section of format
//! `hatch`; the adapter holds it for the VM and the driver stages it
//! once the VM exists, as it does a project's.

use std::cell::RefCell;
use std::path::Path;

use wren_lift::hatch as wh;
use wren_lift::runtime::engine::InterpretResult;
use wren_lift::runtime::vm::VM;

/// A package as bytes, under its name (`@hatch:noise`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub bytes: Vec<u8>,
}

thread_local! {
    static HELD: RefCell<Vec<Package>> = const { RefCell::new(Vec::new()) };
}

/// Keep `package` for the VM to stage.
pub fn hold(package: Package) {
    HELD.with(|held| {
        let mut held = held.borrow_mut();
        if !held.iter().any(|p| p.name == package.name) {
            held.push(package);
        }
    });
}

/// The packages the `hatchfile` of each root depends on, and what those
/// depend on, each once: a path dependency built from its workspace, a
/// version from the cache `hatch install` fills.
pub fn dependencies(roots: &[impl AsRef<Path>]) -> Result<Vec<Package>, String> {
    let mut out: Vec<Package> = Vec::new();
    for root in roots {
        let root = root.as_ref();
        let Ok(text) = std::fs::read_to_string(root.join(wh::HATCHFILE)) else {
            continue;
        };
        let manifest: wh::Manifest = toml::from_str(&text)
            .map_err(|e| format!("{}: {e}", root.join(wh::HATCHFILE).display()))?;
        resolve_into(root, &manifest, &mut out)?;
    }
    Ok(out)
}

fn resolve_into(
    root: &Path,
    manifest: &wh::Manifest,
    out: &mut Vec<Package>,
) -> Result<(), String> {
    for (name, dep) in &manifest.dependencies {
        if out.iter().any(|p| &p.name == name) {
            continue;
        }
        let bytes = wh::resolve_dependency_bytes(root, name, dep, None)
            .map_err(|e| format!("resolving `{name}`: {e}"))?;
        let inner = wh::load(&bytes).map_err(|e| format!("`{name}` is not a hatch: {e}"))?;
        out.push(Package {
            name: name.clone(),
            bytes,
        });
        resolve_into(root, &inner.manifest, out)?;
    }
    Ok(())
}

/// Stage every held package in `vm`: its modules wait for their first
/// import, its native libraries are registered. The VM reports what a
/// package that does not stage said; the error names the package.
pub fn stage(vm: &mut VM) -> Result<(), String> {
    let held = HELD.with(|held| std::mem::take(&mut *held.borrow_mut()));
    for package in held {
        match crate::with_vm(vm, |vm| vm.stage_hatch_modules(&package.bytes)) {
            InterpretResult::Success => {}
            _ => return Err(format!("the package `{}` did not stage", package.name)),
        }
    }
    Ok(())
}
