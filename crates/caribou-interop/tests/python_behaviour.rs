//! A Haxe engine keeps a Python behaviour object per entity: made by the
//! class the build macro emitted, its method called each frame with a
//! collection between frames, its field read back, and each object its
//! own.

use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn a_haxe_engine_steps_a_python_object_per_entity() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/behaviour");
    let mut session = Session::open(
        &fixtures.join("behaviour.hl"),
        Options {
            mode: Mode::Hybrid,
            ..Options::default()
        },
    )
    .expect("the program opens");
    let output = captured(|| session.start().expect("main runs"));
    // Each mover's x is its speed times three half-steps: 1.5 × (0 + … + 99).
    assert_eq!(output, "7425\ntrue\n");
}
