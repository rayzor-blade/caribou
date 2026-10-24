//! A Haxe program cancels the threads it started in a scope
//! (`core.Scope`): their sleeps end at once and they leave, while a thread
//! started outside the scope sleeps as it would.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn a_haxe_program_cancels_a_scope() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    // Interpreted, so the threads run on the program's world.
    let mut session = Session::open(
        &fixtures.join("scope.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens");
    let start = Instant::now();
    let output = captured(|| session.start().expect("main runs"));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(output, "2\na cancelled,b cancelled,c slept\n");
}
