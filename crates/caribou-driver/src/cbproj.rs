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
//! [haxe]
//! class-paths = ["../engine/haxe"]
//! defines = { editor = true, level = "2" }
//!
//! [plugins]
//! gpu = { path = "../plugins/cb_gpu" }
//! window = { path = "../plugins/cb_window", link = "side" }
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
//! built for this machine for a hosted run, and for a program built ahead
//! of time it is linked in, or with `link = "side"` built as a side module
//! beside the program ([`crate::linked`]); a library is loaded as it is.
//! `[haxe]` takes rayzor.toml's keys: `class-paths` are Haxe libraries
//! beyond the sources, compiled as far as the program reaches, and
//! `defines` are `-D` flags. Paths are relative to the file. What the
//! project builds goes to `target/` beside it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::findings::Findings;

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
    haxe: HaxeSection,
    #[serde(default)]
    plugins: BTreeMap<String, PluginEntry>,
}

/// Haxe's own settings, with rayzor.toml's keys.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct HaxeSection {
    #[serde(default)]
    class_paths: Vec<PathBuf>,
    #[serde(default)]
    defines: BTreeMap<String, toml::Value>,
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
    #[serde(default)]
    link: Link,
}

/// How a program built ahead of time takes a plugin crate.
#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Link {
    /// Into the program's runtime.
    #[default]
    Static,
    /// As a side module beside the program, found when it starts.
    Side,
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
    /// The plugins built as side modules for a program built ahead of time.
    pub side_modules: Vec<PathBuf>,
    /// Haxe class paths beyond the sources: libraries.
    pub haxe_class_paths: Vec<PathBuf>,
    /// `-D` flags, `name` or `name=value`.
    pub haxe_defines: Vec<String>,
    /// The file's `[dependencies]` as it declares them.
    pub declared: toml::Table,
}

impl Project {
    /// The project file at `path`.
    pub fn load(path: &Path) -> Result<Project> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file: File = toml::from_str(&text).with_context(|| path.display().to_string())?;
        let declared = toml::from_str::<toml::Table>(&text)?
            .remove("dependencies")
            .and_then(|d| d.as_table().cloned())
            .unwrap_or_default();
        let path = &normal(&std::path::absolute(path)?);
        let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        let mut haxe_defines = Vec::new();
        for (name, value) in &file.haxe.defines {
            match value {
                toml::Value::Boolean(true) => haxe_defines.push(name.clone()),
                toml::Value::Boolean(false) => {}
                toml::Value::String(s) => haxe_defines.push(format!("{name}={s}")),
                toml::Value::Integer(n) => haxe_defines.push(format!("{name}={n}")),
                toml::Value::Float(n) => haxe_defines.push(format!("{name}={n}")),
                _ => bail!(
                    "{}: define `{name}` takes a string, a number or a boolean",
                    path.display()
                ),
            }
        }
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
            sources: sources.iter().map(|s| normal(&dir.join(s))).collect(),
            haxelibs: file.dependencies.haxe,
            packages: file.dependencies.wren,
            python: file.dependencies.python,
            lua: file.dependencies.lua,
            side_modules: file
                .plugins
                .values()
                .filter(|p| p.link == Link::Side)
                .map(|p| normal(&dir.join(&p.path)))
                .collect(),
            plugins: file
                .plugins
                .into_iter()
                .map(|(name, p)| (name, normal(&dir.join(p.path))))
                .collect(),
            haxe_class_paths: file
                .haxe
                .class_paths
                .iter()
                .map(|c| normal(&dir.join(c)))
                .collect(),
            haxe_defines,
            declared,
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

    /// The program `compile_haxe` writes.
    pub fn haxe_output(&self) -> PathBuf {
        self.target_dir().join(format!("{}.hl", self.name))
    }

