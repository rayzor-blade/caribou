//! Building a runtime object for a wasm target: a staticlib built with
//! cargo from caribou's source, joined with WASI's libc and `libsetjmp`
//! into one relocatable object, as Ash's own wasm runtime is. The build
//! script makes the driver's runtime this way, and `aot` a program's own
//! when it links plugins in.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use caribou_wasm_toolchain::{cflags_are_configured, clang, compiler_is_configured};
pub use caribou_wasm_toolchain::{library_dir, sysroot};

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
        // What an enclosing cargo run sets for its own build.
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CLIPPY_ARGS")
        .env("CARGO_INCREMENTAL", "0");
    if !cflags_are_configured(target) {
        build.env(
            format!("CFLAGS_{}", target.replace('-', "_")),
            format!("--target={target} --sysroot={}", sysroot.display()),
        );
    }
    if !compiler_is_configured(target) {
        if let Some(clang) = clang(target) {
            build.env(format!("CC_{}", target.replace('-', "_")), clang);
        }
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
    let library_dir = library_dir(sysroot, target).ok_or_else(|| {
        format!(
            "{} has no libc and libsetjmp for {target}",
            sysroot.display()
        )
    })?;
    let status = Command::new(&lld)
        .args(["-flavor", "wasm", "-r", "-o"])
        .arg(out)
        .arg("--whole-archive")
        .arg(archive)
        .arg("--no-whole-archive")
        .arg(format!("-L{}", library_dir.display()))
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

/// Join relocatable objects into one, `out`: a runtime object and what
/// the program links beside it.
pub fn join(rustc: &OsStr, root: &Path, objects: &[&Path], out: &Path) -> Result<(), String> {
    let lld = lld(rustc, root)?;
    let status = Command::new(&lld)
        .args(["-flavor", "wasm", "-r", "-o"])
        .arg(out)
        .args(objects)
        .status()
        .map_err(|e| format!("running {}: {e}", lld.display()))?;
    if !status.success() {
        return Err(format!("joining the program's objects: {status}"));
    }
    Ok(())
}

/// Link the PIC staticlib `archive` as a `dylink.0` side module at `out`,
/// exporting `exports` and nothing else; what it leaves undefined it
/// imports from the program it loads into.
pub fn side_module(
    rustc: &OsStr,
    root: &Path,
    archive: &Path,
    exports: &[String],
    out: &Path,
) -> Result<(), String> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let lld = lld(rustc, root)?;
    let mut link = Command::new(&lld);
    link.args([
        "-flavor",
        "wasm",
        "--experimental-pic",
        "-shared",
        "--unresolved-symbols=import-dynamic",
        "--no-entry",
        "--gc-sections",
        "--no-export-dynamic",
    ]);
    for symbol in exports {
        link.arg(format!("--export={symbol}"));
    }
    let status = link
        .arg("--whole-archive")
        .arg(archive)
        .arg("--no-whole-archive")
        .arg("-o")
        .arg(out)
        .status()
        .map_err(|e| format!("running {}: {e}", lld.display()))?;
    if !status.success() {
        return Err(format!(
            "linking the side module {}: {status}",
            out.display()
        ));
    }
    Ok(())
}
