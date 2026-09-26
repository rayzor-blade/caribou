//! A project file: `<name>.cbproj`, TOML, one per project, declaring what
//! the project is made of instead of the driver inferring it.
//!
//! ```toml
//! [project]
//! name = "game"
//! entry = "haxe:game.Main"
//!
//! [languages.haxe]
//! roots = ["src"]
//! libs = ["heaps"]
//! [languages.wren]
//! roots = ["src"]
//! [languages.python]
//! roots = ["src"]
//! [languages.zynml]
//! grammar = "grammars/zynml.zyn"
//! roots = ["src"]
//!
//! [dependencies]
//! "@hatch:greet" = { path = "../greet" }
//!
//! [plugins]
//! gpu = { path = "plugins/libcaribou_gpu.dylib" }
//! ```
//!
//! The entry is a module of one of the languages, `language:module`.
//! Caribou compiles a Haxe entry itself, with `-lib caribou` and the
//! declared haxelibs. A language is Haxe, Wren, a Zyntax language built
//! into caribou (Python, Lua), or a Zyntax language given by its grammar.
//! `[dependencies]` are Wren's hatch packages, in a hatchfile's form.
//! Paths are relative to the file. What the project builds goes to
//! `target/` beside it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

/// The extension a project file has.
pub const EXTENSION: &str = "cbproj";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    project: Header,
    #[serde(default)]
    languages: BTreeMap<String, Language>,
    #[serde(default)]
    dependencies: BTreeMap<String, caribou_wren::hatch::Dependency>,
    #[serde(default)]
    plugins: BTreeMap<String, PluginEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    name: String,
    entry: String,
}

/// One language of the project: where its modules are, and what it
/// needs of its own.
#[derive(Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Language {
    #[serde(default)]
    pub roots: Vec<PathBuf>,
    /// Haxe's haxelibs, beside caribou's own.
    #[serde(default)]
    pub libs: Vec<String>,
    /// A Zyntax language's grammar, for one caribou does not build in.
    pub grammar: Option<PathBuf>,
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
    pub languages: BTreeMap<String, Language>,
    pub dependencies: BTreeMap<String, caribou_wren::hatch::Dependency>,
    /// Each native plugin, by name, at its library's path.
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
        let (lang, module) = file.project.entry.split_once(':').ok_or_else(|| {
            anyhow!(
                "{}: entry `{}` is not `language:module`",
                path.display(),
                file.project.entry
            )
        })?;
        if !file.languages.contains_key(lang) {
            bail!(
                "{}: the entry is {lang}, which [languages] does not declare",
                path.display()
            );
        }
        let languages = file
            .languages
            .into_iter()
            .map(|(name, mut language)| {
                language.roots = language.roots.iter().map(|r| dir.join(r)).collect();
                language.grammar = language.grammar.map(|g| dir.join(g));
                (name, language)
            })
            .collect();
        Ok(Project {
            path: path.to_path_buf(),
            name: file.project.name,
            entry: Entry {
                lang: lang.to_owned(),
                module: module.to_owned(),
            },
            languages,
            dependencies: file.dependencies,
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

    /// Every language's roots, each once, in declaration order.
    pub fn roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        for language in self.languages.values() {
            for root in &language.roots {
                if !roots.contains(root) {
                    roots.push(root.clone());
                }
            }
        }
        roots
    }

    /// Compile the Haxe entry to HashLink bytecode in the target
    /// directory, with caribou's library and the declared haxelibs, and
    /// every module under the Haxe roots with it.
    pub fn compile_haxe(&self) -> Result<PathBuf> {
        if self.entry.lang != "haxe" {
            bail!(
                "the entry is {}; only a Haxe entry starts a program so far",
                self.entry.lang
            );
        }
        let haxe = self.languages.get("haxe").cloned().unwrap_or_default();
        let out = self.target_dir().join(format!("{}.hl", self.name));
        std::fs::create_dir_all(self.target_dir())?;
        let mut command = Command::new("haxe");
        for root in &haxe.roots {
            command.arg("-cp").arg(root);
        }
        command.args(["-lib", "caribou"]);
        for lib in &haxe.libs {
            command.args(["-lib", lib]);
        }
        // Every module under the roots, not only what the entry reaches:
        // another language may import any of them.
        let roots: Vec<String> = haxe
            .roots
            .iter()
            .map(|r| format!("'{}'", r.display()))
            .collect();
        command
            .arg("--macro")
            .arg(format!("include('', true, null, [{}])", roots.join(", ")));
        command
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

    /// The declared Zyntax languages, as frontends: one caribou builds
    /// in by name, any other by its grammar.
    pub fn frontends(&self) -> Result<Vec<caribou_zyntax::Frontend>> {
        let mut out = Vec::new();
        for (name, language) in &self.languages {
            if name == "haxe" || name == "wren" {
                continue;
            }
            let frontend = match &language.grammar {
                Some(grammar) => {
                    caribou_zyntax::Frontend::file(grammar).map_err(|e| anyhow!(e))?
                }
                None => crate::project::builtin(name).ok_or_else(|| {
                    anyhow!(
                        "{}: language `{name}` is not built into caribou; give its `grammar`",
                        self.path.display()
                    )
                })?,
            };
            out.push(frontend);
        }
        Ok(out)
    }

    /// The declared native plugins, loaded.
    pub fn load_plugins(&self) -> Result<Vec<caribou_plugin::Plugin>> {
        self.plugins
            .values()
            .map(|path| caribou_plugin::load(path).map_err(|e| anyhow!("{e}")))
            .collect()
    }

    /// The declared hatch packages, resolved, and what they depend on.
    pub fn packages(&self) -> Result<Vec<caribou_wren::hatch::Package>> {
        caribou_wren::hatch::declared(&self.dir, &self.dependencies).map_err(|e| anyhow!(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_file_declares_its_languages_entry_and_dependencies() {
        let dir = std::env::temp_dir().join(format!("caribou-cbproj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("game.cbproj");
        std::fs::write(
            &file,
            r#"
[project]
name = "game"
entry = "haxe:game.Main"

[languages.haxe]
roots = ["src"]
libs = ["heaps"]
[languages.wren]
roots = ["src", "scripts"]

[dependencies]
"@hatch:greet" = { path = "../greet" }

[plugins]
gpu = { path = "plugins/libcaribou_gpu.dylib" }
"#,
        )
        .unwrap();
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
        assert_eq!(project.roots(), [dir.join("src"), dir.join("scripts")]);
        assert_eq!(project.languages["haxe"].libs, ["heaps"]);
        assert!(project.dependencies.contains_key("@hatch:greet"));
        assert_eq!(
            project.plugins["gpu"],
            dir.join("plugins/libcaribou_gpu.dylib")
        );
    }

    #[test]
    fn an_entry_must_name_a_declared_language() {
        let dir = std::env::temp_dir().join(format!("caribou-cbproj-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.cbproj");
        std::fs::write(&file, "[project]\nname = \"x\"\nentry = \"lua:main\"\n").unwrap();
        let err = Project::load(&file).err().expect("refused").to_string();
        std::fs::remove_dir_all(&dir).ok();
        assert!(err.contains("does not declare"), "{err}");
    }
}
