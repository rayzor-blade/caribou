//! With the `llvm` feature, which is what builds programs ahead of time, the
//! runtime a wasm program links against: `caribou-runtime` built for
//! `wasm32-wasip1` and joined with WASI's libc and `libsetjmp` into one
//! relocatable object, as Ash's own wasm runtime is. `caribou build --target
//! wasm32-wasip1` hands it to Ash's linker. A machine with no WASI sysroot
//! builds the driver without it and says so.

use std::env;
use std::path::{Path, PathBuf};

// The driver uses all of it; this script, the runtime build.
#[allow(dead_code)]
#[path = "src/wasm_toolchain.rs"]
mod wasm_toolchain;

const TARGET: &str = "wasm32-wasip1";

fn main() {
    for variable in caribou_wasm_toolchain::CONFIG_ENV {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    if env::var_os("CARGO_FEATURE_LLVM").is_none() {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../..")
        .canonicalize()
        .unwrap();
    println!(
        "cargo:rerun-if-changed={}",
        root.join("Cargo.lock").display()
    );
    for entry in std::fs::read_dir(root.join("crates")).unwrap() {
        let path = entry.unwrap().path();
        if path.join("Cargo.toml").is_file() && !path.ends_with("caribou-driver") {
            println!("cargo:rerun-if-changed={}", path.join("src").display());
        }
    }

    let Some(sysroot) = wasm_toolchain::sysroot(TARGET) else {
        println!(
            "cargo:warning=no WASI sysroot (set WASI_SYSROOT or WASI_SDK_PATH, or put a \
             WASI SDK on PATH): this caribou builds no wasm programs"
        );
        return;
    };
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    // Cargo holds the outer target directory's lock, so the runtime builds
    // in one of its own.
    let target_dir = nested_target(&out, "caribou-runtime");
    let cargo = env::var_os("CARGO").unwrap();
    let status = wasm_toolchain::cargo_build(&cargo, &root, TARGET, &sysroot, &target_dir)
        .args(["--locked", "-p", "caribou-runtime"])
        .status()
        .expect("running cargo");
    assert!(
        status.success(),
        "building caribou-runtime for {TARGET}: {status}"
    );

    let archive = target_dir.join(TARGET).join("release/libcaribou_runtime.a");
    let object = out.join(TARGET).join("caribou_runtime.o");
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    wasm_toolchain::prelink(&rustc, &root, &archive, &sysroot, TARGET, &object)
        .unwrap_or_else(|e| panic!("{e}"));
    println!("cargo:rustc-env=CARIBOU_WASM_RUNTIME={}", object.display());
}

/// A target directory for a nested build that stays put when this build
/// script reruns: `nested/<name>` beside the `build` directory `OUT_DIR`
/// lies in, whichever layout cargo gives it (`build/<package>-<hash>/out`
/// or `build/<package>/<hash>/out`). A rerun reuses and updates it rather
/// than leaving a whole build behind in each `OUT_DIR`.
fn nested_target(out: &Path, name: &str) -> PathBuf {
    out.ancestors()
        .find(|dir| dir.file_name().is_some_and(|n| n == "build"))
        .and_then(Path::parent)
        .expect("OUT_DIR lies in a build directory")
        .join("nested")
        .join(name)
}
