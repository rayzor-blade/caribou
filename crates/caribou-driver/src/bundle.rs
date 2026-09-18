//! Building a bundle from a project: the program, and every Wren module
//! under the project's source roots, compiled. The namespaces are the
//! project's (`project::namespaces`), so a program run from the bundle
//! sees what it saw from the directory.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use caribou::bundle::{Bundle, Entry, Manifest, Section, SectionKind};

use crate::project;

/// The extension a bundle is written with.
pub const EXTENSION: &str = "cb";

/// The bundle for the program at `program`, with the Wren modules under
/// `roots`.
pub fn build(program: &Path, roots: &[PathBuf]) -> Result<Bundle> {
    let name = program
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("{} has no name", program.display()))?
        .to_owned();
    let imported: Vec<String> = caribou_ash::imports_in(program)?
        .into_iter()
        .map(|(namespace, _)| namespace)
        .collect();
    // The plugins beside the program are the ones its namespaces cover,
    // and they ship in the bundle for this target.
    let plugins = caribou_plugin::load_dir(
        &program
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("plugins"),
    )
    .map_err(|e| anyhow!("{e}"))?;
    let plugin_names: Vec<String> = plugins.iter().map(|p| p.name().to_owned()).collect();
    let namespaces = project::namespaces(roots, &imported, &plugin_names);
    let mut sections = vec![Section {
        kind: SectionKind::Module,
        lang: "haxe".to_owned(),
        format: "hl".to_owned(),
        name: name.clone(),
        data: std::fs::read(program).with_context(|| format!("reading {}", program.display()))?,
    }];
    let mut sources: Vec<(String, String)> = Vec::new();
    for root in roots {
        let mut modules = Vec::new();
        wren_modules(root, root, &mut modules)?;
        modules.sort();
        for (module, path) in modules {
            // The first root with a module of a name is the one a run
            // from the directory would have found.
            if sources.iter().any(|(name, _)| *name == module) {
                continue;
            }
            let source = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            sources.push((module, source));
        }
    }
    // Compiled after what they import, so a class of one module is known
    // to the modules that use it.
    let order = caribou_wren::project::import_order(&sources);
    sources.sort_by_key(|(name, _)| order.iter().position(|n| n == name));
    for (module, bytes) in caribou_wren::project::compile(&sources).map_err(|e| anyhow!(e))? {
        sections.push(Section {
            kind: SectionKind::Module,
            lang: "wren".to_owned(),
            format: caribou_wren::project::WLBC.clone(),
            name: module,
            data: bytes,
        });
    }
    // The hatch packages the roots depend on, whole: wren_lift reads its
    // own form, native libraries included.
    for package in caribou_wren::hatch::dependencies(roots).map_err(|e| anyhow!(e))? {
        sections.push(Section {
            kind: SectionKind::Module,
            lang: "wren".to_owned(),
            format: "hatch".to_owned(),
            name: package.name,
            data: package.bytes,
        });
    }
    // The text beside each, for the errors a module raises at run time.
    for (module, source) in sources {
        sections.push(Section {
            kind: SectionKind::Source,
            lang: "wren".to_owned(),
            format: String::new(),
            name: module,
            data: source.into_bytes(),
        });
    }
    for plugin in &plugins {
        let path = plugin.path();
        sections.push(Section {
            kind: SectionKind::NativeLib,
            lang: String::new(),
            format: caribou::bundle::target(),
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| anyhow!("{} has no file name", path.display()))?
                .to_owned(),
            data: std::fs::read(path).with_context(|| format!("reading {}", path.display()))?,
        });
    }
    Ok(Bundle {
        manifest: Manifest {
            name: name.clone(),
            entry: Entry {
                lang: "haxe".to_owned(),
                module: name,
            },
            namespaces,
        },
        sections,
    })
}

/// Every `.wren` under `dir`, as `(module name, path)`: the path under
/// `root` without the extension, `game/hud`.
pub fn wren_modules(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if file_name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            wren_modules(root, &path, out)?;
        } else if path.extension().is_some_and(|e| e == "wren") {
            let relative = path.strip_prefix(root)?.with_extension("");
            let module = relative
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect::<Vec<_>>()
                .join("/");
            out.push((module, path));
        }
    }
    Ok(())
}

/// The bundle's plugins for this target, loaded. A library loads from a
/// file, so each is written under the temporary directory by the hash
/// of its bytes, once per content; a library already there is loaded as
/// it is.
pub fn plugins(bundle: &Bundle) -> Result<Vec<caribou_plugin::Plugin>> {
    let mut out = Vec::new();
    for section in bundle.native_libs(&caribou::bundle::target()) {
        let dir = std::env::temp_dir()
            .join("caribou-plugins")
            .join(format!("{:016x}", fnv(&section.data)));
        let path = dir.join(&section.name);
        if !path.is_file() {
            std::fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
            // Written whole before it has its name, so a reader never
            // sees a partial library.
            let part = dir.join(format!("{}.{}", section.name, std::process::id()));
            std::fs::write(&part, &section.data).with_context(|| format!("writing {}", part.display()))?;
            std::fs::rename(&part, &path).with_context(|| format!("placing {}", path.display()))?;
        }
        out.push(caribou_plugin::load(&path).map_err(|e| anyhow!("{}: {e}", section.name))?);
    }
    Ok(out)
}

/// FNV-1a over `bytes`.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Build the bundle for `program` from its project and write it to
/// `out`, or beside the program as `<name>.cb`. Returns where it
/// was written.
pub fn write(program: &Path, out: Option<&Path>) -> Result<PathBuf> {
    let roots = project::roots(program);
    let bundle = build(program, &roots)?;
    let out = out.map_or_else(|| program.with_extension(EXTENSION), Path::to_owned);
    std::fs::write(&out, caribou::bundle::emit(&bundle))
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(out)
}
