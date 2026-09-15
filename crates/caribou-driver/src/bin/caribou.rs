//! The caribou command.
//!
//!     caribou run [--mode interp|hybrid] [--wren interpreter|tiered] [--report] <program> [args...]
//!     caribou build <program.hl> [-o <out.caribou>]
//!     caribou describe <module.wren>...
//!
//! `run` runs a program with every resident language, from the project
//! directory: the other languages' modules are found under the project's
//! class paths and load on first use. The program is a `.hl`, or a bundle
//! `build` wrote from one: the program and every module under the class
//! paths in one file, run the same way anywhere. `--report` prints, when
//! the program ends, what the run did: the tier each function reached
//! and how each send across the bridge went. `describe` prints the
//! modules' interfaces as JSON, for a build step.

use std::path::PathBuf;
use std::process;

use caribou_ash::Mode;
use wren_lift::runtime::engine::ExecutionMode;

const USAGE: &str = "usage: caribou run [--mode interp|hybrid] [--wren interpreter|tiered] [--report] <program> [args...]\n       caribou build <program.hl> [-o <out.caribou>]\n       caribou describe <module.wren>...";

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
    let program = program.ok_or_else(|| USAGE.to_owned())?;
    options.args = args;
    caribou_driver::run(&program, options).map_err(|e| format!("{e:#}"))
}

fn build(argv: &mut impl Iterator<Item = String>) -> Result<(), String> {
    let mut program = None;
    let mut out = None;
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "-o" => out = Some(PathBuf::from(argv.next().ok_or("-o takes a path")?)),
            _ if arg.starts_with('-') => return Err(format!("unknown flag {arg}")),
            _ if program.is_some() => return Err(USAGE.to_owned()),
            _ => program = Some(PathBuf::from(arg)),
        }
    }
    let program = program.ok_or_else(|| USAGE.to_owned())?;
    let written =
        caribou_driver::bundle::write(&program, out.as_deref()).map_err(|e| format!("{e:#}"))?;
    println!("{}", written.display());
    Ok(())
}

fn describe(files: &[String]) -> Result<(), String> {
    if files.is_empty() {
        return Err(USAGE.to_owned());
    }
    let mut modules = Vec::with_capacity(files.len());
    for file in files {
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
