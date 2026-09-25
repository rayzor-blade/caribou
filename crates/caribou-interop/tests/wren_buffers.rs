//! A WrenLift typed array where a buffer is taken, and a root module in
//! Wren's own namespace.
use caribou::world::{Config, World};
use std::{cell::RefCell, path::PathBuf, rc::Rc};
use wren_lift::runtime::{
    engine::{ExecutionMode, InterpretResult},
    vm::{VM, VMConfig},
};

const BUFFERS: &str = r#"
import "math:Data" for Data
var bytes = ByteArray.fromList([1, 2, 3])
System.print(Data.sum(bytes))
var back = Data.echo(bytes)
System.print(back == bytes)
System.print(Data.same_storage(bytes, back))
Data.save(Float32Array.fromList([2.5, 4]))
System.gc()
var saved = Data.saved()
System.print(saved is Float32Array)
System.print(saved[0])
System.print(Data.sum(ByteArray.new(0)))
"#;

#[test]
fn wren_passes_typed_arrays_as_buffers_and_loads_a_root_module() {
    caribou_ash::install().unwrap();
    caribou_wren::install().unwrap();
    let library = format!(
        "{}caribou_plugin_math.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    );
    let plugin = caribou_plugin::load(
        &PathBuf::from(env!("OUT_DIR"))
            .join("plugins/debug")
            .join(library),
    )
    .unwrap();
    let roots = std::env::temp_dir().join(format!("caribou-buffers-{}", std::process::id()));
    std::fs::create_dir_all(&roots).unwrap();
    std::fs::write(
        roots.join("shading.wren"),
        "class Shading {\n  static value { 42 }\n}\n",
    )
    .unwrap();
    let world = World::new(Config {
        roots: vec![roots.clone()],
        ..Config::default()
    });
    world
        .register(Box::new(caribou_wren::Runtime::new()))
        .unwrap();
    world
        .register(Box::new(caribou_plugin::Runtime::new(vec![plugin])))
        .unwrap();
    let errors = Rc::new(RefCell::new(Vec::new()));
    let sink = errors.clone();
    let mut config = VMConfig {
        execution_mode: ExecutionMode::Interpreter,
        error_fn: Some(Box::new(move |_, _, _, message: &str| {
            sink.borrow_mut().push(message.to_owned())
        })),
        ..VMConfig::default()
    };
    caribou_wren::import::configure(&mut config);
    let mut vm = VM::new(config);
    vm.krio_fiber_active = true;
    vm.output_buffer = Some(String::new());
    let result = caribou_wren::with_vm(&mut vm, |vm| vm.interpret("buffers", BUFFERS));
    let output = vm.take_output();
    assert_eq!(
        result,
        InterpretResult::Success,
        "{:?}: {output}",
        errors.borrow()
    );
    assert_eq!(output, "6\ntrue\ntrue\ntrue\n2.5\n0\n");

    // A root's module in Wren's own namespace, `wren:shading`, is the root's
    // shading.wren.
    let loaded = caribou_wren::with_vm(&mut vm, |_| caribou_wren::project::load("wren", "shading"));
    assert_eq!(loaded, Ok(true));
    assert!(caribou::registry::lookup_class("wren", "shading", "Shading").is_some());
    let _ = std::fs::remove_dir_all(&roots);
}
