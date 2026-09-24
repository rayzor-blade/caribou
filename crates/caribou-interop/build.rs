//! Builds the test plugins as the libraries the tests load: a cdylib is
//! no dependency cargo links, so each is built here, into a target
//! directory of its own under `OUT_DIR` (the outer build holds the
//! workspace's), and the tests find it by `OUT_DIR`.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let plugins = manifest_dir.join("plugins");
    println!("cargo:rerun-if-changed={}", plugins.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../caribou_abi/src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("../caribou_abi_derive/src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir
            .join("../../plugins/cb_window/src/events.rs")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir
            .join("../../plugins/cb_window/src/events")
            .display()
    );
    for (package, directory) in [
        ("caribou-plugin-math", "plugins"),
        ("caribou-plugin-window-events", "window-events"),
    ] {
        let status = Command::new(std::env::var("CARGO").unwrap())
            .args(["build", "-p", package, "--target-dir"])
            .arg(out_dir.join(directory))
            .current_dir(&manifest_dir)
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTFLAGS")
            // Under `cargo clippy` the outer build's lints would otherwise
            // run on this nested build too, with the outer `-D warnings`.
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("CLIPPY_ARGS")
            .status()
            .expect("cargo runs");
        assert!(status.success(), "the test plugins build");
    }
}
