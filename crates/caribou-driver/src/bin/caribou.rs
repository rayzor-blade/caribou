//! The caribou command.
//!
//!     caribou run [--mode interp|hybrid] [--wren interpreter|tiered] [--report] [<project.cbproj | program>] [args...]
//!     caribou build [--target <triple>] [<project.cbproj | program.hl>] [-o <out>]
//!     caribou describe <module.wren | plugin library | root directory>...
//!
//! With no program named, both act on the project file in the current
//! directory (`cbproj`): its Haxe entry compiled, its declared languages,
//! plugins and packages; what they build goes to its `target/`.
//!
//! `run` runs a program with every resident language, from the project
//! directory: the other languages' modules are found under the project's
//! class paths and load on first use. The program is a `.hl`, or a bundle
//! `build` wrote from one: the program and every module under the class
//! paths in one file, run the same way anywhere. `--report` prints, when
//! the program ends, what the run did: the tier each function reached
//! and how each send across the bridge went. `build --target
//! wasm32-wasip1` builds the program ahead of time instead: a wasm module
//! on caribou's runtime, with nothing interpreted. `describe` prints the
//! modules' interfaces as JSON, for a build step; a plugin library's
//! classes come one module each.

use std::path::{Path, PathBuf};
use std::process;

use caribou_ash::Mode;
use caribou_driver::cbproj::{self, Project};
use wren_lift::runtime::engine::ExecutionMode;

const USAGE: &str = "usage: caribou run [--mode interp|hybrid] [--wren interpreter|tiered] [--report] [<project.cbproj | program>] [args...]\n       caribou build [--target <triple>] [<project.cbproj | program.hl>] [-o <out>]\n       caribou describe <module.wren | plugin library | root directory>...";

fn run(argv: &mut impl Iterator<Item = String>) -> Result<(), String> {
    let mut options = caribou_driver::Options::default();
    let mut program = None;
    let mut args = Vec::new();
    while let Some(arg) = argv.next() {
        if program.is_some() {
            args.push(arg);
            continue;
        }
        match arg.as_str() {
            "--mode" => {
                options.mode = match argv.next().as_deref() {
                    Some("interp") => Mode::Interp,
                    Some("hybrid") => Mode::Hybrid,
                    other => return Err(format!("--mode takes interp or hybrid, not {other:?}")),
                };
            }
            "--wren" => {
                options.wren_mode = match argv.next().as_deref() {
                    Some("interpreter") => ExecutionMode::Interpreter,
                    Some("tiered") => ExecutionMode::Tiered,
                    other => {
                        return Err(format!("--wren takes interpreter or tiered, not {other:?}"));
                    }
                };
            }
            "--report" => options.report = true,
            _ if arg.starts_with("--") => return Err(format!("unknown flag {arg}")),
            _ => program = Some(PathBuf::from(arg)),
        }
    }
    options.args = args;
    match project(program.as_deref())? {
        Some(project) => caribou_driver::run_project(&project, options),
        None => caribou_driver::run(&program.expect("named"), options),
    }
    .map_err(|e| format!("{e:#}"))
}

fn build(argv: &mut impl Iterator<Item = String>) -> Result<(), String> {
    let mut program = None;
    let mut out = None;
    let mut target = None;
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "-o" => out = Some(PathBuf::from(argv.next().ok_or("-o takes a path")?)),
            "--target" => target = Some(argv.next().ok_or("--target takes a triple")?),
            _ if arg.starts_with('-') => return Err(format!("unknown flag {arg}")),
            _ if program.is_some() => return Err(USAGE.to_owned()),
            _ => program = Some(PathBuf::from(arg)),
        }
    }
    let written = match project(program.as_deref())? {
        Some(project) => {
            let hl = project.compile_haxe().map_err(|e| format!("{e:#}"))?;
            let target_dir = project.target_dir();
            match target {
                Some(triple) => {
                    let out = out.unwrap_or_else(|| target_dir.join(format!("{}.wasm", project.name)));
                    let plugins: Vec<PathBuf> = project.plugins.values().cloned().collect();
                    aot(&hl, &triple, Some(&out), &plugins)?
                }
                None => {
                    let out = out.unwrap_or_else(|| target_dir.join(format!("{}.cb", project.name)));
                    caribou_driver::bundle::write_from(&hl, &project.sources, &out)
                        .map_err(|e| format!("{e:#}"))?
                }
            }
        }
        None => {
            let program = program.expect("named");
            match target {
                Some(triple) => aot(&program, &triple, out.as_deref(), &plugins_beside(&program))?,
                None => caribou_driver::bundle::write(&program, out.as_deref())
                    .map_err(|e| format!("{e:#}"))?,
            }
        }
    };
    println!("{}", written.display());
    Ok(())
}