    /// The arguments `haxe` runs with, from [`Self::dir`], for the plugin
    /// libraries `plugins`. The sources are relative class paths: caribou's
    /// library describes the relative ones, the project's own, and the
    /// include takes every module in them, since another language may
    /// import any. The `[haxe]` class paths are absolute, as libraries'
    /// are, and compiled as far as the program reaches.
    pub fn haxe_args(&self, plugins: &[PathBuf]) -> Result<Vec<String>> {
        if self.entry.lang != "haxe" {
            bail!(
                "the entry is {}; only a Haxe entry starts a program so far",
                self.entry.lang
            );
        }
        let sources: Vec<String> = self
            .sources
            .iter()
            .map(|s| s.strip_prefix(&self.dir).unwrap_or(s).display().to_string())
            .collect();
        let mut args = Vec::new();
        for source in &sources {
            args.extend(["-cp".to_owned(), source.clone()]);
        }
        for class_path in &self.haxe_class_paths {
            args.extend(["-cp".to_owned(), class_path.display().to_string()]);
        }
        args.extend(["-L".to_owned(), "caribou".to_owned()]);
        // The declared plugins, which the library gives Haxe faces in place
        // of any it would find beside the output.
        if !plugins.is_empty() {
            let list = std::env::join_paths(plugins)?;
            args.extend([
                "-D".to_owned(),
                format!("caribou_plugins={}", list.to_string_lossy()),
            ]);
        }
        for define in &self.haxe_defines {
            args.extend(["-D".to_owned(), define.clone()]);
        }
        for (lib, version) in &self.haxelibs {
            let lib = match version.as_str() {
                "*" | "" => lib.clone(),
                version => format!("{lib}:{version}"),
            };
            args.extend(["-L".to_owned(), lib]);
        }
        let included: Vec<String> = sources.iter().map(|s| format!("'{s}'")).collect();
        args.extend([
            "--macro".to_owned(),
            format!("include('', true, null, [{}])", included.join(", ")),
            "-main".to_owned(),
            self.entry.module.clone(),
            "-hl".to_owned(),
            self.haxe_output().display().to_string(),
        ]);
        Ok(args)
    }

    /// Compile the Haxe entry to HashLink bytecode in the target
    /// directory, with caribou's library and the declared haxelibs, and
    /// every Haxe module in the sources with it.
    pub fn compile_haxe(&self) -> Result<PathBuf> {
        self.compile_haxe_into(None)
    }

    /// [`Self::compile_haxe`], with what `haxe` reports also recorded in
    /// `findings`, each a finding with its position. A failed compile is
    /// then [`HaxeFailed`], its findings already written.
    pub fn compile_haxe_into(&self, findings: Option<&Findings>) -> Result<PathBuf> {
        if let Some(missing) = self.haxe_class_paths.iter().find(|c| !c.is_dir()) {
            bail!(
                "{}: haxe.class-paths names {}, which is not a directory",
                self.path.display(),
                missing.display()
            );
        }
        let plugins = self.plugin_libraries()?;
        let args = self.haxe_args(&plugins)?;
        std::fs::create_dir_all(self.target_dir())?;
        let mut command = Command::new("haxe");
        command.args(&args).current_dir(&self.dir);
        let Some(findings) = findings else {
            let status = command
                .status()
                .context("running the Haxe compiler, `haxe`")?;
            if !status.success() {
                bail!("the Haxe compiler failed on {}", self.entry.module);
            }
            return Ok(self.haxe_output());
        };
        // What haxe says still reaches the console, and its findings the file.
        let output = command
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::piped())
            .output()
            .context("running the Haxe compiler, `haxe`")?;
        let said = String::from_utf8_lossy(&output.stderr);
        eprint!("{said}");
        for finding in crate::findings::haxe_findings(&said, &self.dir) {
            findings.record(&finding);
        }
        if !output.status.success() {
            return Err(HaxeFailed(self.entry.module.clone()).into());
        }
        Ok(self.haxe_output())
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

