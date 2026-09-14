//! The Swarm arena: a headless game loop split the way a scripted engine
//! splits it, engine in Haxe and behaviour in Wren, beside the same loop
//! in Haxe alone and in Wren alone. Every frame the engine calls each
//! entity's behaviour, the behaviour reads its neighbours through the
//! engine's statics and writes its velocity through its entity's fields,
//! collisions go back to the script, waves spawn script objects, a tween
//! the script gave is sampled, and the HUD's text crosses. So a frame
//! holds every crossing the interop bench measures alone, in the mix and
//! at the rate a game makes them. All three runs (`fixtures/src/Swarm.hx`,
//! `fixtures/src/swarm/`) end on the same checksum, which is checked.
//!
//!     cargo bench -p caribou-interop --bench swarm -- [--entities 1000]
//!         [--frames 600] [--runs 3] [--mode haxe|wren|mixed]
//!
//! Reported per run: milliseconds per frame, the median of the runs, and
//! the mixed run's cost over each baseline.

use std::path::PathBuf;
use std::time::Instant;

use caribou::diag::NoSources;
use caribou_abi::Value;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use wren_lift::runtime::engine::ExecutionMode;

const RUNS: [(&str, &str); 3] = [
    ("haxe", "runHaxe"),
    ("wren", "runWren"),
    ("mixed", "runMixed"),
];

struct Args {
    entities: i32,
    frames: i32,
    runs: usize,
    mode: Option<String>,
}

fn args() -> Args {
    let mut out = Args {
        entities: 1000,
        frames: 600,
        runs: 3,
        mode: None,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--entities" => out.entities = argv.next().and_then(|v| v.parse().ok()).unwrap_or(1000),
            "--frames" => out.frames = argv.next().and_then(|v| v.parse().ok()).unwrap_or(600),
            "--runs" => out.runs = argv.next().and_then(|v| v.parse().ok()).unwrap_or(3),
            "--mode" => out.mode = argv.next(),
            "--bench" => {}
            _ => {}
        }
    }
    out
}

fn main() {
    let args = args();
    let program = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/swarm.hl");
    let mut session = Session::open(
        &program,
        Options {
            mode: Mode::Hybrid,
            wren_mode: ExecutionMode::Tiered,
            ..Options::default()
        },
    )
    .expect("the arena program opens");
    session.start().expect("its empty main runs");

    println!(
        "swarm: {} entities, {} frames, median of {} runs, ms per frame\n",
        args.entities, args.frames, args.runs
    );
    let mut medians = Vec::new();
    for (label, member) in RUNS {
        if args.mode.as_deref().is_some_and(|m| m != label) {
            continue;
        }
        let mut run = || {
            let started = Instant::now();
            let checksum = session
                .call(
                    "swarm",
                    "Swarm",
                    "Swarm",
                    member,
                    &[Value::int(args.entities), Value::int(args.frames)],
                )
                .unwrap_or_else(|e| {
                    panic!(
                        "{member}: {}",
                        caribou::diag::render_string(&caribou::diag::report(e), &NoSources, false)
                    )
                });
            let ms = started.elapsed().as_secs_f64() * 1000.0 / args.frames as f64;
            (ms, checksum.as_number().unwrap_or(f64::NAN))
        };
        // Warm: the tiers compile what is hot before the measured runs.
        let (_, checksum) = run();
        let mut samples: Vec<f64> = (0..args.runs).map(|_| run().0).collect();
        samples.sort_by(|a, b| a.total_cmp(b));
        let median = samples[samples.len() / 2];
        println!("{label:<8}{median:>10.3} ms/frame   checksum {checksum}");
        medians.push((label, median, checksum));
    }
    if let (Some(mixed), Some(haxe), Some(wren)) = (
        medians.iter().find(|m| m.0 == "mixed"),
        medians.iter().find(|m| m.0 == "haxe"),
        medians.iter().find(|m| m.0 == "wren"),
    ) {
        println!(
            "\nmixed over haxe: x{:.2}   mixed over wren: x{:.2}",
            mixed.1 / haxe.1,
            mixed.1 / wren.1
        );
        if mixed.2 != haxe.2 || mixed.2 != wren.2 {
            println!("checksums differ: the three runs are not the same arena");
        }
    }
    // `ASH_PROFILE=sample` says where a run's time went, both languages'
    // compiled code named.
    caribou_ash::program::profile_report();
}
