use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};

fn main() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("window_gpu");
    println!("fixture: {}", fixture.display());
    let args = std::env::args().skip(1).collect();

    let mut session = Session::open(
        &fixture.join("window_gpu.hl"),
        Options {
            mode: Mode::Hybrid,
            args,
            ..Options::default()
        },
    )
    .expect("the program opens with both plugins");

    session.start().expect("the triangle runs")
}
