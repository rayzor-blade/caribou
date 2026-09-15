//! A Haxe program's frame loop, installed the way a UI library installs
//! one, gives its idle time to the world: a Wren fiber counts on its
//! sleeps between the frames.

use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn a_wren_fiber_runs_between_haxe_frames() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut session = Session::open(
        &fixtures.join("frame.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens");
    // The loop ends on the error the frame function raises once done.
    let output = captured(|| session.start().expect("main and its frames run"));
    assert_eq!(output, "count 5 in time\n");
}
