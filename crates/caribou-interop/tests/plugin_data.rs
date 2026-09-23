use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn buffers_and_enums_cross_as_native_haxe_values() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    // The library the program was built against, beside it as at build
    // time; the fixture's plugins/ is not checked in.
    let library = format!(
        "{}caribou_plugin_math.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    );
    let plugins = fixtures.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::copy(
        PathBuf::from(env!("OUT_DIR"))
            .join("plugins/debug")
            .join(&library),
        plugins.join(&library),
    )
    .unwrap();

    let mut session = Session::open(
        &fixtures.join("data.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens with its plugin");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(output, "data ok\n");
}
