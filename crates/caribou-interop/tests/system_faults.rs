//! A system another language registers with a Haxe class raises: Wren's
//! `Fiber.abort`, Lua's `error` and Python's `raise` each reach the Haxe
//! catch around the call, as a value carrying its message, and the engine
//! goes on. Interpreted Haxe, where a function the engine holds is called
//! by the interpreter.

use std::path::PathBuf;

use caribou::bridge;
use caribou::registry;
use caribou::world::LANG_CORE;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn a_raising_system_reaches_the_haxe_catch() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/systems");
    let mut session = Session::open(
        &fixtures.join("systems.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens");
    captured(|| session.start().expect("main runs"));
    session
        .call("game", "system_wren", "SystemWren", "installFailing", &[])
        .unwrap_or_else(|e| panic!("installFailing: {}", bridge::describe(e)));
    for module in ["system_lua", "system_py"] {
        let iface = registry::lookup_or_load("game", module)
            .unwrap_or_else(|e| panic!("{module}: {e}"))
            .unwrap_or_else(|| panic!("game has {module}"));
        let install = iface
            .functions
            .iter()
            .find(|f| f.name == "installFailing")
            .unwrap_or_else(|| panic!("{module} publishes installFailing"));
        bridge::call(install.target, &[], LANG_CORE)
            .unwrap_or_else(|e| panic!("{module}.installFailing: {}", bridge::describe(e)));
    }
    let faults = session
        .call("haxe", "game.Systems", "Systems", "faults", &[])
        .unwrap_or_else(|e| panic!("faults: {}", bridge::describe(e)));
    let faults = unsafe { caribou::error::Str::text(faults) }
        .unwrap()
        .to_owned();
    for message in ["wren failed", "lua failed", "python failed"] {
        assert!(faults.contains(message), "{message} in {faults:?}");
    }
}
