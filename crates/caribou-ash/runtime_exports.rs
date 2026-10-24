//! For a build script that links a binary with ash_std in it: the link
//! arguments that keep ash_std's own exports out of the binary's dynamic
//! symbols. Binaries here export their symbols (`.cargo/config.toml`), and
//! a program with HDLLs runs through the runtime image staged beside the
//! binary, which caribou installs its heap and scheduler into. An HDLL that
//! found the binary's copy of a runtime function first would run it on
//! Ash's own scheduler and heap instead.

use std::path::Path;

/// ash_std's exports, by prefix (`*`) or name.
const EXPORTS: &[&str] = &[
    "hlp_*",
    "hl_*",
    "hlt_*",
    "ash_*",
    "buffer_*",
    "__fun_*",
    "ustrlen",
    "ustrdup",
    "usprintf",
    "uprintf",
    "uvszprintf",
    "ucmp",
];

/// Emit the link arguments for `targets` (`bins`, `tests`, `benches`);
/// a Linux version script is written under `out`.
pub fn hide(out: &Path, targets: &[&str]) {
    let mut args = Vec::new();
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            for name in EXPORTS {
                args.push(format!("-Wl,-unexported_symbol,_{name}"));
            }
        }
        Ok("linux") => {
            let script = out.join("ash-runtime-exports.map");
            let local: String = EXPORTS.iter().map(|name| format!("{name}; ")).collect();
            std::fs::write(&script, format!("{{ global: *; local: {local}}};\n"))
                .expect("the version script is written");
            args.push(format!("-Wl,--version-script={}", script.display()));
        }
        _ => {}
    }
    for target in targets {
        for arg in &args {
            println!("cargo:rustc-link-arg-{target}={arg}");
        }
    }
}
