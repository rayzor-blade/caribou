//! Building a bundle from a project: the program, every Wren module
//! under the project's source roots, compiled, every Zyntax frontend
//! under them with the modules of its language, and the plugins beside
//! the program. The namespaces are the project's
//! (`project::namespaces`), so a program run from the bundle sees what
//! it saw from the directory.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use caribou::bundle::{Bundle, Entry, Manifest, Section, SectionKind};
use caribou_zyntax::Frontend;

use crate::project;

/// The form a Zyntax snapshot language section comes in.
const SNAPSHOT: &str = "zsnap";
/// The form of a language section for a frontend built into caribou.
const BUILTIN: &str = "builtin";
/// The extension of a Zyntax runtime plugin.
const ZRTL: &str = "zrtl";

/// The extension a bundle is written with.
pub const EXTENSION: &str = "cb";

/// The bundle for the program at `program`, with the modules under
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
    let plugin_dir = program
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("plugins");
    let plugins = caribou_plugin::load_dir(&plugin_dir).map_err(|e| anyhow!("{e}"))?;
    // The Zyntax frontends under the roots, each as the snapshot a run
    // brings it up from, or as a name when caribou has it in.
    let mut frontends: Vec<(Frontend, Section)> = Vec::new();
    for file in Frontend::files_in(roots) {
        let bytes = Frontend::snapshot_bytes(&file).map_err(|e| anyhow!(e))?;
        let frontend = Frontend::snapshot(&bytes).map_err(|e| anyhow!("{}: {e}", file.display()))?;
        let section = Section {
            kind: SectionKind::Language,
            lang: frontend.name().to_owned(),
            format: SNAPSHOT.to_owned(),
            name: file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            data: bytes,
        };
        frontends.push((frontend, section));
    }
    for frontend in project::python(roots) {
        let section = Section {
            kind: SectionKind::Language,
            lang: frontend.name().to_owned(),
            format: BUILTIN.to_owned(),
            name: String::new(),
            data: Vec::new(),
        };
        frontends.push((frontend, section));
    }
    let others: Vec<String> = plugins
        .iter()
        .map(|p| p.name().to_owned())
        .chain(frontends.iter().map(|(f, _)| f.name().to_owned()))
        .collect();
    let namespaces = project::namespaces(roots, &imported, &others);
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
    // Each frontend, then the modules of its language under the roots,
    // as source: the frontend parses them as it would from a root.
    for (frontend, section) in &frontends {
        sections.push(section.clone());
        let mut staged: Vec<String> = Vec::new();
        for root in roots {
            for (module, path) in frontend.modules_in(root) {
                if staged.contains(&module) {
                    continue;
                }
                sections.push(Section {
                    kind: SectionKind::Module,
                    lang: frontend.name().to_owned(),
                    format: caribou_zyntax::SOURCE.to_owned(),
                    name: module.clone(),
                    data: std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
                });
                staged.push(module);
            }
        }
    }
    let mut libs: Vec<(String, PathBuf)> = plugins
        .iter()
        .map(|p| (String::new(), p.path().to_owned()))
        .collect();
    libs.extend(zrtl_plugins(&plugin_dir)?.into_iter().map(|p| ("zyntax".to_owned(), p)));
    for (lang, path) in libs {
        sections.push(Section {
            kind: SectionKind::NativeLib,
            lang,
            format: caribou::bundle::target(),
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| anyhow!("{} has no file name", path.display()))?
                .to_owned(),
            data: std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
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

/// The `.zrtl` plugins in `dir`, in name order.
fn zrtl_plugins(dir: &Path) -> Result<Vec<PathBuf>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == ZRTL))
        .collect();
    out.sort();
    Ok(out)
}

/// The bundle's native libraries for this target, as a directory: a
/// library loads from a file, so they are written under the temporary
/// directory, in one directory per set of contents, once; a directory
/// already there is used as it is. `caribou_plugin::load_dir` reads the
/// plugins from it, and a Zyntax frontend its `.zrtl` plugins.
pub fn native_libs(bundle: &Bundle) -> Result<PathBuf> {
    let target = caribou::bundle::target();
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for section in bundle.native_libs(&target) {
        hash = fnv(hash, section.name.as_bytes());
        hash = fnv(hash, &section.data);
    }
    let dir = std::env::temp_dir()
        .join("caribou-plugins")
        .join(format!("{hash:016x}"));
    for section in bundle.native_libs(&target) {
        let path = dir.join(&section.name);
        if path.is_file() {
            continue;
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
        // Written whole before it has its name, so a reader never sees
        // a partial library.
        let part = dir.join(format!("{}.{}", section.name, std::process::id()));
        std::fs::write(&part, &section.data).with_context(|| format!("writing {}", part.display()))?;
        std::fs::rename(&part, &path).with_context(|| format!("placing {}", path.display()))?;
    }
    Ok(dir)
}

/// The frontends a bundle's language sections name, brought up from the
/// snapshot each carries or from this build of caribou, with
/// `plugin_dir` for their `.zrtl` plugins.
pub fn frontends(bundle: &Bundle, plugin_dir: &Path) -> Result<Vec<Frontend>> {
    let mut out = Vec::new();
    for section in bundle.languages() {
        let frontend = match section.format.as_str() {
            SNAPSHOT => Frontend::snapshot(&section.data)
                .map_err(|e| anyhow!("the language {} does not load: {e}", section.lang))?,
            BUILTIN => project::builtin(&section.lang).ok_or_else(|| {
                anyhow!(
                    "the bundle needs the language {} built into caribou, and this build has no such language",
                    section.lang
                )
            })?,
            other => {
                return Err(anyhow!(
                    "the language {} comes as `{other}`, which this build does not read",
                    section.lang
                ));
            }
        };
        out.push(frontend.with_plugin_dir(plugin_dir.to_owned()));
    }
    Ok(out)
}

/// FNV-1a over `bytes`, continued from `hash`.
fn fnv(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |h, &b| {
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
