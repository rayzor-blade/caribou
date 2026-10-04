//! The cost of using a plugin from ZynML and Python: a call to one of its
//! functions or methods, a read and a write of a declared field, and a
//! string result. A member the plugin declares in place is the program's
//! direct call or load (zyntax_embed's native bindings); every other goes
//! through the protocol.
//!
//! Every cell is one operation looped `n` times inside a function of the
//! calling language (`fixtures/native/game/native_*`), timed from here
//! as one call; the `loop` row is the loop alone. The median of the
//! measured runs is reported, in nanoseconds per iteration.
//!
//!     cargo bench -p caribou-interop --bench native -- [--n 1000000] [--runs 5]

use std::path::PathBuf;
use std::time::Instant;

use caribou::bridge;
use caribou::registry::{self, Namespace};
use caribou::world::{Config, LANG_CORE, World};
use caribou_zyntax::Frontend;

const ROWS: [(&str, &str); 6] = [
    ("loop", "baseline"),
    ("static call", "statics"),
    ("method call", "methods"),
    ("field read", "getter"),
    ("field write", "setter"),
    ("string result", "strings"),
];

const COLUMNS: [(&str, &str); 2] = [("ZynML", "native_zyn"), ("Python", "native_py")];

fn flag(args: &[String], name: &str, default: usize) -> usize {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map_or(default, |v| v.parse().expect("a count"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n = flag(&args, "--n", 1_000_000);
    let runs = flag(&args, "--runs", 5);

    let library = format!(
        "{}caribou_plugin_math.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    );
    let plugin = caribou_plugin::load(
        &PathBuf::from(env!("CARIBOU_TEST_PLUGINS"))
            .join("plugins/debug")
            .join(library),
    )
    .expect("the math plugin loads");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/native");
    let world = World::new(Config {
        namespaces: vec![Namespace {
            name: "game".to_owned(),
            langs: vec!["zynml".to_owned(), "python".to_owned()],
            modules: None,
        }],
        roots: vec![root],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_plugin::Runtime::new(vec![plugin])))
        .unwrap();
    let zynml = Frontend::snapshot(zynml::snapshot_bytes()).expect("the snapshot loads");
    let python = Frontend::new(Box::new(caribou_python::Python::new()));
    world
        .register(Box::new(caribou_zyntax::Runtime::new(vec![zynml, python])))
        .unwrap();

    print!("{:<14}", format!("n={n}"));
    for (label, _) in COLUMNS {
        print!("{label:>12}");
    }
    println!();
    let modules: Vec<_> = COLUMNS
        .iter()
        .map(|(_, module)| {
            registry::lookup_or_load("game", module)
                .unwrap_or_else(|e| panic!("{module}: {e:?}"))
                .unwrap_or_else(|| panic!("game has {module}"))
        })
        .collect();
    for (label, function) in ROWS {
        print!("{label:<14}");
        for module in &modules {
            let f = module
                .functions
                .iter()
                .find(|f| f.name == function)
                .unwrap_or_else(|| panic!("the module publishes {function}"));
            let call = || {
                let started = Instant::now();
                bridge::call(
                    f.target,
                    &[caribou::error::Int64::value(n as i64)],
                    LANG_CORE,
                )
                .unwrap_or_else(|e| panic!("{function}: {}", bridge::describe(e)));
                started.elapsed().as_nanos() as f64 / n as f64
            };
            // Warm: the tiers promote what is hot before the measured runs.
            call();
            call();
            let mut samples: Vec<f64> = (0..runs).map(|_| call()).collect();
            samples.sort_by(|a, b| a.total_cmp(b));
            print!("{:>12.1}", samples[samples.len() / 2]);
        }
        println!();
    }
}
