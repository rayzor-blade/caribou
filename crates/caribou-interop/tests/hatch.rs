//! A hatch package works on the Wren side as it does under wlift: the
//! project's `hatchfile` names it, the driver resolves it as `hatch`
//! does and stages it in the VM, and `import "@hatch:greet"` finds it.
//! A bundle carries the package whole, and a session from the bundle
//! stages it the same way.

use std::path::PathBuf;

use caribou::bundle::SectionKind;
use caribou_ash::Mode;
use caribou_driver::{Options, Session};
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

const USE: &str = r#"
import "@hatch:greet" for Greet
System.print(Greet.hello("ada"))
"#;

#[test]
fn a_hatch_package_is_imported_as_it_is() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let root = fixtures.join("hatch/src");

    let packages = caribou_wren::hatch::dependencies(&[&root]).expect("the hatchfile resolves");
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].name, "@hatch:greet");
    assert!(wren_lift::hatch::looks_like_hatch(&packages[0].bytes));

    // A bundle built from the project carries the package whole.
    let bundle = caribou_driver::bundle::build(&fixtures.join("hud.hl"), &[root])
        .expect("the project bundles");
    let package = bundle
        .sections
        .iter()
        .find(|s| s.kind == SectionKind::Module && s.format == "hatch")
        .expect("the package is a section");
    assert_eq!((package.lang.as_str(), package.name.as_str()), ("wren", "@hatch:greet"));
    assert!(wren_lift::hatch::looks_like_hatch(&package.data));

    // A session from the bundle stages it: the import finds it.
    let dir = std::env::temp_dir().join(format!("caribou-hatch-{}", std::process::id()));
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
    let vm = session.wren();
    vm.output_buffer = Some(String::new());
    let result = caribou_wren::with_vm(vm, |vm| vm.interpret("use", USE));
    assert_eq!(result, InterpretResult::Success);
    assert_eq!(vm.take_output(), "hello, ada\n");
    drop(session);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without a session: the packages held, then staged once the VM
/// exists, as the driver does it.
#[test]
fn a_held_package_stages_into_a_fresh_vm() {
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/hatch/src");
    for package in caribou_wren::hatch::dependencies(&[&root]).expect("resolves") {
        caribou_wren::hatch::hold(package);
    }
    let mut config = VMConfig {
        execution_mode: ExecutionMode::Interpreter,
        gc_strategy: GcStrategy::Immix,
        ..VMConfig::default()
    };
    caribou_wren::import::configure(&mut config);
    let mut vm = VM::new(config);
    vm.krio_fiber_active = true;
    caribou_wren::hatch::stage(&mut vm).expect("stages");
    vm.output_buffer = Some(String::new());
    let result = caribou_wren::with_vm(&mut vm, |vm| vm.interpret("use", USE));
    assert_eq!(result, InterpretResult::Success);
    assert_eq!(vm.take_output(), "hello, ada\n");
}
