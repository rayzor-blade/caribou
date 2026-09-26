//! A program built ahead of time: Ash's AOT build of the `.hl`, linked
//! against caribou's runtime in place of Ash's own, so the program runs on
//! the core's heap and scheduler with every adapter present. Nothing is
//! interpreted and nothing loads at run time.
//!
//! Only wasm for now: the wasm runtime object is built with the driver
//! (`build.rs`), and a release puts it beside the binary as Ash's does.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use ash_core::llvm::aot_build::{AotRequest, emit_aot};
use ash_core::llvm::aot_link::is_wasm_triple;

/// Build `program` for `triple` into `out`, by default the program's name
/// with the target's extension beside it. Returns what was written.
pub fn build(program: &Path, triple: &str, out: Option<&Path>) -> Result<PathBuf> {
    if !is_wasm_triple(triple) {
        bail!("`{triple}`: caribou builds wasm programs ahead of time so far");
    }
    let runtime = wasm_runtime(triple)?;
    let exe = out.map_or_else(|| program.with_extension("wasm"), Path::to_path_buf);
    // Scratch, named after the module beside it and removed once linked.
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".o");
    let object = exe.with_file_name(name);
    emit_aot(AotRequest {
        file: program,
        out: &object,
        exe: Some(&exe),
        runtime: Some(&runtime),
        target: Some(triple.to_owned()),
        pgo: None,
        allow_refused: false,
        abi_version: 1,
        quiet: false,
    })?;
    Ok(exe)
}

/// Caribou's runtime object for `triple`: beside the binary, where a
/// release puts it, else the one this build of the driver made.
fn wasm_runtime(triple: &str) -> Result<PathBuf> {
    const NAME: &str = "caribou_runtime.o";
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(triple).join(NAME)));
    let built = option_env!("CARIBOU_WASM_RUNTIME").map(PathBuf::from);
    beside
        .into_iter()
        .chain(built)
        .find(|p| p.is_file())
        .ok_or_else(|| {
            anyhow!(
                "no {NAME} for {triple}: this caribou was built without a WASI sysroot. \
                 Install wasi-libc or the WASI SDK (or set WASI_SYSROOT) and rebuild it"
            )
        })
}
