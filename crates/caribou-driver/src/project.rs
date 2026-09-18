//! A project's layout: where its modules are and what namespaces it has.
//!
//! The source roots are the class paths of the project's `.hxml` files,
//! read from the directory the command runs in and from the program's
//! own; without any, `src` under those directories when it exists, else
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

/// The source roots for `program`: see the module doc.
pub fn roots(program: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = vec![PathBuf::from(".")];
    if let Some(dir) = program.parent().filter(|d| !d.as_os_str().is_empty()) {
        dirs.push(dir.to_owned());
    }
    let mut roots = Vec::new();
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut hxmls: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "hxml"))
            .collect();
        hxmls.sort();
        for hxml in hxmls {
            roots.extend(class_paths(&hxml));
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

/// The namespaces of a project: each directory under a root, and each
/// name in `imported`, every one over every resident language, the
/// languages named in `others` (plugins, Zyntax frontends) after the
/// runtimes.
pub fn namespaces(roots: &[PathBuf], imported: &[String], others: &[String]) -> Vec<Namespace> {
    let mut names: BTreeSet<String> = imported.iter().cloned().collect();
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
        let namespaces = namespaces(&found, &["net".to_owned()], &["gfx".to_owned()]);
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
}
