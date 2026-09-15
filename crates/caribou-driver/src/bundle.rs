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
    let namespaces = project::namespaces(roots, &imported);
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
fn wren_modules(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
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
