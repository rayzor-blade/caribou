//! With the `llvm` feature, which is what builds programs ahead of time, the
//! runtime a wasm program links against: `caribou-runtime` built for
//! `wasm32-wasip1` and joined with WASI's libc and `libsetjmp` into one
//! relocatable object, as Ash's own wasm runtime is. `caribou build --target
//! wasm32-wasip1` hands it to Ash's linker. A machine with no WASI sysroot
//! builds the driver without it and says so.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "wasm32-wasip1";

fn main() {
    println!("cargo:rerun-if-env-changed=WASI_SYSROOT");
    if env::var_os("CARGO_FEATURE_LLVM").is_none() {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../..")
        .canonicalize()
        .unwrap();
    println!("cargo:rerun-if-changed={}", root.join("Cargo.lock").display());
    for entry in std::fs::read_dir(root.join("crates")).unwrap() {
        let path = entry.unwrap().path();
        if path.join("Cargo.toml").is_file() && !path.ends_with("caribou-driver") {
            println!("cargo:rerun-if-changed={}", path.join("src").display());
        }
    }

    let Some(sysroot) = sysroot() else {
        println!(
            "cargo:warning=no WASI sysroot (set WASI_SYSROOT, or install wasi-libc or the \
             WASI SDK): this caribou builds no wasm programs"
        );
        return;
    };
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target_dir = out.join("runtime");

    // Cargo holds the outer target directory's lock, so the runtime builds in
    // one of its own. The workspace's config gives wasm targets Ash's flags.
    let mut build = Command::new(env::var_os("CARGO").unwrap());
    build
        .args(["build", "--locked", "--release", "-p", "caribou-runtime", "--target", TARGET])
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir(&root)
        .env("WASI_SYSROOT", &sysroot)
        .env(
            "CFLAGS_wasm32_wasip1",
            format!("--target={TARGET} --sysroot={}", sysroot.display()),
        )
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CLIPPY_ARGS")
        .env("CARGO_INCREMENTAL", "0");
    if let Some(clang) = clang() {
        build.env("CC_wasm32_wasip1", clang);
    }
    run(&mut build, "building caribou-runtime for wasm32-wasip1");

    let archive = target_dir.join(TARGET).join("release/libcaribou_runtime.a");
    let object = out.join(TARGET).join("caribou_runtime.o");
    std::fs::create_dir_all(object.parent().unwrap()).unwrap();
    let lib = sysroot.join("lib").join(TARGET);
    run(
        Command::new(lld())
            .args(["-flavor", "wasm", "-r", "-o"])
            .arg(&object)
            .arg("--whole-archive")
            .arg(&archive)
            .arg("--no-whole-archive")
            .arg(format!("-L{}", lib.display()))
            .args(["-lc", "-lsetjmp"]),
        "joining the wasm runtime with WASI's libc",
    );
    println!("cargo:rustc-env=CARIBOU_WASM_RUNTIME={}", object.display());
}

fn run(command: &mut Command, what: &str) {
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(status.success(), "{what}: {status}");
}

/// A WASI sysroot with the libc and `libsetjmp` the runtime is joined with.
fn sysroot() -> Option<PathBuf> {
    env::var_os("WASI_SYSROOT")
        .map(PathBuf::from)
        .into_iter()
        .chain(
            [
                "/opt/homebrew/opt/wasi-libc/share/wasi-sysroot",
                "/usr/local/opt/wasi-libc/share/wasi-sysroot",
                "/opt/wasi-sdk/share/wasi-sysroot",
                "/usr/local/wasi-sdk/share/wasi-sysroot",
                "/usr/share/wasi-sysroot",
            ]
            .map(PathBuf::from),
        )
        .find(|s| {
            let lib = s.join("lib").join(TARGET);
            lib.join("libc.a").is_file() && lib.join("libsetjmp.a").is_file()
        })
}

/// A clang whose WebAssembly backend takes the setjmp lowering's flags;
/// Apple's, first on a Mac's path, refuses them.
fn clang() -> Option<PathBuf> {
    [
        "/opt/homebrew/opt/llvm/bin/clang",
        "/usr/local/opt/llvm/bin/clang",
        "/opt/wasi-sdk/bin/clang",
        "/usr/local/wasi-sdk/bin/clang",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// The linker every Rust toolchain ships, which speaks wasm.
fn lld() -> PathBuf {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let sysroot = Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .expect("rustc prints its sysroot");
    let sysroot = PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim());
    let host = env::var("HOST").unwrap();
    let bin = Path::new("lib/rustlib").join(host).join("bin/rust-lld");
    sysroot.join(bin)
}
