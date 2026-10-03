//! The cost of a call across the bridge, beside the same call inside each
//! language, for every pair of Haxe, Wren and Python.
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
//!         [--only <operation>] [--column <0-8>]
//!
//! `--only` and `--column` run one cell, for a profiler to sample. Without
//! them each cell runs in a process of its own: what one cell leaves in a
//! heap is not the next cell's cost, and Zyntax's heap is not collected
//! under the core (git-bug
//! 0a1ced7817b26944287f3f4c98a2af98e7b15f203ecbb94d5a1fd8362ac607ea).

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

type Cell = Option<(&'static str, &'static str, &'static str, &'static str, bool)>;

/// (column, caller module/class/member prefix and whether its count is an int)
const COLUMNS: [(&str, Cell); 9] = [
    ("Haxe→Haxe", Some(("bench", "Bench", "Bench", "haxe", true))),
    ("Haxe→Wren", Some(("bench", "Bench", "Bench", "wren", true))),
    (
        "Haxe→Python",
        Some(("bench", "Bench", "Bench", "python", true)),
    ),
    (
        "Wren→Haxe",
        Some(("bench", "tally", "Tally", "haxe", false)),
    ),
    (
        "Wren→Wren",
        Some(("bench", "tally", "Tally", "wren", false)),
    ),
    (
        "Wren→Python",
        Some(("bench", "to_python", "ToPython", "python", false)),
    ),
    (
        "Python→Haxe",
        Some(("bench", "python_tally", "PythonTally", "haxe", true)),
    ),
    (
        "Python→Wren",
        Some(("bench", "python_tally", "PythonTally", "wren", true)),
    ),
    (
        "Python→Python",
        Some(("bench", "python_tally", "PythonTally", "python", true)),
    ),
];

struct Args {
    mode: Mode,
    wren_mode: ExecutionMode,
    n: usize,
    runs: usize,
    only: Option<String>,
    column: Option<usize>,
    /// Print only the cell's median: a run of one cell for the full table.
    cell: bool,
}

fn args() -> Args {
    let mut out = Args {
        mode: Mode::Hybrid,
        wren_mode: ExecutionMode::Tiered,
        n: 200_000,
        runs: 5,
        only: None,
        column: None,
        cell: false,
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
            "--cell" => out.cell = true,
            // cargo bench passes its own flags through.
            _ => {}
        }
    }
    out
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Interp => "interp",
        Mode::Hybrid => "hybrid",
    }
}

fn wren_name(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Interpreter => "interpreter",
        _ => "tiered",
    }
}

fn header(args: &Args) {
    println!(
        "ash {}, wren_lift {}, {} iterations, median of {} runs, ns per call\n",
        mode_name(args.mode),
        wren_name(args.wren_mode),
        args.n,
        args.runs
    );
    print!("{:<14}", "");
    for (column, _) in COLUMNS {
        print!("{column:>15}");
    }
    println!();
}

/// The whole table, each cell measured by a run of this program of its
/// own.
fn table(args: &Args) {
    use std::io::Write;
    header(args);
    let me = std::env::current_exe().expect("the bench knows its own path");
    for (label, _) in OPERATIONS {
        print!("{label:<14}");
        for column in 0..COLUMNS.len() {
            let run = std::process::Command::new(&me)
                .args(["--column", &column.to_string(), "--only", label, "--cell"])
                .args(["--n", &args.n.to_string(), "--runs", &args.runs.to_string()])
                .args([
                    "--mode",
                    mode_name(args.mode),
                    "--wren",
                    wren_name(args.wren_mode),
                ])
                .output()
                .expect("the bench runs itself");
            let median = String::from_utf8_lossy(&run.stdout).trim().to_owned();
            let shown = if run.status.success() && !median.is_empty() {
                median
            } else {
                "failed".to_owned()
            };
            print!("{shown:>15}");
            let _ = std::io::stdout().flush();
        }
        println!();
    }
}

fn main() {
    let args = args();
    if args.column.is_none() && args.only.is_none() {
        return table(&args);
    }
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

    if !args.cell {
        header(&args);
    }

    for (label, suffix) in OPERATIONS {
        if args
            .only
            .as_deref()
            .is_some_and(|only| !label.starts_with(only))
        {
            continue;
        }
        if !args.cell {
            print!("{label:<14}");
        }
        for (i, (_, cell)) in COLUMNS.into_iter().enumerate() {
            if args.column.is_some_and(|c| c != i) {
                if !args.cell {
                    print!("{:>15}", "");
                }
                continue;
            }
            let Some((namespace, module, class, prefix, int)) = cell else {
                print!("{:>15}", "n/a");
                continue;
            };
            // Each cell starts after both heaps have retired the previous
            // cell's temporaries. Otherwise the later construction columns
            // inherit collection debt from every earlier column and a full
            // row disagrees sharply with the same cell run by itself.
            session.wren().collect_garbage();
            caribou::heap::major();
            session.wren().collect_garbage();
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
            if args.cell {
                print!("{median:.1}");
            } else {
                print!("{median:>15.1}");
            }
        }
        if !args.cell {
            println!();
        }
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