    /// The project resolved for a tool: every path absolute, and what
    /// caribou runs `haxe` with. Read only: nothing is built or written, and a
    /// plugin crate not built yet is a problem with the library it will be.
    pub fn resolved(&self) -> Resolved {
        let mut problems = Vec::new();
        for source in &self.sources {
            if !source.is_dir() {
                problems.push(Problem::new(
                    "project.sources",
                    format!("{} is not a directory", source.display()),
                ));
            }
        }
        for class_path in &self.haxe_class_paths {
            if !class_path.is_dir() {
                problems.push(Problem::new(
                    "haxe.class-paths",
                    format!("{} is not a directory", class_path.display()),
                ));
            }
        }
        let mut plugins = Vec::new();
        for (name, path) in &self.plugins {
            let key = format!("plugins.{name}");
            let is_crate = crate::linked::is_crate(path);
            let library = if is_crate {
                match crate::linked::host_library_path(path, &self.target_dir()) {
                    Ok(library) => Some(library),
                    Err(e) => {
                        problems.push(Problem::new(&key, format!("{e:#}")));
                        None
                    }
                }
            } else if path.is_file() {
                Some(path.clone())
            } else {
                problems.push(Problem::new(
                    &key,
                    format!("{} is neither a plugin crate nor a library", path.display()),
                ));
                None
            };
            let built = library.as_ref().is_some_and(|l| l.is_file());
            if is_crate && library.is_some() && !built {
                problems.push(Problem::new(
                    &key,
                    "not built yet: `caribou run` builds it".to_owned(),
                ));
            }
            plugins.push(ResolvedPlugin {
                name: name.clone(),
                path: path.clone(),
                kind: if is_crate { "crate" } else { "library" },
                library,
                built,
                link: if self.side_modules.contains(path) {
                    "side"
                } else {
                    "static"
                },
            });
        }
        let libraries: Vec<PathBuf> = plugins.iter().filter_map(|p| p.library.clone()).collect();
        let args = match self.haxe_args(&libraries) {
            Ok(args) => args,
            Err(e) => {
                problems.push(Problem::new("project.entry", format!("{e:#}")));
                Vec::new()
            }
        };
        for (lang, declared) in [("python", &self.python), ("lua", &self.lua)] {
            if !declared.is_empty() {
                problems.push(Problem::new(
                    &format!("dependencies.{lang}"),
                    "found, not installed: Zyntax has no package installer yet".to_owned(),
                ));
            }
        }
        let roots = self
            .sources
            .iter()
            .map(|path| {
                let mut namespaces: Vec<String> = std::fs::read_dir(path)
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| !n.starts_with('.'))
                    .collect();
                namespaces.sort();
                Root {
                    path: path.clone(),
                    namespaces,
                }
            })
            .collect();
        let target = self.target_dir();
        Resolved {
            file: self.path.clone(),
            dir: self.dir.clone(),
            name: self.name.clone(),
            entry: ResolvedEntry {
                language: self.entry.lang.clone(),
                module: self.entry.module.clone(),
            },
            languages: self.languages.clone(),
            roots,
            plugins,
            dependencies: ResolvedDependencies {
                declared: self.declared.clone(),
                manifests: crate::deps::manifests(self),
            },
            outputs: Outputs {
                hl: self.haxe_output(),
                bundle: target.join(format!("{}.cb", self.name)),
                wasm: target.join(format!("{}.wasm", self.name)),
            },
            haxe: HaxeCommand {
                cwd: self.dir.clone(),
                args,
            },
            target,
            problems,
        }
    }

    /// The declared native plugins, loaded.
    pub fn load_plugins(&self) -> Result<Vec<caribou_plugin::Plugin>> {
        self.plugin_libraries()?
            .iter()
            .map(|library| caribou_plugin::load(library).map_err(|e| anyhow!("{e}")))
            .collect()
    }
}

/// The Haxe compiler failed, and what it reported is recorded already.
#[derive(Debug)]
pub struct HaxeFailed(pub String);

impl std::fmt::Display for HaxeFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the Haxe compiler failed on {}", self.0)
    }
}

impl std::error::Error for HaxeFailed {}

