//! Building a runtime object for a wasm target: a staticlib built with
//! cargo from caribou's source, joined with WASI's libc and `libsetjmp`
//! into one relocatable object, as Ash's own wasm runtime is. The build
//! script makes the driver's runtime this way, and `aot` a program's own
//! when it links plugins in.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A WASI sysroot with the libc and `libsetjmp` of `target`.
pub fn sysroot(target: &str) -> Option<PathBuf> {
    std::env::var_os("WASI_SYSROOT")
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
            let lib = s.join("lib").join(target);
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

/// A release build for `target` into `target_dir`, run from caribou's
/// source root `root` so its toolchain file and its config, which gives
/// wasm targets Ash's flags, apply. The caller names what to build.
pub fn cargo_build(
    cargo: &OsStr,
    root: &Path,
    target: &str,
    sysroot: &Path,
    target_dir: &Path,
) -> Command {
    let mut build = Command::new(cargo);
    build
        .args(["build", "--release", "--target", target])
        .arg("--target-dir")
        .arg(target_dir)
        .current_dir(root)
        .env("WASI_SYSROOT", sysroot)
        .env(
            format!("CFLAGS_{}", target.replace('-', "_")),
            format!("--target={target} --sysroot={}", sysroot.display()),
        )
        // What an enclosing cargo run sets for its own build.
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CLIPPY_ARGS")
        .env("CARGO_INCREMENTAL", "0");
    if let Some(clang) = clang() {
        build.env(format!("CC_{}", target.replace('-', "_")), clang);
    }
    build
}

/// The linker every Rust toolchain ships, which speaks wasm: `rustc`'s,
/// as it resolves from `root`.
fn lld(rustc: &OsStr, root: &Path) -> Result<PathBuf, String> {
    let out = Command::new(rustc)
        .args(["--print", "target-libdir"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("running rustc: {e}"))?;
    let libdir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    Ok(libdir.with_file_name("bin").join("rust-lld"))
}

/// Join the staticlib `archive` with WASI's libc and `libsetjmp` into the
/// relocatable object `out`.
pub fn prelink(
    rustc: &OsStr,
    root: &Path,
    archive: &Path,
    sysroot: &Path,
    target: &str,
    out: &Path,
) -> Result<(), String> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let lld = lld(rustc, root)?;
    let status = Command::new(&lld)
        .args(["-flavor", "wasm", "-r", "-o"])
        .arg(out)
        .arg("--whole-archive")
        .arg(archive)
        .arg("--no-whole-archive")
        .arg(format!("-L{}", sysroot.join("lib").join(target).display()))
        .args(["-lc", "-lsetjmp"])
        .status()
        .map_err(|e| format!("running {}: {e}", lld.display()))?;
    if !status.success() {
        return Err(format!(
            "joining {} with WASI's libc: {status}",
            archive.display()
        ));
    }
    Ok(())
}
