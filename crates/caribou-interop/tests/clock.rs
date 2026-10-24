//! A Haxe program steps the world's clock (`core.Clock`): its threads'
//! sleeps end when it moves the clock on, the earlier deadline first,
//! while main's own sleeps keep wall time and run them.

use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn a_haxe_program_steps_the_worlds_clock() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    // Interpreted, so the threads run on the program's world, whose clock
    // it steps; a compiled body would go to a pool worker's world.
    let mut session = Session::open(
        &fixtures.join("clock.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(output, "0 0\nfirst\nfirst,late\ntrue\nfalse\n");
}
