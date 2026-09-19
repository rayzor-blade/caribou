//! A Wren class published for Haxe, and a Wren object held from the Haxe
//! side: the ref Haxe keeps is one per object, keeps the object alive
//! through both collectors for as long as Haxe holds it, forwards every
//! message to it, and gives the object back as itself when it returns to
//! Wren.
//!
//! One test, because the seams are process-global: both are installed
//! before either runtime allocates, the world registers both adapters, and
//! only then does the VM run.

use std::ffi::c_void;

use caribou::bridge;
use caribou::error::Str;
use caribou::heap::{self, Handle};
use caribou::protocol::Send;
use caribou::registry::{self, Interface, MethodKind};
use caribou::symbol::intern;
use caribou::world::{Config, World};
use caribou_abi::{LangId, Value};
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::vm::{VM, VMConfig};

const HUD: &str = r#"
class Hud {
  construct new(p) { _p = p }
  draw() { }
  score { 3 }
  score=(v) { _p = v }
  name { "hud %(_p)" }
  static make(p) { return Hud.new(p) }
}
"#;

fn vm(mode: ExecutionMode) -> VM {
    let mut vm = VM::new(VMConfig {
        execution_mode: mode,
        ..VMConfig::default()
    });
    vm.output_buffer = Some(String::new());
    vm
}

/// Whether wren_lift still has the object whose core address is `hidden`
/// inverted; the address is spelled out only in this frame, which is gone
/// before the next collection scans the stack.
#[inline(never)]
fn wren_has(vm: &VM, hidden: usize) -> bool {
    vm.gc.containing_allocation(!hidden + 16).is_some()
}

/// The ref Haxe holds for the object at `hidden` inverted, as its address
/// inverted, or 0 for none; see `wren_has` for the frame. A word, not an
/// `Option`: the payload register of a `None` is whatever the callee left
/// there, the raw address included, and the caller spills it.
#[inline(never)]
fn ref_for(hidden: usize) -> usize {
    let obj = Value::object(!hidden as *const c_void);
    caribou_ash::foreign_ref(obj).map_or(0, |r| !(r.as_object().unwrap() as usize))
}

/// Make a `Hud` nothing in Wren refers to, wrap it for Haxe twice, root
/// the ref by a handle as a Haxe frame would, and answer the handle with
/// the ref's and the object's core addresses inverted, so the caller's
/// frame holds no word either collector's conservative scan would take
/// for them.
#[inline(never)]
fn held(vm: &mut VM, hud: &Interface, ash: LangId) -> (Handle, usize, usize) {
    let class = hud.class("Hud").unwrap();
    caribou_wren::with_vm(vm, |vm| {
        let h = bridge::call(
            class.ctor.as_ref().unwrap().target,
            &[Value::number(7.0)],
            ash,
        )
        .expect("constructed");
        let obj = h.as_object().unwrap() as *mut u8;
        let r = caribou_ash::wrap_foreign(h);
        let rp = r.as_object().unwrap() as *mut u8;
        assert_ne!(rp, obj);
        assert_eq!(
            caribou_ash::wrap_foreign(h).to_bits(),
            r.to_bits(),
            "one ref per object"
        );
        assert_eq!(caribou_ash::unwrap_foreign(r).to_bits(), h.to_bits());
        assert_eq!(caribou_ash::wrenref_as_abstract(r), rp as *mut c_void);

        // Every message reaches the Wren object; a string comes back as a
        // core string; the object comes back to Wren as itself.
        let score = class
            .methods
            .iter()
            .find(|m| m.name == "score" && m.kind() == MethodKind::Getter)
            .unwrap();
        assert_eq!(
            bridge::call(score.target, &[r], ash).map(|v| v.as_number()),
            Ok(Some(3.0))
        );
        assert_eq!(
            bridge::get(r, intern("p"), ash).map(|v| v.as_number()),
            Ok(Some(7.0))
        );
        bridge::set(r, intern("score"), Value::number(9.0), ash).expect("set through the ref");
        assert_eq!(
            bridge::get(h, intern("p"), ash).map(|v| v.as_number()),
            Ok(Some(9.0))
        );
        let name = bridge::get(r, intern("name"), ash).expect("a name");
        assert_eq!(unsafe { Str::text(name) }, Some("hud 9"));
        assert_eq!(bridge::type_name(r).as_deref(), Some("main.Hud"));
        unsafe {
            assert_eq!(Send::equals(rp, h), Ok(true));
            assert_eq!(Send::equals(rp, r), Ok(true));
            assert_eq!(Send::hash(rp), Send::hash(obj));
            assert_eq!(
                Str::text(Send::to_string(rp).unwrap()),
                Some("instance of Hud")
            );
        }
        let back = caribou_wren::unwrap(vm, r).expect("crosses back");
        assert_eq!(back.as_object(), Some(obj.wrapping_add(16)));

        (heap::handle_new(rp), !(rp as usize), !(obj as usize))
    })
}

#[test]
fn haxe_holds_a_wren_object_through_a_ref() {
    // Both seams before either runtime allocates.
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");

    let world = World::new(Config::default());
    let ash = world
        .register(Box::new(caribou_ash::Runtime::new()))
        .expect("haxe registers")[0];
    let wren = world
        .register(Box::new(caribou_wren::Runtime::new()))
        .expect("wren registers")[0];

    let mut vm = vm(ExecutionMode::Interpreter);
    assert_eq!(vm.interpret("main", HUD), InterpretResult::Success);
    let hud = caribou_wren::publish_module(&vm, "main").expect("Hud publishes");
    assert_eq!(hud.lang, wren);
    assert!(registry::lookup_class("wren", "main", "Hud").is_some());

    let (root, ref_hidden, obj_hidden) = held(&mut vm, &hud, ash);

    // Both collectors run; Haxe's handle on the ref is what keeps the
    // object, on both sides.
    heap::major();
    vm.collect_garbage();
    assert!(
        wren_has(&vm, obj_hidden),
        "the object outlived a Wren cycle"
    );
    assert_eq!(
        ref_for(obj_hidden),
        ref_hidden,
        "the same ref is still the one for the object"
    );

    // Haxe lets go: the ref dies with the core's collection and gives up
    // its handle; the object goes with the next Wren cycle. The scrub
    // clears what `ref_for` left below this frame.
    heap::handle_release(root);
    heap::scrub_stack_and_registers();
    heap::major();
    assert_eq!(ref_for(obj_hidden), 0, "the object no longer keeps the ref");
    heap::scrub_stack_and_registers();
    vm.collect_garbage();
    assert!(
        !wren_has(&vm, obj_hidden),
        "the object went with the Wren cycle"
    );
    drop(vm);
}
