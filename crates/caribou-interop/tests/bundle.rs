//! A bundle built from the project runs as the project did: the program
//! from memory, its Wren modules compiled, from the bundle's sections,
//! its namespaces from the manifest, with no source root in sight.

use std::path::PathBuf;

use caribou::bundle::SectionKind;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

const OUTPUT: &str = "7\n10\nhp: 10\n1 true true true\ntrue\ntrue\n10\ncaught boom\nada\n5\n42\n3\n6\n3 hp\nhp,mp,4\n6.5\n3 10\n";

#[test]
fn a_bundle_runs_as_the_project_did() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let bundle = caribou_driver::bundle::build(&fixtures.join("hud.hl"), &[fixtures.join("src")])
        .expect("the project bundles");
    assert_eq!(bundle.manifest.name, "hud");
    assert_eq!(bundle.manifest.entry.module, "hud");
    let names: Vec<&str> = bundle
        .manifest
        .namespaces
        .iter()
        .map(|n| n.name.as_str())
        .collect();
    assert!(names.contains(&"game"), "{names:?}");
    let modules: Vec<(&str, &str, &str)> = bundle
        .sections
        .iter()
        .filter(|s| s.kind == SectionKind::Module)
        .map(|s| (s.lang.as_str(), s.format.as_str(), s.name.as_str()))
        .collect();
    let wlbc = caribou_wren::project::WLBC.as_str();
    assert!(wlbc.starts_with("wlbc@"), "{wlbc}");
    assert!(modules.contains(&("haxe", "hl", "hud")), "{modules:?}");
    assert!(modules.contains(&("wren", wlbc, "game/hud")), "{modules:?}");
    assert!(
        modules.contains(&("wren", wlbc, "bench/tally")),
        "{modules:?}"
    );
    let sources: Vec<&str> = bundle
        .sections
        .iter()
        .filter(|s| s.kind == SectionKind::Source && s.lang == "wren")
        .map(|s| s.name.as_str())
        .collect();
    assert!(sources.contains(&"game/hud"), "{sources:?}");
    assert!(sources.contains(&"game/format"), "{sources:?}");

    // Written where nothing else is, and opened from there.
    let dir = std::env::temp_dir().join(format!("caribou-bundle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hud.cb");
    std::fs::write(&path, caribou::bundle::emit(&bundle)).unwrap();

    let mut session = Session::open(
        &path,
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the bundle opens");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(output, OUTPUT);
    let output = captured(|| {
        if let Err(e) = session.call("haxe", "UseHud", "UseHud", "after", &[]) {
            panic!("after: {}", unsafe {
                caribou::error::Error::message_str(e.as_object().unwrap() as *const _)
            });
        }
    });
    assert_eq!(output, "after: 10\n12\nscore 10\n");

    // Each compiled module has its text with it, for its errors: the one
    // Haxe loaded and the sibling it imported.
    let hud = std::fs::read_to_string(fixtures.join("src/game/hud.wren")).unwrap();
    let format = std::fs::read_to_string(fixtures.join("src/game/format.wren")).unwrap();
    let sources = &session.wren().module_sources;
    assert_eq!(sources.get("game/hud"), Some(&hud));
    assert_eq!(sources.get("game/format"), Some(&format));
    let _ = std::fs::remove_dir_all(&dir);
}
