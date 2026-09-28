//! Builds the test plugins as the libraries the tests load: a cdylib is
//! no dependency cargo links, so each is built here, into a target
//! directory of its own (the outer build holds the workspace's), which the
//! tests find by `CARIBOU_TEST_PLUGINS`.

use std::path::{Path, PathBuf};
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
    let nested = nested_target(&out_dir, "caribou-interop");
    println!("cargo:rustc-env=CARIBOU_TEST_PLUGINS={}", nested.display());
    for (package, directory) in [
        ("caribou-plugin-math", "plugins"),
        ("caribou-plugin-window-events", "window-events"),
    ] {
        let status = Command::new(std::env::var("CARGO").unwrap())
            .args(["build", "-p", package, "--target-dir"])
            .arg(nested.join(directory))
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
