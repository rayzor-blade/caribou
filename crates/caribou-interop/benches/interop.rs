//! The cost of a call across the bridge, beside the same call inside each
//! language.
//!
//! Every cell is one operation looped `n` times inside a function of the
//! calling language (`fixtures/src/Bench.hx`, `fixtures/src/bench/
//! tally.wren`), timed from here as one call, so a cell includes the loop
//! itself and one crossing of the harness's own; the columns for a
//! language's own calls are its baselines. The median of the measured runs
//! is reported, in nanoseconds per iteration.
//!
//!     cargo bench -p caribou-interop -- [--mode interp|hybrid]
//!         [--wren interpreter|tiered] [--n 200000] [--runs 5]
//!         [--only <operation>] [--column <0-3>]
//!
//! `--only` and `--column` run one cell, for a profiler to sample.

use std::path::PathBuf;
use std::time::Instant;

use caribou::diag::NoSources;
use caribou_abi::Value;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use wren_lift::runtime::engine::ExecutionMode;

const OPERATIONS: [(&str, &str); 6] = [
    ("static call", "Static"),
    ("method call", "Method"),
    ("getter", "Getter"),
    ("setter", "Setter"),
    ("closure call", "Closure"),
    ("construct", "New"),
];

/// (column, namespace, module, class, member prefix, argument as int)
const COLUMNS: [(&str, &str, &str, &str, &str, bool); 4] = [
    ("Haxe→Haxe", "bench", "Bench", "Bench", "haxe", true),
    ("Wren→Wren", "bench", "tally", "Tally", "wren", false),
    ("Haxe→Wren", "bench", "Bench", "Bench", "wren", true),
    ("Wren→Haxe", "bench", "tally", "Tally", "haxe", false),
];

struct Args {
    mode: Mode,
    wren_mode: ExecutionMode,
    n: usize,
    runs: usize,
    only: Option<String>,
    column: Option<usize>,
}

fn args() -> Args {
    let mut out = Args {
        mode: Mode::Hybrid,
        wren_mode: ExecutionMode::Tiered,
        n: 200_000,
        runs: 5,
        only: None,
        column: None,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--mode" => {
                out.mode = match argv.next().as_deref() {
                    Some("interp") => Mode::Interp,
                    _ => Mode::Hybrid,
                }
            }
            "--wren" => {
                out.wren_mode = match argv.next().as_deref() {
                    Some("interpreter") => ExecutionMode::Interpreter,
                    _ => ExecutionMode::Tiered,
                }
            }
            "--n" => out.n = argv.next().and_then(|v| v.parse().ok()).unwrap_or(out.n),
            "--runs" => out.runs = argv.next().and_then(|v| v.parse().ok()).unwrap_or(out.runs),
            "--only" => out.only = argv.next(),
            "--column" => out.column = argv.next().and_then(|v| v.parse().ok()),
            // cargo bench passes its own flags through.
            _ => {}
        }
    }
    out
}

fn main() {
    let args = args();
    let program = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/bench.hl");
    let mut session = Session::open(
        &program,
        Options {
            mode: args.mode,
            wren_mode: args.wren_mode,
            // The program as it runs, not as it reloads.
            reload: false,
            ..Options::default()
        },
    )
    .expect("the benchmark program opens");
    session.start().expect("its empty main runs");

    let mode = match args.mode {
        Mode::Interp => "interp",
        Mode::Hybrid => "hybrid",
    };
    let wren = match args.wren_mode {
        ExecutionMode::Interpreter => "interpreter",
        _ => "tiered",
    };
    println!(
        "ash {mode}, wren_lift {wren}, {} iterations, median of {} runs, ns per call\n",
        args.n, args.runs
    );
    print!("{:<14}", "");
    for (column, ..) in COLUMNS {
        print!("{column:>12}");
    }
    println!();

    for (label, suffix) in OPERATIONS {
        if args
            .only
            .as_deref()
            .is_some_and(|only| !label.starts_with(only))
        {
            continue;
        }
        print!("{label:<14}");
        for (i, (_, namespace, module, class, prefix, int)) in COLUMNS.into_iter().enumerate() {
            if args.column.is_some_and(|c| c != i) {
                print!("{:>12}", "");
                continue;
            }
            let member = format!("{prefix}{suffix}");
            let arg = if int {
                Value::int(args.n as i32)
            } else {
                Value::number(args.n as f64)
            };
            let mut call = || {
                let started = Instant::now();
                session
                    .call(namespace, module, class, &member, &[arg])
                    .unwrap_or_else(|e| {
                        panic!(
                            "{member}: {}",
                            caribou::diag::render_string(
                                &caribou::diag::report(e),
                                &NoSources,
                                false
                            )
                        )
                    });
                started.elapsed().as_nanos() as f64 / args.n as f64
            };
            // Warm: the tiers promote what is hot before the measured runs.
            call();
            call();
            let mut samples: Vec<f64> = (0..args.runs).map(|_| call()).collect();
            samples.sort_by(|a, b| a.total_cmp(b));
            let median = samples[samples.len() / 2];
            print!("{median:>12.1}");
        }
        println!();
    }
    // `WLIFT_TIER_STATS=1` and `ASH_TIER_LOG=1` say which tier ran what,
    // and how many collections the run took;
    // `ASH_PROFILE=sample` says where the time went, Ash's compiled code
    // named.
    if std::env::var_os("WLIFT_TIER_STATS").is_some() {
        let vm = session.wren();
        let interner = &vm.interner;
        vm.engine.dump_tier_stats(interner);
        eprintln!("collections={}", caribou::heap::collections());
    }
    caribou_ash::program::profile_report();
}