/// The plugin libraries in `plugins/` beside a program named directly.
fn plugins_beside(program: &Path) -> Vec<PathBuf> {
    let dir = program
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("plugins");
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == std::env::consts::DLL_EXTENSION))
        .collect();
    found.sort();
    found
}

/// The project a command acts on: the project file it names, else, when
/// it names no program, the one in the current directory. `None` for a
/// program named directly.
fn project(program: Option<&Path>) -> Result<Option<Project>, String> {
    match program {
        Some(p) if p.extension().is_some_and(|e| e == cbproj::EXTENSION) => {
            Project::load(p).map(Some).map_err(|e| format!("{e:#}"))
        }
        Some(_) => Ok(None),
        None => match Project::find(Path::new(".")).map_err(|e| format!("{e:#}"))? {
            Some(project) => Ok(Some(project)),
            None => Err(format!("no program named and no .cbproj here\n{USAGE}")),
        },
    }
}

#[cfg(feature = "llvm")]
fn aot(
    program: &Path,
    triple: &str,
    out: Option<&Path>,
    plugins: &[PathBuf],
) -> Result<PathBuf, String> {
    caribou_driver::aot::build(program, triple, out, plugins).map_err(|e| format!("{e:#}"))
}

#[cfg(not(feature = "llvm"))]
fn aot(_: &Path, triple: &str, _: Option<&Path>, _: &[PathBuf]) -> Result<PathBuf, String> {
    Err(format!(
        "`--target {triple}` builds ahead of time, which this caribou was built without: \
         its `llvm` feature"
    ))
}

fn describe(files: &[String]) -> Result<(), String> {
    if files.is_empty() {
        return Err(USAGE.to_owned());
    }
    let mut modules = Vec::with_capacity(files.len());
    for file in files {
        let path = std::path::Path::new(file);
        // A root: every module under it, of every language, with its
        // path.
        if path.is_dir() {
            modules.extend(describe_root(path)?);
            continue;
        }
        // A plugin library: its classes, one module each.
        if path
            .extension()
            .is_some_and(|e| e == std::env::consts::DLL_EXTENSION)
        {
            modules.extend(caribou_plugin::describe(path).map_err(|e| e.to_string())?);
            continue;
        }
        let source =
            std::fs::read_to_string(file).map_err(|e| format!("cannot read '{file}': {e}"))?;
        let name = std::path::Path::new(file)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("module");
        modules.push(
            caribou_wren::describe::describe_source(name, &source)
                .map_err(|e| format!("{file}: {e}"))?,
        );
    }
    let json = serde_json::to_string_pretty(&modules).map_err(|e| e.to_string())?;
    println!("{json}");
    Ok(())
}

/// The modules under `root`: Wren's from their source, and those of the
/// Zyntax frontends at the root as they publish.
fn describe_root(root: &std::path::Path) -> Result<Vec<caribou::describe::ModuleDesc>, String> {
    let mut modules = Vec::new();
    let mut wren = Vec::new();
    caribou_driver::bundle::wren_modules(root, root, &mut wren).map_err(|e| e.to_string())?;
    wren.sort();
    for (_, path) in wren {
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("module");
        let mut desc = caribou_wren::describe::describe_source(name, &source)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        desc.path = Some(path.to_string_lossy().into_owned());
        modules.push(desc);
    }
    modules.extend(caribou_zyntax::describe(
        root,
        caribou_driver::project::builtins(&[root]),
    )?);
    Ok(modules)
}

fn main() {
    let mut argv = std::env::args().skip(1);
    let result = match argv.next().as_deref() {
        Some("run") => run(&mut argv),
        Some("build") => build(&mut argv),
        Some("describe") => describe(&argv.collect::<Vec<_>>()),
        _ => Err(USAGE.to_owned()),
    };
    if let Err(e) = result {
        eprintln!("caribou: {e}");
        process::exit(2);
    }
}
