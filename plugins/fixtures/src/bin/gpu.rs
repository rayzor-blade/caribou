use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};

fn main() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("gpu");
    println!("fixture: {}", fixture.display());

    let mut session = Session::open(
        &fixture.join("gpu.hl"),
        Options {
            mode: Mode::Hybrid,
            ..Options::default()
        },
    )
    .expect("the program opens with its plugin");

    session.start().expect("main runs")
}
