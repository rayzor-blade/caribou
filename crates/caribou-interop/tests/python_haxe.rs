//! A Haxe program sees of a Python module the classes it declares: the
//! build macro described the classpath root, found `game/tally.py`
//! through the Python frontend and emitted `game.tally.Tally` for its
//! class, under the module as the package; the session registers Python
//! because a root holds a `.py` file. The module's functions are the
//! module's own, not types, and Haxe does not see them.

use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn a_haxe_program_sees_a_python_modules_class() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut session = Session::open(
        &fixtures.join("python.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens with the Python frontend");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(output, "game.tally.Tally\n");
}
