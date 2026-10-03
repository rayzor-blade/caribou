//! A DSL's effect handled by its host, from a Haxe driver: the Haxe
//! program calls into a ZynML module whose function performs `Ask`, and
//! the module's handler answers it through the Haxe class `game.Prompt`,
//! which ZynML imports as a typed host class.

use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn a_zynml_effect_is_answered_by_the_haxe_driver() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    // The frontend under the root, as the build had it.
    std::fs::write(fixtures.join("src/zynml.zsnap"), zynml::snapshot_bytes()).unwrap();
    let mut session = Session::open(
        &fixtures.join("interview.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens with its frontend");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(
        output,
        "Ada, to \"your name?\"\n1\ncaught no answer to \"anything\"\n"
    );
}
