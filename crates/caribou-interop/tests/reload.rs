//! A Wren module reloads under a running Haxe program: the source changes
//! on disk, the session reloads the module, and the Haxe program's next
//! call into a Wren object it has kept since before reaches the new body,
//! through the call site it filled before. The world's subscribers hear of
//! the reload.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::world::{Event, EventKind};
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

const BEFORE: &str = "7\n10\nhp: 10\n1 true true true\ntrue\ntrue\n10\ncaught boom\nada\n5\n42\n3\n6\n3 hp\nhp,mp,4\n6.5\n3 10\n";

#[test]
fn a_wren_module_reloads_under_a_haxe_program() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let roots = std::env::temp_dir().join(format!("caribou-reload-{}", std::process::id()));
    let module = roots.join("game").join("hud.wren");
    std::fs::create_dir_all(module.parent().unwrap()).unwrap();
    let source = std::fs::read_to_string(fixtures.join("src/game/hud.wren")).unwrap();
    std::fs::write(&module, &source).unwrap();

    let mut session = Session::open(
        &fixtures.join("hud.hl"),
        Options {
            mode: Mode::Interp,
            roots: vec![roots.clone()],
            ..Options::default()
        },
    )
    .expect("the program opens");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(output, BEFORE);

    // The Haxe program's own call into the object it kept, filling the
    // site's cache.
    let after = || ("haxe", "UseHud", "UseHud", "after");
    let output = captured(|| {
        let (ns, module, class, member) = after();
        session
            .call(ns, module, class, member, &[])
            .expect("after runs");
    });
    assert_eq!(output, "after: 10\n12\n");

    let heard = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&heard);
    session.world().on(EventKind::Reload, move |event| {
        sink.borrow_mut().push(event.clone());
    });

    // A change on disk, then the reload.
    std::fs::write(
        &module,
        source.replace("\"%(prefix): %(_score)\"", "\"%(prefix)= %(_score)\""),
    )
    .unwrap();
    session.reload("game", "hud").expect("the module reloads");
    // The session's watch may hear the edit too: at least this one.
    assert!(heard.borrow().iter().any(|event| matches!(
        event,
        Event::Reload { module, error: None, .. } if module == "game/hud"
    )));

    // The same object, the same site, the new body.
    let output = captured(|| {
        let (ns, module, class, member) = after();
        session
            .call(ns, module, class, member, &[])
            .expect("after runs again");
    });
    assert_eq!(output, "after= 10\n12\n");

    // A module nothing loaded does not reload.
    assert!(session.reload("game", "nothing").is_err());

    let _ = std::fs::remove_dir_all(&roots);
}
