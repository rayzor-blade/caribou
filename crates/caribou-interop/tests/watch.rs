//! The session watches the files its modules came from: a Wren file
//! edited while the Haxe program runs its frame loop reloads its module
//! between two frames, with no call of the program's own, and the
//! program's next call reaches the new body. The world's subscribers hear
//! of it, and a broken edit is heard as an error and changes nothing.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use caribou::world::{Event, EventKind};
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;

#[test]
fn an_edited_wren_file_reloads_between_haxe_frames() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let roots = std::env::temp_dir().join(format!("caribou-watch-{}", std::process::id()));
    let module = roots.join("game").join("tune.wren");
    std::fs::create_dir_all(module.parent().unwrap()).unwrap();
    let source = std::fs::read_to_string(fixtures.join("src/game/tune.wren")).unwrap();
    std::fs::write(&module, &source).unwrap();

    let mut session = Session::open(
        &fixtures.join("watch.hl"),
        Options {
            mode: Mode::Interp,
            roots: vec![roots.clone()],
            ..Options::default()
        },
    )
    .expect("the program opens");
    let heard = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&heard);
    session.world().on(EventKind::Reload, move |event| {
        sink.borrow_mut().push(event.clone());
    });

    // Two edits from another thread while the program runs its frames: a
    // broken one first, then the one the program is waiting for.
    let broken = source.replace("static value() { 1 }", "static value() { 1 +");
    let edited = source.replace("static value() { 1 }", "static value() { 2 }");
    let path = module.clone();
    let editor = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(&path, broken).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(&path, edited).unwrap();
    });
    let output = captured(|| session.start().expect("main and its frames run"));
    editor.join().unwrap();
    assert_eq!(output, "value 1\nvalue 2\n");

    let heard = heard.borrow();
    let reloads: Vec<(&str, bool)> = heard
        .iter()
        .map(|event| {
            let Event::Reload { module, error, .. } = event;
            (module.as_str(), error.is_some())
        })
        .collect();
    assert_eq!(reloads, [("game/tune", true), ("game/tune", false)]);

    let _ = std::fs::remove_dir_all(&roots);
}
