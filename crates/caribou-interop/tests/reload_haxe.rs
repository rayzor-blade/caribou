//! A Haxe program reloads under the world: its file is rebuilt with a body
//! changed, the session reloads it, and the next call from outside reaches
//! the new body, through the code the tier had compiled for the old one. A
//! rebuild that changes the program's shape is refused and changes nothing.
//! The rebuild here is the fixture's bytecode with one constant edited, so
//! the test needs no Haxe compiler.

use std::path::{Path, PathBuf};

use ash_core::bytecode::BytecodeDecoder;
use ash_core::bytecode_encode::encode;
use caribou::world::{Event, EventKind};
use caribou_abi::Value;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use caribou_interop::captured;
use std::cell::RefCell;
use std::rc::Rc;

/// Write `program` back as `path`, at the version the file has.
fn rebuild(path: &Path, program: &ash_core::bytecode::DecodedBytecode) {
    let version = std::fs::read(path).unwrap()[3] as usize;
    std::fs::write(path, encode(program, version).unwrap()).unwrap();
}

#[test]
fn a_haxe_program_reloads_under_the_world() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let dir = std::env::temp_dir().join(format!("caribou-reload-haxe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("reload.hl");
    std::fs::copy(fixtures.join("reload.hl"), &program).unwrap();

    let mut session = Session::open(
        &program,
        Options {
            mode: Mode::Hybrid,
            ..Options::default()
        },
    )
    .expect("the program opens");
    let output = captured(|| session.start().expect("main runs"));
    assert_eq!(output, "42\n");
    let call = |session: &mut Session, member: &str, args: &[Value]| {
        session
            .call("haxe", "UseReload", "UseReload", member, args)
            .unwrap_or_else(|e| {
                panic!("{member}: {}", unsafe {
                    caribou::error::Error::message_str(e.as_object().unwrap() as *const _)
                })
            })
            .as_int()
    };
    // Enough calls for the tier to compile `answer`: the compile lands
    // from a broker thread, so call until the report lists it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        assert_eq!(call(&mut session, "warm", &[Value::int(20_000)]), Some(840_000));
        let compiled = caribou_ash::report::compiled(session.program());
        if compiled.iter().any(|f| f.name == "UseReload.answer") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "answer never compiled: {compiled:?}");
    }
    assert_eq!(call(&mut session, "answer", &[]), Some(42));

    let heard = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&heard);
    session.world().on(EventKind::Reload, move |event| {
        sink.borrow_mut().push(event.clone());
    });

    // Rebuilt with the constant changed: 41 + 1 becomes 51 + 1.
    let mut edited = BytecodeDecoder::decode(&program).unwrap();
    let slot = edited.ints.iter().position(|&v| v == 41).expect("the constant");
    edited.ints[slot] = 51;
    rebuild(&program, &edited);
    session.reload("haxe", "UseReload").expect("the program reloads");
    assert!(heard.borrow().iter().any(|event| matches!(
        event,
        Event::Reload { module, error: None, .. } if module == "UseReload"
    )));
    // The compiled body went back to the interpreter, which runs the new
    // one; the tier compiles it again from the new program.
    assert_eq!(call(&mut session, "answer", &[]), Some(52));
    assert_eq!(call(&mut session, "warm", &[Value::int(20_000)]), Some(1_040_000));
    assert_eq!(call(&mut session, "answer", &[]), Some(52));

    // A rebuild of another shape is refused, and the program stays.
    let mut reshaped = BytecodeDecoder::decode(&program).unwrap();
    reshaped.globals.push(reshaped.globals[0].clone());
    rebuild(&program, &reshaped);
    let refused = session.reload("haxe", "UseReload");
    assert!(refused.is_err(), "{refused:?}");
    assert!(
        format!("{:#}", refused.unwrap_err()).contains("globals count"),
        "names the change"
    );
    assert_eq!(call(&mut session, "answer", &[]), Some(52));
    let _ = std::fs::remove_dir_all(&dir);
}
