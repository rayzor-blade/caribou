//! A project file: `<name>.cbproj`, TOML, one per project, declaring what
//! the project is: its entry, the languages it is written in, and what
//! each depends on. Its modules are not listed: they are found under the
//! sources, `src/` by convention, as the runtime finds them, and a file's
//! path is its name (`src/game/ui/hud.wren` is `game:ui/hud`, the first
//! directory its namespace).
//!
//! ```toml
//! [project]
//! name = "game"
//! entry = "haxe:game.Main"
//! languages = ["haxe", "wren", "python", "zynml"]
//!
//! [dependencies.haxe]
//! heaps = "*"
//! [dependencies.wren]
//! "@hatch:greet" = { path = "../greet" }
//! [dependencies.python]
//! requests = ">=2.31"
//! [dependencies.lua]
//! penlight = "1.13.1"
//!
//! [plugins]
//! gpu = { path = "../plugins/cb_gpu" }
//! window = { path = "plugins/libcaribou_window.dylib" }
//! ```
//!
//! The entry is a module of one of the languages, `language:module`.
//! Caribou compiles a Haxe entry itself, with `-lib caribou` and the
//! declared haxelibs. A language is Haxe, Wren, a Zyntax language built
//! into caribou (Python, Lua), or a Zyntax language whose grammar is a
//! frontend file in the sources. Wren's dependencies are hatch packages,
//! in a hatchfile's form. Each language's own manifest counts as well,
//! declared or not: a `hatchfile` beside the project file or at a source
//! root, a `requirements.txt` and `*.rockspec` files beside the project
//! file ([`crate::deps`]); Python's and Lua's are found but not yet
//! installed. A plugin is its crate or its built library: a crate is
//! built for this machine for a hosted run, and linked into a program
//! built ahead of time ([`crate::linked`]); a library is loaded as it is.
//! Paths are relative to the file. What the project builds goes to
//! `target/` beside it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

/// The extension a project file has.
pub const EXTENSION: &str = "cbproj";

/// Where modules are found when the file names no sources.
const SOURCES: &str = "src";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    project: Header,
    #[serde(default)]
    dependencies: Dependencies,
    #[serde(default)]
    plugins: BTreeMap<String, PluginEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    name: String,
    entry: String,
    languages: Vec<String>,
    #[serde(default)]
    sources: Vec<PathBuf>,
}

/// Each language's dependencies, in that language's own form.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Dependencies {
    /// Haxelibs, by name, at a version or `*`.
    #[serde(default)]
    haxe: BTreeMap<String, String>,
    #[serde(default)]
    wren: BTreeMap<String, caribou_wren::hatch::Dependency>,
    /// Python packages, by name, at a version specifier or `*`.
    #[serde(default)]
    python: BTreeMap<String, String>,
    /// Lua rocks, by name, at a version or `*`.
    #[serde(default)]
    lua: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginEntry {
    path: PathBuf,
}

/// The module a project starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub lang: String,
    pub module: String,
}

/// A project, as its file declares it, with every path resolved against
/// the file's directory.
pub struct Project {
    pub path: PathBuf,
    pub dir: PathBuf,
    pub name: String,
    pub entry: Entry,
    pub languages: Vec<String>,
    /// Where the project's modules are.
    pub sources: Vec<PathBuf>,
    /// Haxelibs, by name, at a version or `*`.
    pub haxelibs: BTreeMap<String, String>,
    /// Wren's hatch packages the file declares.
    pub packages: BTreeMap<String, caribou_wren::hatch::Dependency>,
    /// Python packages the file declares, by name, at a specifier or `*`.
    pub python: BTreeMap<String, String>,
    /// Lua rocks the file declares, by name, at a version or `*`.
    pub lua: BTreeMap<String, String>,
    /// Each native plugin, by name, at its crate's or its library's path.
    pub plugins: BTreeMap<String, PathBuf>,
}

