use caribou::world::{Config, World};
use std::{cell::RefCell, path::PathBuf, rc::Rc};
use wren_lift::runtime::{
    engine::{ExecutionMode, InterpretResult},
    vm::{VM, VMConfig},
};

#[test]
fn window_events_cross_wren() {
    caribou_ash::install().unwrap();
    caribou_wren::install().unwrap();
    let library = format!(
        "{}caribou_plugin_window_events.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    );
    let plugin = caribou_plugin::load(
        &PathBuf::from(env!("CARIBOU_TEST_PLUGINS"))
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
import "window:Event" for Event
import "window:TouchForce" for TouchForce
import "window:TouchPhase" for TouchPhase
import "window:Theme" for Theme
var touch = Samples.event(2)
System.print(touch.constructor)
System.print(touch.force.force)
var keyboard = Samples.event(3)
System.print(keyboard.event.physical_key.code.constructor)
System.print(keyboard.event.logical_key.text)
System.print(keyboard.device_id == touch.device_id)
System.print(Samples.event(6).path.bytes[1])
System.print(Samples.event(7).constructor)
System.print(Samples.sizing(0).constructor)
System.print(Samples.sizing(1).constructor)
System.print(Samples.theme(Theme.Dark))
System.print(Samples.theme(null))
System.print(Fiber.new { Samples.theme(3) }.try())
var touched = Event.Touch(4, TouchPhase.Ended, 0.5, 1.5, TouchForce.None, 9)
System.print(touched.phase.constructor)
System.print(touched.id)
System.print(Fiber.new { Event.Touch(4, 5, 0.5, 1.5, TouchForce.None, 9) }.try())
var far = Samples.event(10)
System.print(far.id.toString)
var copy = Event.Touch(far.device_id, far.phase, far.x, far.y, far.force, far.id)
System.print(copy.id == far.id)
System.print(copy.id != touched.id)
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
        concat!(
            "Touch\n2\nKeyA\né\ntrue\n255\nRedrawRequested\nLogical\nPhysical\n",
            "1\n-1\nargument 1 of the plugin function must be a window.Theme, not a number\n",
            "Ended\n9\nwindow.Event.Touch: invalid phase field\n",
            "1152921504606846977\ntrue\ntrue\n"
        )
    );
}
