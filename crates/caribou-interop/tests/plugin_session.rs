//! A session finds the plugins in `plugins/` beside what it opens, a
//! program or a bundle, and registers them as languages of its world
//! before the program starts.

use std::path::PathBuf;

use caribou::registry;
use caribou::world::LANG_CORE;
use caribou::{bridge, world};
use caribou_abi::Value;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn a_session_loads_the_plugins_beside_its_program() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let dir = std::env::temp_dir().join(format!("caribou-plugin-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("plugins")).unwrap();
    let bundle = caribou_driver::bundle::build(&fixtures.join("hud.hl"), &[fixtures.join("src")])
        .expect("the project bundles");
    let program = dir.join("hud.cb");
    std::fs::write(&program, caribou::bundle::emit(&bundle)).unwrap();
    let library = format!(
        "{}caribou_plugin_math.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    );
    std::fs::copy(
        PathBuf::from(env!("OUT_DIR"))
            .join("plugins/debug")
            .join(&library),
        dir.join("plugins").join(&library),
    )
    .unwrap();

    let mut session = Session::open(
        &program,
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the bundle opens with its plugins");
    let math = world::language_id("math").expect("the plugin is a language");
    assert_eq!(session.world().language("math"), Some(math));
    let (iface, index) = registry::lookup_class("math", "Math", "Math").expect("published");
    let hypot = iface.classes[index]
        .methods
        .iter()
        .find(|m| m.name == "hypot")
        .map(|m| m.target)
        .expect("hypot");
    let output = captured(|| session.start().expect("main runs"));
    assert!(output.starts_with("7\n10\n"), "{output}");
    let five = bridge::call(hypot, &[Value::number(3.0), Value::number(4.0)], LANG_CORE)
        .expect("the plugin answers");
    assert_eq!(five.as_number(), Some(5.0));
    let _ = std::fs::remove_dir_all(&dir);
}
