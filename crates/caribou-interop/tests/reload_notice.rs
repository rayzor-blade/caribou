//! A program hears each module's reload (`core.Modules.onReload`): before
//! the new body runs, once its source compiled, and after; a source that
//! does not compile is heard only after, as failed, with the module as it
//! was. For a Lua module, which runs, and a Wren module.

use std::path::{Path, PathBuf};

use caribou::registry;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn a_program_hears_reloads_before_and_after() {
    let dir = std::env::temp_dir().join(format!("caribou-reload-notice-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/notice"),
        &dir,
    );
    let mut session = Session::open(
        &dir.join("notice.hl"),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the program opens");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(output, "listening\n");

    // Both modules loaded before they reload.
    registry::lookup_or_load("game", "door")
        .unwrap()
        .expect("game:door");
    session
        .call("game", "gate", "Gate", "value", &[])
        .expect("game:gate loads");

    let door = dir.join("src/game/door.lua");
    let gate = dir.join("src/game/gate.wren");
    let edit = |path: &Path, from: &str, to: &str| {
        let source = std::fs::read_to_string(path).unwrap();
        std::fs::write(path, source.replace(from, to)).unwrap();
    };
    edit(&door, "return 1", "return 2");
    session.reload("game", "door").expect("the edit reloads");
    edit(&door, "return 2", "return (");
    assert!(
        session.reload("game", "door").is_err(),
        "a broken edit fails"
    );
    edit(&gate, "{ 1 }", "{ 2 }");
    session.reload("game", "gate").expect("the edit reloads");
    edit(&gate, "{ 2 }", "{ 2 ");
    assert!(
        session.reload("game", "gate").is_err(),
        "a broken edit fails"
    );

    let log = session
        .call("haxe", "UseNotice", "UseNotice", "log", &[])
        .expect("the log");
    let log = unsafe { caribou::error::Str::text(log) }
        .unwrap()
        .to_owned();
    assert_eq!(
        log,
        "game:door before,game:door after,game:door failed,game:gate before,game:gate after,game:gate failed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
