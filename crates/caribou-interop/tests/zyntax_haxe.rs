//! A Haxe program reaches a Zyntax module as it reaches any class: the
//! build macro described the classpath root, found the ZynML module
//! beside the ZynML snapshot and emitted `game.scorer.Scorer` from its
//! HIR; the session finds the same snapshot under the root, registers
//! ZynML as a language, and every call binds by name through the
//! registry to the compiled function.

use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn a_haxe_program_reaches_a_zynml_module() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    // The frontend under the root, as the build had it.
    std::fs::write(fixtures.join("src/zynml.zsnap"), zynml::snapshot_bytes()).unwrap();
    let mut session = Session::open(
        &fixtures.join("zynml.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens with its frontend");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(
        output,
        "64\n5\ntrue false\ngoal\n12\ncaught the function returns a Zyntax type the core does not pass yet\n"
    );
}
