//! The interop benchmark built ahead of time for wasm and run under
//! wasmtime, as `caribou build --target wasm32-wasip1` and `caribou run`
//! do: the same cells as `interop.rs`, over the same `Bench.hx` and
//! `bench/tally.wren`, each call across the boundary a linked call. The
//! program (`wasm/WasmBench.hx`) times itself and prints the table.
//!
//!     cargo bench -p caribou-interop --features llvm --bench interop_wasm --
//!         [--n 200000] [--runs 5]

use std::path::{Path, PathBuf};

use caribou_driver::cbproj::Project;

fn main() {
    let (mut n, mut runs) = ("200000".to_owned(), "5".to_owned());
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--n" => n = argv.next().unwrap_or(n),
            "--runs" => runs = argv.next().unwrap_or(runs),
            // cargo bench passes its own flags through.
            _ => {}
        }
    }

    // A project of the benchmark's own: its program, and the two classes
    // the hosted benchmark measures.
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixtures = crate_dir.join("fixtures/src");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("interop-wasm");
    std::fs::create_dir_all(dir.join("src/bench")).unwrap();
    for (from, to) in [
        (
            crate_dir.join("benches/wasm/WasmBench.hx"),
            "src/WasmBench.hx",
        ),
        (fixtures.join("Bench.hx"), "src/Bench.hx"),
        (fixtures.join("bench/tally.wren"), "src/bench/tally.wren"),
    ] {
        std::fs::copy(&from, dir.join(to)).unwrap_or_else(|e| panic!("{}: {e}", from.display()));
    }
    std::fs::write(
        dir.join("interop.cbproj"),
        "[project]\nname = \"interop\"\nentry = \"haxe:WasmBench\"\nlanguages = [\"haxe\", \"wren\"]\n",
    )
    .unwrap();

    let project = Project::load(&dir.join("interop.cbproj")).expect("the project file");
    let program = project.compile_haxe().unwrap_or_else(|e| panic!("{e:#}"));
    let module = caribou_driver::aot::build(
        &program,
        "wasm32-wasip1",
        Some(&project.target_dir().join("interop.wasm")),
        &[],
        &project.sources,
        Some(&project.target_dir()),
    )
    .unwrap_or_else(|e| panic!("{e:#}"));
    let status =
        caribou_driver::aot::run_module(&module, &[n, runs]).unwrap_or_else(|e| panic!("{e:#}"));
    assert_eq!(status, 0, "the benchmark program exited with {status}");
}
