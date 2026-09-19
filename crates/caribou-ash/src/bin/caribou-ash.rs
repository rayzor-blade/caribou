//! A minimal runner: ash's interpreter, with its Cranelift tier in hybrid
//! mode, over the core. It does what ash's CLI does for `--mode interp` and
//! `--mode hybrid`, with the seam installed first. It exists to prove the
//! seam, not to replace that CLI.
//!
//!     caribou-ash [--mode interp|hybrid] [--no-install] <file.hl> [args...]
//!
//! `--no-install` runs the same program on ash's own heap and scheduler.

use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use caribou_ash::{Mode, Options};

struct Args {
    options: Options,
    file: PathBuf,
}

fn parse_args() -> Result<Args> {
    // No world asks this runner to reload: the program runs as ash runs it.
    let mut options = Options {
        reload: false,
        ..Options::default()
    };
    let mut file = None;
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        if file.is_some() {
            options.args.push(arg);
            continue;
        }
        match arg.as_str() {
            "--mode" => {
                options.mode = match argv.next().as_deref() {
                    Some("interp") => Mode::Interp,
                    Some("hybrid") => Mode::Hybrid,
                    other => bail!("--mode takes interp or hybrid, not {other:?}"),
                };
            }
            "--no-install" => options.install = false,
            _ if arg.starts_with("--") => bail!("unknown flag {arg}"),
            _ => file = Some(PathBuf::from(arg)),
        }
    }
    let file = file.ok_or_else(|| {
        anyhow!("usage: caribou-ash [--mode interp|hybrid] [--no-install] <file.hl> [args...]")
    })?;
    Ok(Args { options, file })
}

fn run() -> Result<()> {
    let args = parse_args()?;
    let mut program = caribou_ash::load(&args.file, args.options)?;
    program.start()?;
    program.finish();
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        // An uncaught HL exception is the program's failure and already
        // reads as HashLink prints it.
        let text = format!("{e:#}");
        if text.starts_with("Uncaught exception:") {
            eprintln!("{text}");
        } else {
            eprintln!("Error: {text}");
        }
        std::process::exit(1);
    }
}
