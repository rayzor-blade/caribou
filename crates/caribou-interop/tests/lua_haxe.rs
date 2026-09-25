//! A Haxe program using a Lua class: the build macro described the
//! classpath root, where the Lua module `game/counter.lua` returns the
//! class `Counter`, and emitted `game.counter.Counter` for it from the
//! chunk's types and its LuaLS annotations, without running it. The
//! program makes an instance, calls its methods, reads and writes its
//! fields and the class's static, hands Lua a Haxe function to call and
//! a buffer Lua reads in place, calls a Lua function Lua returns, and
//! takes an instance a method returns as a `Counter`.

use std::path::PathBuf;

use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn a_haxe_program_uses_a_lua_class() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/lua");
    let mut session = Session::open(
        &fixtures.join("lua.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens with the Lua frontend");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(output, "3\n7\n10\n10\n15\n30\ncount\n294\n7\n4\n");
}
