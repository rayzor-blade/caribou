//! Native window payloads use the production schemas without opening a GUI.
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;
use std::path::PathBuf;

#[test]
fn window_events_and_scale_callback_cross_haxe() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/window_events");
    let library = format!(
        "{}caribou_plugin_window_events.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    );
    let plugins = fixtures.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::copy(
        PathBuf::from(env!("OUT_DIR"))
            .join("window-events/debug")
            .join(&library),
        plugins.join(library),
    )
    .unwrap();
    let mut session = Session::open(
        &fixtures.join("events.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("window schemas load");
    assert_eq!(
        captured(|| session.start().expect("events round trip")),
        "window events ok\n"
    );
}