impl Project {
    /// The project file at `path`.
    pub fn load(path: &Path) -> Result<Project> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file: File = toml::from_str(&text).with_context(|| path.display().to_string())?;
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let header = file.project;
        let (lang, module) = header.entry.split_once(':').ok_or_else(|| {
            anyhow!(
                "{}: entry `{}` is not `language:module`",
                path.display(),
                header.entry
            )
        })?;
        if !header.languages.iter().any(|l| l == lang) {
            bail!(
                "{}: the entry is {lang}, which `languages` does not list",
                path.display()
            );
        }
        let sources = if header.sources.is_empty() {
            vec![PathBuf::from(SOURCES)]
        } else {
            header.sources
        };
        Ok(Project {
            path: path.to_path_buf(),
            name: header.name,
            entry: Entry {
                lang: lang.to_owned(),
                module: module.to_owned(),
            },
            languages: header.languages,
            sources: sources.iter().map(|s| dir.join(s)).collect(),
            haxelibs: file.dependencies.haxe,
            packages: file.dependencies.wren,
            python: file.dependencies.python,
            lua: file.dependencies.lua,
            plugins: file
                .plugins
                .into_iter()
                .map(|(name, p)| (name, dir.join(p.path)))
                .collect(),
            dir,
        })
    }

    /// The one project file in `dir`, if there is one.
    pub fn find(dir: &Path) -> Result<Option<Project>> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == EXTENSION) && p.is_file())
            .collect();
        match files.len() {
            0 => Ok(None),
            1 => Project::load(&files.remove(0)).map(Some),
            _ => {
                files.sort();
                let names: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
                bail!(
                    "{} holds more than one project: {}; name one",
                    dir.display(),
                    names.join(", ")
                )
            }
        }
    }

    /// Where the project's builds go.
    pub fn target_dir(&self) -> PathBuf {
        self.dir.join("target")
    }

    /// Compile the Haxe entry to HashLink bytecode in the target
    /// directory, with caribou's library and the declared haxelibs, and
    /// every Haxe module in the sources with it.
    pub fn compile_haxe(&self) -> Result<PathBuf> {
        if self.entry.lang != "haxe" {
            bail!(
                "the entry is {}; only a Haxe entry starts a program so far",
                self.entry.lang
            );
        }
        let out = self.target_dir().join(format!("{}.hl", self.name));
        std::fs::create_dir_all(self.target_dir())?;
        let mut command = Command::new("haxe");
        // Relative to the project, where the compiler runs: the library
        // takes an absolute class path for the standard library's.
        let sources: Vec<&Path> = self
            .sources
            .iter()
            .map(|s| s.strip_prefix(&self.dir).unwrap_or(s))
            .collect();
        for source in &sources {
            command.arg("-cp").arg(source);
        }
        command.args(["-lib", "caribou"]);
        // The declared plugins, which the library gives Haxe faces in place
        // of any it would find beside the output.
        let plugins = self.plugin_libraries()?;
        if !plugins.is_empty() {
            let list = std::env::join_paths(&plugins)?;
            command
                .arg("-D")
                .arg(format!("caribou_plugins={}", list.to_string_lossy()));
        }
        for (lib, version) in &self.haxelibs {
            match version.as_str() {
                "*" | "" => command.args(["-lib", lib]),
                version => command.args(["-lib", &format!("{lib}:{version}")]),
            };
        }
        // Every module in the sources, not only what the entry reaches:
        // another language may import any of them.
        let sources: Vec<String> = sources
            .iter()
            .map(|s| format!("'{}'", s.display()))
            .collect();
        command
            .arg("--macro")
            .arg(format!("include('', true, null, [{}])", sources.join(", ")))
            .args(["-main", &self.entry.module, "-hl"])
            .arg(&out)
            .current_dir(&self.dir);
        let status = command
            .status()
            .context("running the Haxe compiler, `haxe`")?;
        if !status.success() {
            bail!("the Haxe compiler failed on {}", self.entry.module);
        }
        Ok(out)
    }

    /// The listed Zyntax languages, as frontends: one caribou builds in by
    /// name, any other by its frontend file in the sources.
    pub fn frontends(&self) -> Result<Vec<caribou_zyntax::Frontend>> {
        let mut found = Vec::new();
        for file in caribou_zyntax::Frontend::files_in(&self.sources) {
            found.push(caribou_zyntax::Frontend::file(&file).map_err(|e| anyhow!(e))?);
        }
        let mut out = Vec::new();
        for lang in &self.languages {
            if lang == "haxe" || lang == "wren" {
                continue;
            }
            let frontend = match found.iter().position(|f| f.name() == lang) {
                Some(at) => found.remove(at),
                None => crate::project::builtin(lang).ok_or_else(|| {
                    anyhow!(
                        "{}: language `{lang}` is neither built into caribou nor a \
                         frontend file in the sources",
                        self.path.display()
                    )
                })?,
            };
            out.push(frontend);
        }
        Ok(out)
    }

    /// The declared native plugins' libraries for this machine: a library
    /// as it is, a crate built first.
    pub fn plugin_libraries(&self) -> Result<Vec<PathBuf>> {
        self.plugins
            .values()
            .map(|path| {
                if crate::linked::is_crate(path) {
                    crate::linked::host_library(path, &self.target_dir())
                } else {
                    Ok(path.clone())
                }
            })
            .collect()
    }

    /// The declared native plugins, loaded.
    pub fn load_plugins(&self) -> Result<Vec<caribou_plugin::Plugin>> {
        self.plugin_libraries()?
            .iter()
            .map(|library| caribou_plugin::load(library).map_err(|e| anyhow!("{e}")))
            .collect()
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(name: &str, text: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("caribou-cbproj-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{name}.cbproj"));
        std::fs::write(&file, text).unwrap();
        (dir, file)
    }

    #[test]
    fn a_project_file_declares_its_entry_languages_and_dependencies() {
        let (dir, _) = written(
            "game",
            r#"
[project]
name = "game"
entry = "haxe:game.Main"
languages = ["haxe", "wren"]

[dependencies.haxe]
heaps = "*"
[dependencies.wren]
"@hatch:greet" = { path = "../greet" }

[plugins]
gpu = { path = "plugins/libcaribou_gpu.dylib" }
"#,
        );
        let project = Project::find(&dir).unwrap().expect("found");
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(project.name, "game");
        assert_eq!(
            project.entry,
            Entry {
                lang: "haxe".into(),
                module: "game.Main".into()
            }
        );
        assert_eq!(project.languages, ["haxe", "wren"]);
        // Modules are found under `src` when the file names no sources.
        assert_eq!(project.sources, [dir.join("src")]);
        assert_eq!(project.haxelibs["heaps"], "*");
        assert!(project.packages.contains_key("@hatch:greet"));
        assert_eq!(
            project.plugins["gpu"],
            dir.join("plugins/libcaribou_gpu.dylib")
        );
    }

    #[test]
    fn an_entry_must_be_in_a_listed_language() {
        let (dir, file) = written(
            "x",
            "[project]\nname = \"x\"\nentry = \"lua:main\"\nlanguages = [\"haxe\"]\n",
        );
        let err = Project::load(&file).err().expect("refused").to_string();
        std::fs::remove_dir_all(&dir).ok();
        assert!(err.contains("does not list"), "{err}");
    }
}
