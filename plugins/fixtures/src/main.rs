use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};

/// Where the build script put the test plugins.
fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("OUT_DIR")).join("plugins/debug")
}

fn main() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("window");
    println!("fixture: {}", fixture.display());
    let plugins = caribou_plugin::load_dir(&plugin_dir()).expect("the plugins load");
    assert_eq!(plugins.len(), 1, "{:?}", plugin_dir());
    println!("{}", plugins[0].name());

    let mut session = Session::open(
        &fixture.join("window.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens with its plugin");

    session.start().expect("main runs")
}
