//! A project's dependencies, from what its file declares and from each
//! language's own manifest, declared or not: Wren's `hatchfile` beside
//! the project file or at a source root, Python's `requirements.txt` and
//! Lua's `*.rockspec` beside the project file.
//!
//! Wren's resolve as hatch resolves them. Python's and Lua's are found
//! and reported, not installed: Zyntax has no machinery for their
//! ecosystems' packages yet.

use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::cbproj::Project;

/// Wren's hatch packages: the declared ones, then those of a `hatchfile`
/// beside the project file or at a source root, each once, with what
/// they depend on.
pub fn hatch(project: &Project) -> Result<Vec<caribou_wren::hatch::Package>> {
    let mut packages =
        caribou_wren::hatch::declared(&project.dir, &project.packages).map_err(|e| anyhow!(e))?;
    let mut dirs = vec![project.dir.clone()];
    dirs.extend(project.sources.iter().cloned());
    for package in caribou_wren::hatch::dependencies(&dirs).map_err(|e| anyhow!(e))? {
        if !packages.iter().any(|p| p.name == package.name) {
            packages.push(package);
        }
    }
    Ok(packages)
}

/// Python's and Lua's dependencies the project has, declared or in
/// their manifests, which nothing installs yet: what a run says it
/// leaves out.
pub fn unresolved(project: &Project) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let requirements = project.dir.join("requirements.txt");
    if requirements.is_file() {
        out.push(requirements.display().to_string());
    }
    let mut rockspecs: Vec<PathBuf> = std::fs::read_dir(&project.dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "rockspec"))
        .collect();
    rockspecs.sort();
    out.extend(rockspecs.iter().map(|r| r.display().to_string()));
    out.extend(
        project
            .python
            .keys()
            .map(|name| format!("python package `{name}`")),
    );
    out.extend(project.lua.keys().map(|name| format!("lua rock `{name}`")));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str, text: &str) -> (PathBuf, Project) {
        let dir = std::env::temp_dir().join(format!("caribou-deps-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{name}.cbproj"));
        std::fs::write(&file, text).unwrap();
        let project = Project::load(&file).unwrap();
        (dir, project)
    }

    #[test]
    fn a_hatchfile_at_a_source_root_counts_undeclared() {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../caribou-interop/fixtures/hatch/src")
            .canonicalize()
            .unwrap();
        let (dir, project) = project(
            "hatch",
            &format!(
                "[project]\nname = \"g\"\nentry = \"haxe:Main\"\nlanguages = [\"haxe\", \"wren\"]\nsources = [{:?}]\n",
                src.display().to_string()
            ),
        );
        let packages = hatch(&project).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["@hatch:greet"]);
    }

    #[test]
    fn python_and_lua_dependencies_are_found_and_reported() {
        let (dir, project) = project(
            "zy",
            "[project]\nname = \"z\"\nentry = \"haxe:Main\"\nlanguages = [\"haxe\", \"python\", \"lua\"]\n\n[dependencies.python]\nrequests = \">=2\"\n",
        );
        std::fs::write(dir.join("requirements.txt"), "numpy\n").unwrap();
        std::fs::write(dir.join("game-1.0-1.rockspec"), "dependencies = {}\n").unwrap();
        let found = unresolved(&project).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].ends_with("requirements.txt"));
        assert!(found[1].ends_with("game-1.0-1.rockspec"));
        assert_eq!(found[2], "python package `requests`");
    }
}
