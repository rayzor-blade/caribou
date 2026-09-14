//! The cost of an object, a string or a sequence crossing the bridge, and
//! the memory it leaves behind: the same object crossing again, a fresh
//! one each time, one made by the callee and dropped by the caller, a
//! string in and out, and a hundred-element sequence walked on the other
//! side. Each cell is one loop of `n` iterations in a function of the
//! calling language (`fixtures/src/Bench.hx`, `fixtures/src/bench/
//! tally.wren`), timed as one call; beside the time, the bytes the core
//! heap handed out per iteration, and the collections and Wren cycles the
//! runs took.
//!
//!     cargo bench -p caribou-interop --bench transfer -- [--n 200000]
//!         [--runs 5] [--only <operation>] [--column <0-1>]

use std::path::PathBuf;
use std::time::Instant;

use caribou::diag::NoSources;
use caribou_abi::Value;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use wren_lift::runtime::engine::ExecutionMode;

const OPERATIONS: [(&str, &str); 5] = [
    ("same object", "Same"),
    ("fresh object", "Fresh"),
    ("returned object", "Returned"),
    ("string", "String"),
    ("sequence", "Sequence"),
];

/// (column, namespace, module, class, member prefix, argument as int)
const COLUMNS: [(&str, &str, &str, &str, &str, bool); 2] = [
    ("Haxe→Wren", "bench", "Bench", "Bench", "wren", true),
    ("Wren→Haxe", "bench", "tally", "Tally", "haxe", false),
];

struct Args {
    n: usize,
    runs: usize,
    only: Option<String>,
    column: Option<usize>,
}

fn args() -> Args {
    let mut out = Args {
        n: 200_000,
        runs: 5,
        only: None,
        column: None,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--n" => out.n = argv.next().and_then(|v| v.parse().ok()).unwrap_or(out.n),
            "--runs" => out.runs = argv.next().and_then(|v| v.parse().ok()).unwrap_or(out.runs),
            "--only" => out.only = argv.next(),
            "--column" => out.column = argv.next().and_then(|v| v.parse().ok()),
            _ => {}
        }
    }
    out
}

/// Bytes the core heap has handed out so far.
fn allocated() -> f64 {
    let mut total = 0.0;
    unsafe { caribou::heap::stats(&mut total, std::ptr::null_mut(), std::ptr::null_mut()) };
    total
}

fn main() {
    let args = args();
    let program = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/bench.hl");
    let mut session = Session::open(
        &program,
        Options {
            mode: Mode::Hybrid,
            wren_mode: ExecutionMode::Tiered,
            ..Options::default()
        },
    )
    .expect("the benchmark program opens");
    session.start().expect("its empty main runs");

    println!(
        "{} iterations, median of {} runs; per iteration: ns, bytes the heap handed out; per run: collections, Wren cycles\n",
        args.n, args.runs
    );
    print!("{:<17}", "");
    for (column, ..) in COLUMNS {
        print!("{column:>34}");
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
        print!("{label:<17}");
        for (i, (_, namespace, module, class, prefix, int)) in COLUMNS.into_iter().enumerate() {
            if args.column.is_some_and(|c| c != i) {
                print!("{:>34}", "");
                continue;
            }
            let member = format!("{prefix}{suffix}");
            let arg = if int {
                Value::int(args.n as i32)
            } else {
                Value::number(args.n as f64)
            };
            let call = |session: &mut Session| {
                let bytes = allocated();
                let collections = caribou::heap::collections();
                let cycles = caribou_wren::report::cycles(session.wren()).0;
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
                let ns = started.elapsed().as_nanos() as f64 / args.n as f64;
                (
                    ns,
                    (allocated() - bytes) / args.n as f64,
                    caribou::heap::collections() - collections,
                    caribou_wren::report::cycles(session.wren()).0 - cycles,
                )
            };
            // Warm: the tiers promote what is hot before the measured runs.
            call(&mut session);
            call(&mut session);
            let mut samples: Vec<_> = (0..args.runs).map(|_| call(&mut session)).collect();
            samples.sort_by(|a, b| a.0.total_cmp(&b.0));
            let (ns, bytes, collections, cycles) = samples[samples.len() / 2];
            print!("{ns:>10.1} ns {bytes:>7.1} B {collections:>4} gc {cycles:>4} cy");
        }
        println!();
    }
    // `ASH_PROFILE=sample` says where a cell's time went.
    caribou_ash::program::profile_report();
}
