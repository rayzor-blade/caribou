//! The driver runs the two-language fixture as a project: nothing is
//! configured. The program's imports and the class paths of the `.hxml`
//! beside it give the world its namespaces and source roots, the Wren
//! module loads on the program's first use of it, and it imports a class
//! of the program in turn.

use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_interop::captured;

const EXPECTED: &str = "7\n10\nhp: 10\ntrue\n10\ncaught boom\nada\n5\n42\n3\n6\n";

#[test]
fn a_project_runs_with_nothing_configured() {
    let program = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/hud.hl");
    let mut result = None;
    let output = captured(|| {
        result = Some(caribou_driver::run(
            &program,
            caribou_driver::Options {
                mode: Mode::Interp,
                ..caribou_driver::Options::default()
            },
        ));
    });
    result.unwrap().expect("the program runs");
    assert_eq!(output, EXPECTED);
}
