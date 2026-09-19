//! A bundle carries the Zyntax frontends under the roots and the modules
//! of their languages, and a run from it brings the languages up itself:
//! ZynML from the snapshot the bundle holds, Python from the frontend
//! built into caribou. The modules load from the bundle's sections, with
//! no source root in sight.

use std::path::PathBuf;

use caribou::bundle::SectionKind;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;
use wren_lift::runtime::engine::InterpretResult;

const USE_TALLY: &str = r#"
import "game:tally" for score
System.print(score.call(7, 2))
"#;

#[test]
fn a_bundle_carries_its_zyntax_languages() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    // The frontend under the root, as the build had it.
    std::fs::write(fixtures.join("src/zynml.zsnap"), zynml::snapshot_bytes()).unwrap();
    let bundle = caribou_driver::bundle::build(&fixtures.join("zynml.hl"), &[fixtures.join("src")])
        .expect("the project bundles");

    let languages: Vec<(&str, &str, &str)> = bundle
        .languages()
        .map(|s| (s.lang.as_str(), s.format.as_str(), s.name.as_str()))
        .collect();
    assert_eq!(
        languages,
        [("zynml", "zsnap", "zynml.zsnap"), ("python", "builtin", "")]
    );
    let modules: Vec<(&str, &str, &str)> = bundle
        .sections
        .iter()
        .filter(|s| s.kind == SectionKind::Module)
        .map(|s| (s.lang.as_str(), s.format.as_str(), s.name.as_str()))
        .collect();
    assert!(
        modules.contains(&("zynml", "source", "game/scorer.zynml")),
        "{modules:?}"
    );
    assert!(
        modules.contains(&("python", "source", "game/tally.py")),
        "{modules:?}"
    );
    let game = bundle
        .manifest
        .namespaces
        .iter()
        .find(|n| n.name == "game")
        .expect("the game namespace");
    assert!(game.langs.contains(&"zynml".to_owned()), "{:?}", game.langs);
    assert!(
        game.langs.contains(&"python".to_owned()),
        "{:?}",
        game.langs
    );

    // Written where nothing else is, and opened from there.
    let dir = std::env::temp_dir().join(format!("caribou-bundle-zyntax-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("zynml.cb");
    std::fs::write(&path, caribou::bundle::emit(&bundle)).unwrap();

    let mut session = Session::open(
        &path,
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the bundle opens");
    // Haxe reaches the ZynML module's struct from the bundle.
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(
        output,
        "12\ncaught the function returns a Zyntax type the core does not pass yet\n"
    );
    // Wren reaches the Python module's function from the bundle.
    let vm = session.wren();
    vm.output_buffer = Some(String::new());
    let result = caribou_wren::with_vm(vm, |vm| vm.interpret("use", USE_TALLY));
    let output = vm.take_output();
    assert_eq!(result, InterpretResult::Success, "{output:?}");
    assert_eq!(output, "64\n");
    let _ = std::fs::remove_dir_all(&dir);
}
