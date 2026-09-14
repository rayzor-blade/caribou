//! A minimal runner: wren_lift's interpreter or its tiered JIT over the core,
//! with the Immix strategy. It does what `wlift` does for a `.wren` file in
//! `--mode interpreter` and `--mode tiered`, with the seam installed first. It
//! exists to prove the seam, not to replace that CLI: no module loader, no
//! bytecode caches, no packages.
//!
//!     caribou-wren [--mode interpreter|tiered] [--no-install] [--gc-stats] <file.wren>
//!     caribou-wren --describe <file.wren>...
//!
//! `--no-install` runs the same program on wren_lift's own heap. Exit codes
//! are `wlift`'s: 65 for a compile error, 70 for a runtime error.
//!
//! `--describe` prints each module's interface (`caribou::describe`) as a
//! JSON array, one entry per file, without running anything: what a build
//! step reads to declare the classes in another language. The module's
//! name is the file's stem; a build step knowing better substitutes its
//! own.

use std::process;

use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::rt::wlift_rt_installed;
use wren_lift::runtime::vm::{VM, VMConfig};

const USAGE: &str = "usage: caribou-wren [--mode interpreter|tiered] [--no-install] [--gc-stats] <file.wren>\n       caribou-wren --describe <file.wren>...";

struct Args {
    mode: ExecutionMode,
    install: bool,
    gc_stats: bool,
    describe: bool,
    files: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut mode = ExecutionMode::Tiered;
    let mut install = true;
    let mut gc_stats = false;
    let mut describe = false;
    let mut files = Vec::new();
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
            "--describe" => describe = true,
            _ if arg.starts_with("--") => return Err(format!("unknown flag {arg}")),
            _ if !files.is_empty() && !describe => return Err(USAGE.to_string()),
            _ => files.push(arg),
        }
    }
    if files.is_empty() {
        return Err(USAGE.to_string());
    }
    Ok(Args {
        mode,
        install,
        gc_stats,
        describe,
        files,
    })
}

/// Each file's interface, as one JSON array.
fn describe(files: &[String]) -> Result<(), String> {
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

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if args.describe {
        return describe(&args.files);
    }
    let file = &args.files[0];
    let source = std::fs::read_to_string(file).map_err(|e| format!("cannot read '{file}': {e}"))?;

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
    // Every fiber on a stack of its own, on the core's heap and on
    // wren_lift's alike.
    vm.krio_fiber_active = true;
    let module_name = file.strip_suffix(".wren").unwrap_or(file);
    // The VM the bridge's Wren entries run on, for the life of the run.
    // Only on the core's heap: entering keeps the VM on its heap record,
    // which wren_lift's own heap has none of.
    let result = if args.install {
        let previous = unsafe { caribou_wren::enter_vm(&mut vm) };
        // On failure wlift exits from here, VM and all.
        let result = vm.interpret(module_name, &source);
        unsafe { caribou_wren::leave_vm(previous) };
        result
    } else {
        vm.interpret(module_name, &source)
    };
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
