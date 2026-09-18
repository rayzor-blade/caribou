//! A Haxe program reaches a plugin as it reaches any class: the build
//! macro found the plugin's library in `plugins/` beside the program and
//! emitted `math.Math` and `math.Vec2`; the session loads the same
//! library at run time, and every call binds by name through the
//! registry. A plugin object is a Haxe object of its class.

use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn a_haxe_program_reaches_a_plugin() {
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
        &fixtures.join("plugin.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens with its plugin");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(
        output,
        "5\n42\ntrue\n5\n10 6\n1\ntrue true\ncaught Can't cast String to math.Vec2\n"
    );
}
