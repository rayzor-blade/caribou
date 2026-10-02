//! A project's layout: where its modules are and what namespaces it has.
//!
//! The source roots are the class paths of the `.hxml` that built the
//! program, the one whose `-hl` names it, found in the directory the
//! command runs in, the program's own or the one above it (a program in
//! `bin/`). Without one, the class paths of every `.hxml` in the first
//! two; without any, `src` under those directories when it exists, else
//! the directories themselves. A namespace is a directory under a root
//! (`src/game` is `game`), or one the program imports, and every
//! namespace covers every resident language, Haxe first, the plugins
//! beside the program last.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use caribou::registry::Namespace;

/// The languages a project's namespaces cover, in lookup order, before
/// the plugins beside the program.
pub const LANGUAGES: [&str; 2] = ["haxe", "wren"];

/// The class paths an `.hxml` declares, relative to its directory.
fn class_paths(hxml: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(hxml) else {
        return Vec::new();
    };
    let dir = hxml.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    let mut words = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .flat_map(|l| l.split_whitespace());
    while let Some(word) = words.next() {
        if matches!(word, "-cp" | "-p" | "--class-path")
            && let Some(path) = words.next()
        {
            out.push(dir.join(path));
        }
    }
    out
}

/// The program an `.hxml` writes, its `-hl`, relative to its directory.
fn output(hxml: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(hxml).ok()?;
    let dir = hxml.parent().unwrap_or(Path::new("."));
    let mut words = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .flat_map(|l| l.split_whitespace());
    while let Some(word) = words.next() {
        if word == "-hl" {
            return words.next().map(|path| dir.join(path));
        }
    }
    None
}

/// The `.hxml` files in `dir`, in name order.
fn hxmls_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut hxmls: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "hxml"))
        .collect();
    hxmls.sort();
    hxmls
}

/// The source roots for `program`: see the module doc.
pub fn roots(program: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = vec![PathBuf::from(".")];
    let own = program.parent().filter(|d| !d.as_os_str().is_empty());
    if let Some(dir) = own {
        dirs.push(dir.to_owned());
    }
    // The `.hxml` that built the program decides, wherever it is.
    let above = own
        .and_then(Path::parent)
        .filter(|d| !d.as_os_str().is_empty());
    let built = std::fs::canonicalize(program).ok();
    let mut roots: Vec<PathBuf> = dirs
        .iter()
        .map(PathBuf::as_path)
        .chain(above)
        .flat_map(hxmls_in)
        .filter(|hxml| {
            built.is_some() && output(hxml).and_then(|out| std::fs::canonicalize(out).ok()) == built
        })
        .flat_map(|hxml| class_paths(&hxml))
        .collect();
    if roots.is_empty() {
        for dir in &dirs {
            for hxml in hxmls_in(dir) {
                roots.extend(class_paths(&hxml));
            }
        }
    }
    if roots.is_empty() {
        for dir in &dirs {
            let src = dir.join("src");
            roots.push(if src.is_dir() { src } else { dir.clone() });
        }
    }
    roots.retain(|r| r.is_dir());
    roots.dedup();
    roots
}

/// The frontends built into caribou whose files a root holds: Python for
/// a `.py` file, Lua for a `.lua` file. Each parses on its own, so no
/// file under a root names it.
pub fn builtins(roots: &[impl AsRef<Path>]) -> Vec<caribou_zyntax::Frontend> {
    let mut out = Vec::new();
    if caribou_python::Python::present_in(roots) {
        out.extend(builtin("python"));
    }
    if caribou_lua::Lua::present_in(roots) {
        out.extend(builtin("lua"));
    }
    out
}

/// The frontend this build of caribou has in it under `lang`, the one a
/// bundle's `builtin` language section asks for.
pub fn builtin(lang: &str) -> Option<caribou_zyntax::Frontend> {
    match lang {
        "python" => Some(caribou_zyntax::Frontend::new(Box::new(
            caribou_python::Python::new(),
        ))),
        "lua" => Some(caribou_zyntax::Frontend::new(Box::new(
            caribou_lua::Lua::new(),
        ))),
        _ => None,
    }
}

/// The Zyntax frontends of a project: the frontend files under `roots`,
/// each with `plugin_dir` for its `.zrtl` plugins, and the languages
/// that parse on their own when a root holds their files.
pub fn frontends(
    roots: &[PathBuf],
    plugin_dir: &Path,
) -> anyhow::Result<Vec<caribou_zyntax::Frontend>> {
    let mut frontends = caribou_zyntax::Frontend::files_in(roots)
        .iter()
        .map(|file| {
            caribou_zyntax::Frontend::file(file)
                .map(|f| f.with_plugin_dir(plugin_dir.to_owned()))
                .map_err(|e| anyhow::anyhow!(e))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    frontends.extend(builtins(roots));
    Ok(frontends)
}

/// The namespaces of a project: each directory under a root, and each
/// name in `imported`, every one over every resident language, the
/// languages named in `others` (Zyntax frontends) after the runtimes. A
/// name in `plugins` is that plugin's own namespace, which resolves to its
/// language alone, so it is left out.
pub fn namespaces(
    roots: &[PathBuf],
    imported: &[String],
    others: &[String],
    plugins: &[String],
) -> Vec<Namespace> {
    let mut names: BTreeSet<String> = imported
        .iter()
        .filter(|n| !plugins.contains(n))
        .cloned()
        .collect();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && path.is_dir()
                && !name.starts_with('.')
            {
                names.insert(name.to_owned());
            }
        }
    }
    names
        .into_iter()
        .map(|name| Namespace {
            name,
            langs: LANGUAGES
                .iter()
                .map(|l| (*l).to_owned())
                .chain(others.iter().cloned())
                .collect(),
            modules: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_paths_come_from_the_hxml_and_namespaces_from_the_directories() {
        let dir = std::env::temp_dir().join(format!("caribou-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/game")).unwrap();
        std::fs::create_dir_all(dir.join("lib/ui")).unwrap();
        std::fs::write(
            dir.join("build.hxml"),
            "# the build\n-cp src\n-cp lib\n-main Main\n",
        )
        .unwrap();
        std::fs::write(dir.join("game.hl"), "").unwrap();

        let found = roots(&dir.join("game.hl"));
        assert_eq!(found, vec![dir.join("src"), dir.join("lib")]);
        let namespaces = namespaces(&found, &["net".to_owned()], &["gfx".to_owned()], &[]);
        let names: Vec<&str> = namespaces.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["game", "net", "ui"]);
        assert_eq!(namespaces[0].langs, ["haxe", "wren", "gfx"]);

        // Without an hxml: `src` under the program's directory when there
        // is one, beside whatever the working directory gives.
        std::fs::remove_file(dir.join("build.hxml")).unwrap();
        let found = roots(&dir.join("game.hl"));
        assert!(found.contains(&dir.join("src")), "{found:?}");
        assert!(!found.contains(&dir), "{found:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_hxml_that_built_a_program_in_bin_gives_its_roots() {
        let dir = std::env::temp_dir().join(format!("caribou-project-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("gpu.hxml"),
            "-cp src\n-main Main\n-hl bin/gpu.hl\n",
        )
        .unwrap();
        std::fs::write(dir.join("bin/gpu.hl"), "").unwrap();

        assert_eq!(roots(&dir.join("bin/gpu.hl")), vec![dir.join("src")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
