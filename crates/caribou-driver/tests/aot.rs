//! A Haxe program built ahead of time for wasm, against caribou's runtime:
//! the module Ash's linker writes, with the entry points a host calls.
//! Needs the `llvm` feature, which is what builds ahead of time.
#![cfg(feature = "llvm")]

use std::path::PathBuf;

#[test]
fn a_program_builds_to_a_wasm_module_on_caribous_runtime() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let dir = std::env::temp_dir().join(format!("caribou-aot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("plain.wasm");
    let written = caribou_driver::aot::build(&fixtures.join("plain.hl"), "wasm32-wasip1", Some(&out), &[], None)
        .unwrap_or_else(|e| panic!("{e:#}"));
    let module = std::fs::read(&written).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(module.starts_with(b"\0asm"), "a wasm module");
    let has = |name: &[u8]| module.windows(name.len()).any(|w| w == name);
    assert!(has(b"ash_module_init"), "the entry a host calls");
    assert!(has(b"caribou_runtime"), "linked against caribou's runtime");
}
