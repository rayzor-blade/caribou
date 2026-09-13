//! A Wren program imports a Haxe class with an ordinary `import`: the class
//! the loaded program published becomes a Wren class installed through
//! wren_lift's own module path, and its methods land in the bridge.
//!
//! One test, because the seams are process-global: both are installed
//! before either runtime allocates, the world registers both adapters and
//! its namespaces, the Haxe program starts and publishes, and only then do
//! the VMs run.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use caribou::bridge;
use caribou::error::Str;
use caribou::heap;
use caribou::registry::{self, Namespace};
use caribou::symbol::intern;
use caribou::world::{Config, World};
use caribou_ash::{Mode, Options};
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

const SCRIPT: &str = include_str!("../fixtures/wren/main.wren");

const EXPECTED: &str = "ada\nfalse\n70\ntrue\n7\n1\n11\n";

/// A module the host serves itself, importing through the language's own
/// namespace.
const BY_LANGUAGE: &str = r#"
import "haxe:game.Player" for Player
var ByLanguage = Player
"#;

/// The language's own namespace resolves too, to the same class.
const DEFAULT_NAMESPACE: &str = r#"
import "bylanguage" for ByLanguage
import "game:Player" for Player
System.print(Player == ByLanguage)
System.print(Player.spawnAt(1, 1) is ByLanguage)
"#;

/// A Haxe throw inside a bound method is a fiber abort with its message;
/// so is an argument Haxe cannot take. Objects come back as the installed
/// class, strings as strings, and a Wren subclass constructs through the
/// Haxe constructor.
const ERRORS_AND_SHAPES: &str = r#"
import "game:Player" for Player
var p = Player.new("bob")
var caught = Fiber.new { p.explode() }.try()
System.print(caught)
var bad = Fiber.new { p.hit("thirty") }.try()
System.print(bad is String)
System.print(Player.spawnAt(2, 2).name)
class Hero is Player {
  construct new(name) {
    super(name)
    _title = "sir"
  }
  title { _title }
}
var h = Hero.new("kay")
System.print(h.name + " " + h.title)
System.print(h.hit(100))
"#;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/game.hl")
}

/// A VM whose errors land in `errors` instead of on stderr.
fn vm(mode: ExecutionMode, errors: &Rc<RefCell<Vec<String>>>) -> VM {
    let sink = Rc::clone(errors);
    let mut config = VMConfig {
        execution_mode: mode,
        gc_strategy: GcStrategy::Immix,
        error_fn: Some(Box::new(move |_kind, _module, _line, message: &str| {
            sink.borrow_mut().push(message.to_owned());
        })),
        // The host's own loader keeps serving what is not namespaced.
        load_module_fn: Some(Box::new(|name: &str, _from: &str| {
            (name == "bylanguage").then(|| BY_LANGUAGE.to_owned())
        })),
        ..VMConfig::default()
    };
    caribou_wren::import::configure(&mut config);
    let mut vm = VM::new(config);
    vm.output_buffer = Some(String::new());
    vm
}

fn run(mode: ExecutionMode, module: &str, source: &str) -> (InterpretResult, String, Vec<String>) {
    let errors = Rc::new(RefCell::new(Vec::new()));
    let mut vm = vm(mode, &errors);
    let result = caribou_wren::with_vm(&mut vm, |vm| vm.interpret(module, source));
    let output = vm.take_output();
    drop(vm);
    let errors = errors.take();
    (result, output, errors)
}

fn drive(mode: ExecutionMode) {
    let (result, output, errors) = run(mode, "main", SCRIPT);
    assert_eq!(result, InterpretResult::Success, "{errors:?}");
    assert_eq!(output, EXPECTED);

    let (result, output, errors) = run(mode, "default_namespace", DEFAULT_NAMESPACE);
    assert_eq!(result, InterpretResult::Success, "{errors:?}");
    assert_eq!(output, "true\ntrue\n");

    let (result, output, errors) = run(mode, "errors", ERRORS_AND_SHAPES);
    assert_eq!(result, InterpretResult::Success, "{errors:?}");
    assert_eq!(output, "kaboom\ntrue\nspawned\nkay sir\ntrue\n");

    // A module no namespace holds is an import error naming it.
    let (result, _, errors) = run(mode, "missing", "import \"game:Nope\" for Nope\n");
    assert_eq!(result, InterpretResult::CompileError);
    assert!(
        errors.iter().any(|e| e.contains("game:Nope")),
        "the error names the module: {errors:?}"
    );

    strings_cross_by_value(mode);
}

/// A string crosses by value at every edge: a Wren string leaves as a core
/// `Str`, which Haxe takes as a `String`; a Haxe `String` leaves as a core
/// `Str`, which Wren takes as a Wren string.
fn strings_cross_by_value(mode: ExecutionMode) {
    let errors = Rc::new(RefCell::new(Vec::new()));
    let mut vm = vm(mode, &errors);
    assert_eq!(
        vm.interpret("strings", "var s = \"ada\"\n"),
        InterpretResult::Success
    );
    let s = vm
        .find_imported_var_from("s", "strings")
        .expect("`s` is defined");
    let (player, _) = registry::lookup_class("game", "Player", "Player").expect("published");
    caribou_wren::with_vm(&mut vm, |vm| {
        let crossed = caribou_wren::wrap(s);
        assert_eq!(unsafe { Str::text(crossed) }, Some("ada"));
        let root = heap::handle_new(crossed.as_object().unwrap() as *mut u8);
        let p = caribou_ash::construct(&player.classes[0], &[crossed]).expect("a Player");
        let p_root = heap::handle_new(p.as_object().unwrap() as *mut u8);
        let name = bridge::get(p, intern("name"), caribou_wren::lang()).expect("its name");
        assert_eq!(unsafe { Str::text(name) }, Some("ada"));
        let back = caribou_wren::unwrap(vm, name).expect("a Wren value");
        assert!(back.is_string_object());
        assert_eq!(wren_lift::runtime::core::as_string(back), "ada");
        heap::handle_release(p_root);
        heap::handle_release(root);
    });
    drop(vm);
    assert!(errors.take().is_empty());
}

#[test]
fn a_wren_program_imports_a_haxe_class() {
    // Both seams before either runtime allocates.
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");

    let mut world = World::new(Config {
        namespaces: vec![Namespace {
            name: "game".to_owned(),
            langs: vec!["haxe".to_owned()],
            modules: None,
        }],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_ash::Runtime::new()))
        .expect("haxe registers");
    world
        .register(Box::new(caribou_wren::Runtime::new()))
        .expect("wren registers");

    let mut program = caribou_ash::load(
        &fixture(),
        Options {
            mode: Mode::Interp,
            ..Options::default()
        },
    )
    .expect("the fixture loads");
    program.start().expect("its empty main runs");
    program.publish().expect("its classes publish");

    drive(ExecutionMode::Interpreter);
    drive(ExecutionMode::Tiered);
    program.finish();
}
