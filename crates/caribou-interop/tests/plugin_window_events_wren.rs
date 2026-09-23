use caribou::world::{Config, World};
use std::{cell::RefCell, path::PathBuf, rc::Rc};
use wren_lift::runtime::{
    engine::{ExecutionMode, InterpretResult},
    vm::{VM, VMConfig},
};

#[test]
fn window_events_and_scale_callback_cross_wren() {
    caribou_ash::install().unwrap();
    caribou_wren::install().unwrap();
    let library = format!(
        "{}caribou_plugin_window_events.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    );
    let plugin = caribou_plugin::load(
        &PathBuf::from(env!("OUT_DIR"))
            .join("window-events/debug")
            .join(library),
    )
    .unwrap();
    let world = World::new(Config::default());
    world
        .register(Box::new(caribou_ash::Runtime::new()))
        .unwrap();
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
    let result = caribou_wren::with_vm(&mut vm, |vm| {
        vm.interpret(
            "events",
            r#"
import "window:Samples" for Samples
var touch = Samples.echo(Samples.event(2))
System.print(touch.constructor)
System.print(touch.force.force)
var keyboard = Samples.echo(Samples.event(3))
System.print(keyboard.event.physical_key.code.constructor)
System.print(keyboard.event.logical_key.text)
System.print(keyboard.device_id == touch.device_id)
System.print(Samples.event(6).path.bytes[1])
System.print(Samples.event(7).constructor)
var size = Samples.scale(Fn.new {|factor| Samples.physical(factor * 100, 720) }, 1.25)
System.print(size.width)
System.print(size.height)
System.print(Fiber.new { Samples.scale(Fn.new {|factor| Fiber.abort("scale failure") }, 2) }.try())
"#,
        )
    });
    let output = vm.take_output();
    assert_eq!(
        result,
        InterpretResult::Success,
        "{:?}: {output}",
        errors.borrow()
    );
    assert_eq!(
        output,
        "Touch\n2\nKeyA\né\ntrue\n255\nRedrawRequested\n125\n720\nscale failure\n"
    );
}