/// `path` with its `.` and `..` components taken out, by its text alone:
/// nothing is resolved on disk, so paths compare as they are written.
fn normal(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// A project as [`Project::resolved`] gives it, the JSON `caribou project
/// --json` prints. Paths are absolute but for `haxe.args`, which `haxe`
/// runs with from `haxe.cwd`.
#[derive(Serialize)]
pub struct Resolved {
    pub file: PathBuf,
    pub dir: PathBuf,
    pub name: String,
    pub entry: ResolvedEntry,
    pub languages: Vec<String>,
    pub target: PathBuf,
    pub roots: Vec<Root>,
    pub plugins: Vec<ResolvedPlugin>,
    pub dependencies: ResolvedDependencies,
    pub outputs: Outputs,
    pub haxe: HaxeCommand,
    pub problems: Vec<Problem>,
}

#[derive(Serialize)]
pub struct ResolvedEntry {
    pub language: String,
    pub module: String,
}

/// A source root and the namespaces its directories give.
#[derive(Serialize)]
pub struct Root {
    pub path: PathBuf,
    pub namespaces: Vec<String>,
}

#[derive(Serialize)]
pub struct ResolvedPlugin {
    pub name: String,
    pub path: PathBuf,
    /// `crate`, built for this machine, or `library`, loaded as it is.
    pub kind: &'static str,
    /// The library a hosted run loads, built or not.
    pub library: Option<PathBuf>,
    pub built: bool,
    /// `static` or `side`, for a program built ahead of time.
    pub link: &'static str,
}

#[derive(Serialize)]
pub struct ResolvedDependencies {
    /// The file's `[dependencies]`, as declared.
    pub declared: toml::Table,
    /// Each language's own manifests beside the project or at a root.
    pub manifests: Vec<PathBuf>,
}

/// What `run` and `build` write.
#[derive(Serialize)]
pub struct Outputs {
    pub hl: PathBuf,
    pub bundle: PathBuf,
    pub wasm: PathBuf,
}

/// The `haxe` command `caribou run` compiles with.
#[derive(Serialize)]
pub struct HaxeCommand {
    pub cwd: PathBuf,
    pub args: Vec<String>,
}

/// What in the file caribou rejects, ignores or cannot do yet, under the
/// key it is about.
#[derive(Serialize)]
pub struct Problem {
    pub key: String,
    pub message: String,
}

impl Problem {
    fn new(key: &str, message: String) -> Problem {
        Problem {
            key: key.to_owned(),
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(name: &str, text: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("caribou-cbproj-{name}-{}", std::process::id()));
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
window = { path = "plugins/cb_window", link = "side" }
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
        // Only the plugin declared so is a side module.
        assert_eq!(project.side_modules, [dir.join("plugins/cb_window")]);
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

    #[test]
    fn haxe_runs_with_relative_sources_absolute_libraries_and_the_defines() {
        let (dir, file) = written(
            "args",
            r#"
[project]
name = "args"
entry = "haxe:Main"
languages = ["haxe"]

[haxe]
class-paths = ["../engine/haxe"]
defines = { editor = true, quiet = false, level = "2", count = 3 }

[dependencies.haxe]
heaps = "*"
format = "1.0.0"
"#,
        );
        let project = Project::load(&file).unwrap();
        let args = project.haxe_args(&[]).unwrap();
        let engine = dir.parent().unwrap().join("engine/haxe");
        let out = dir.join("target/args.hl");
        std::fs::remove_dir_all(&dir).ok();
        let expected = [
            "-cp",
            "src",
            "-cp",
            engine.to_str().unwrap(),
            "-L",
            "caribou",
            "-D",
            "count=3",
            "-D",
            "editor",
            "-D",
            "level=2",
            "-L",
            "format:1.0.0",
            "-L",
            "heaps",
            "--macro",
            "include('', true, null, ['src'])",
            "-main",
            "Main",
            "-hl",
            out.to_str().unwrap(),
        ];
        assert_eq!(args, expected);
    }

    #[test]
    fn the_resolved_project_builds_nothing_and_names_an_unbuilt_plugin() {
        let (dir, file) = written(
            "resolved",
            r#"
[project]
name = "resolved"
entry = "haxe:Main"
languages = ["haxe", "python"]

[dependencies.python]
requests = ">=2.31"

[plugins]
tool = { path = "tool" }
"#,
        );
        std::fs::create_dir_all(dir.join("src/game")).unwrap();
        std::fs::create_dir_all(dir.join("tool/src")).unwrap();
        std::fs::write(
            dir.join("tool/Cargo.toml"),
            "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n[lib]\ncrate-type = [\"cdylib\"]\n",
        )
        .unwrap();
        let resolved = Project::load(&file).unwrap().resolved();
        let target_written = dir.join("target").exists();
        std::fs::remove_dir_all(&dir).ok();
        assert!(!target_written, "resolving writes nothing");
        assert_eq!(resolved.file, file);
        assert_eq!(resolved.roots.len(), 1);
        assert_eq!(resolved.roots[0].namespaces, ["game"]);
        let tool = &resolved.plugins[0];
        assert_eq!((tool.kind, tool.built), ("crate", false));
        let library = tool.library.as_ref().expect("the library it will be");
        assert!(library.starts_with(dir.join("target/plugins/release")));
        let keys: Vec<&str> = resolved.problems.iter().map(|p| p.key.as_str()).collect();
        assert!(keys.contains(&"plugins.tool"), "{keys:?}");
        assert!(keys.contains(&"dependencies.python"), "{keys:?}");
        assert_eq!(resolved.haxe.cwd, dir);
        assert_eq!(
            resolved.dependencies.declared["python"]["requests"].as_str(),
            Some(">=2.31")
        );
    }

    #[test]
    fn a_define_takes_a_string_a_number_or_a_boolean() {
        let (dir, file) = written(
            "define",
            "[project]\nname = \"define\"\nentry = \"haxe:Main\"\nlanguages = [\"haxe\"]\n[haxe]\ndefines = { list = [1] }\n",
        );
        let err = Project::load(&file).err().expect("refused").to_string();
        std::fs::remove_dir_all(&dir).ok();
        assert!(err.contains("define `list`"), "{err}");
    }
}
