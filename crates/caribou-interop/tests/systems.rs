//! Systems other languages register with a Haxe class: a Wren Fn, a Lua
//! function and a Python function, each kept by the Haxe class as a typed
//! function and called once a step with a buffer it writes and the step's
//! time, a collection running after each step. The shape an engine's script
//! systems take (ChimpMonk's `scene.system`).

use std::path::PathBuf;

use caribou::bridge;
use caribou::registry;
use caribou::world::LANG_CORE;
use caribou_abi::Value;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn systems_of_other_languages_run_from_haxe_over_their_buffers() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/systems");
    let mut session = Session::open(
        &fixtures.join("systems.hl"),
        Options {
            mode: Mode::Hybrid,
            ..Options::default()
        },
    )
    .expect("the program opens");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(output, "game.Systems\n");

    let call = |session: &mut Session, at: (&str, &str, &str), member: &str, args: &[Value]| {
        let (namespace, module, class) = at;
        session
            .call(namespace, module, class, member, args)
            .unwrap_or_else(|e| {
                panic!(
                    "{class}.{member}: {}",
                    caribou::diag::render_string(
                        &caribou::diag::report(e),
                        &caribou::diag::NoSources,
                        false
                    )
                )
            })
    };
    // Each language's module registers its system: Wren's from a class's
    // static, Lua's and Python's from a function of the module.
    call(
        &mut session,
        ("game", "system_wren", "SystemWren"),
        "install",
        &[],
    );
    for module in ["system_lua", "system_py"] {
        let iface = registry::lookup_or_load("game", module)
            .unwrap_or_else(|e| panic!("{module}: {e}"))
            .unwrap_or_else(|| panic!("game has {module}"));
        let install = iface
            .functions
            .iter()
            .find(|f| f.name == "install")
            .unwrap_or_else(|| panic!("{module} publishes install"));
        bridge::call(install.target, &[], LANG_CORE)
            .unwrap_or_else(|e| panic!("{module}.install: {}", bridge::describe(e)));
    }
    let systems = ("haxe", "game.Systems", "Systems");
    assert_eq!(call(&mut session, systems, "count", &[]).as_int(), Some(3));
    let ran = call(&mut session, systems, "run", &[Value::int(3)]);
    let ran = unsafe { caribou::error::Str::text(ran) }
        .unwrap()
        .to_owned();
    assert_eq!(
        ran, "3:0.5,3:0.5,3:0.5",
        "each column counts its steps and holds dt"
    );
}
