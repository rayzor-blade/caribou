//! A minimal runner: wren_lift's interpreter or its tiered JIT over the core,
//! with the Immix strategy. It does what `wlift` does for a `.wren` file in
//! `--mode interpreter` and `--mode tiered`, with the seam installed first. It
//! exists to prove the seam, not to replace that CLI: no module loader, no
//! bytecode caches, no packages.
//!
//!     caribou-wren [--mode interpreter|tiered] [--no-install] [--gc-stats] <file.wren>
//!
//! `--no-install` runs the same program on wren_lift's own heap. Exit codes
//! are `wlift`'s: 65 for a compile error, 70 for a runtime error.

use std::process;

use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::rt::wlift_rt_installed;
use wren_lift::runtime::vm::{VM, VMConfig};

const USAGE: &str =
    "usage: caribou-wren [--mode interpreter|tiered] [--no-install] [--gc-stats] <file.wren>";

struct Args {
    mode: ExecutionMode,
    install: bool,
    gc_stats: bool,
    file: String,
}

fn parse_args() -> Result<Args, String> {
    let mut mode = ExecutionMode::Tiered;
    let mut install = true;
    let mut gc_stats = false;
    let mut file = None;
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--mode" => {
                mode = match argv.next().as_deref() {
                    Some("interpreter") => ExecutionMode::Interpreter,
                    Some("tiered") => ExecutionMode::Tiered,
                    other => {
                        return Err(format!("--mode takes interpreter or tiered, not {other:?}"));
                    }
                };
            }
            "--no-install" => install = false,
            "--gc-stats" => gc_stats = true,
            _ if arg.starts_with("--") => return Err(format!("unknown flag {arg}")),
            _ if file.is_some() => return Err(USAGE.to_string()),
            _ => file = Some(arg),
        }
    }
    let file = file.ok_or_else(|| USAGE.to_string())?;
    Ok(Args {
        mode,
        install,
        gc_stats,
        file,
    })
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let source = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("cannot read '{}': {e}", args.file))?;

    // Before the VM: its collector mints the heap that seals the table.
    if args.install {
        caribou_wren::install().map_err(|e| e.to_string())?;
    }
    if wlift_rt_installed() != args.install {
        return Err(format!(
            "wren_lift {} the runtime table",
            if args.install {
                "did not take"
            } else {
                "already has"
            }
        ));
    }

    // wlift's defaults for a source file: the step limit by mode, the module
    // named after the path.
    let step_limit = match args.mode {
        ExecutionMode::Interpreter => 1_000_000_000,
        _ => 10_000_000_000,
    };
    let mut vm = VM::new(VMConfig {
        execution_mode: args.mode,
        step_limit,
        gc_strategy: GcStrategy::Immix,
        ..VMConfig::default()
    });
    let module_name = args.file.strip_suffix(".wren").unwrap_or(&args.file);
    // The VM the bridge's Wren entries run on, for the life of the run.
    let previous = unsafe { caribou_wren::enter_vm(&mut vm) };
    // On failure wlift exits from here, VM and all.
    let result = vm.interpret(module_name, &source);
    caribou_wren::leave_vm(previous);
    match result {
        InterpretResult::Success => {}
        InterpretResult::CompileError => process::exit(65),
        InterpretResult::RuntimeError => process::exit(70),
    }

    if args.gc_stats {
        let stats = vm.gc.stats();
        eprintln!("--- GC Stats ---");
        eprintln!("  minor collections: {}", stats.minor_collections);
        eprintln!("  major collections: {}", stats.major_collections);
        eprintln!("  objects allocated:  {}", stats.objects_allocated);
        eprintln!("  objects freed:      {}", stats.objects_freed);
        eprintln!("  objects promoted:   {}", stats.objects_promoted);
        eprintln!("  peak objects:       {}", stats.peak_objects);
        eprintln!("  total allocated:    {} KB", stats.total_allocated / 1024);
        eprintln!("  total freed:        {} KB", stats.total_freed / 1024);
        eprintln!(
            "  gc time:            {:.3}s",
            stats.gc_time_ns as f64 / 1e9
        );
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
