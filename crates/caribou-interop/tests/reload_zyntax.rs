//! A Zyntax module reloads under a running Haxe program: the source
//! changes on disk, the session reloads the module, the runtime swaps
//! the functions that changed, and the next call through the registry
//! reaches the new body. An edit that does not compile fails the reload
//! and leaves the module as it was. A Python module reloads the same
//! way, and a function of it that Wren imported before follows.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::world::{Event, EventKind};
use caribou_abi::Value;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;
use wren_lift::runtime::engine::InterpretResult;

const USE_TALLY: &str = r#"
import "game:tally" for score
var held = score
System.print(held.call(7, 2))
"#;

/// The value the first module kept, from another module.
const HELD: &str = r#"
import "use" for held
System.print(held.call(7, 2))
"#;

#[test]
fn a_zynml_module_reloads_under_a_haxe_program() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let roots = std::env::temp_dir().join(format!("caribou-reload-zyntax-{}", std::process::id()));
    let module = roots.join("game").join("scorer.zynml");
    std::fs::create_dir_all(module.parent().unwrap()).unwrap();
    let source = std::fs::read_to_string(fixtures.join("src/game/scorer.zynml")).unwrap();
    std::fs::write(&module, &source).unwrap();
    // The frontend under the root, as the build had it, and the Python
    // module beside the ZynML one.
    std::fs::write(roots.join("zynml.zsnap"), zynml::snapshot_bytes()).unwrap();
    let tally = roots.join("game").join("tally.py");
    let tally_source = std::fs::read_to_string(fixtures.join("src/game/tally.py")).unwrap();
    std::fs::write(&tally, &tally_source).unwrap();

    let mut session = Session::open(
        &fixtures.join("zynml.hl"),
        Options {
            mode: Mode::Interp,
            roots: vec![roots.clone()],
            ..Options::default()
        },
    )
    .expect("the program opens with its frontend");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(
        output,
        "12\ncaught the function returns a Zyntax type the core does not pass yet\n"
    );
    let area = |session: &mut Session| {
        session
            .call(
                "game",
                "scorer",
                "Point",
                "area",
                &[Value::number(3.0), Value::number(4.0)],
            )
            .expect("area runs")
            .as_number()
    };
    assert_eq!(area(&mut session), Some(12.0));

    let heard = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&heard);
    session.world().on(EventKind::Reload, move |event| {
        sink.borrow_mut().push(event.clone());
    });

    // A change on disk, then the reload: the new body answers.
    let doubled = source.replace("return w * h }", "return w * h * 2.0 }");
    assert_ne!(doubled, source);
    std::fs::write(&module, &doubled).unwrap();
    session
        .reload("game", "scorer")
        .expect("the module reloads");
    assert!(heard.borrow().iter().any(|event| matches!(
        event,
        Event::Reload { module, error: None, .. } if module == "game/scorer"
    )));
    assert_eq!(area(&mut session), Some(24.0));

    // An edit that does not parse fails the reload and changes nothing.
    std::fs::write(&module, doubled.replace("def area", "def area(")).unwrap();
    let failed = session.reload("game", "scorer");
    assert!(failed.is_err(), "{failed:?}");
    assert!(heard.borrow().iter().any(|event| matches!(
        event,
        Event::Reload { module, error: Some(_), .. } if module == "game/scorer"
    )));
    assert_eq!(area(&mut session), Some(24.0));

    // A module nothing loaded does not reload.
    assert!(session.reload("game", "nothing").is_err());

    // The Python module: Wren holds its function as a value from before
    // the edit, and the value follows the reload.
    let run = |session: &mut Session, module: &str, source: &str| {
        let vm = session.wren();
        vm.output_buffer = Some(String::new());
        let result = caribou_wren::with_vm(vm, |vm| vm.interpret(module, source));
        let output = vm.take_output();
        assert_eq!(result, InterpretResult::Success, "{output:?}");
        output
    };
    assert_eq!(run(&mut session, "use", USE_TALLY), "64\n");
    std::fs::write(&tally, tally_source.replace("hits * 10 -", "hits * 100 -")).unwrap();
    session
        .reload("game", "tally")
        .expect("the Python module reloads");
    assert_eq!(run(&mut session, "again", HELD), "694\n");
    let _ = std::fs::remove_dir_all(&roots);
}
