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
    let target_dir = out_dir.join("plugins");
    let status = Command::new(std::env::var("CARGO").unwrap())
        .args(["build", "-p", "caribou-plugin-math", "--target-dir"])
        .arg(&target_dir)
        .current_dir(&manifest_dir)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .status()
        .expect("cargo runs");
    assert!(status.success(), "the test plugins build");
}
