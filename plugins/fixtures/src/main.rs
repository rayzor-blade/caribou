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

    let mut session = Session::open(
        &fixture.join("window.hl"),
        Options {
            mode: Mode::Hybrid,
            ..Options::default()
        },
    )
    .expect("the program opens with its plugin");

    session.start().expect("main runs")
}
