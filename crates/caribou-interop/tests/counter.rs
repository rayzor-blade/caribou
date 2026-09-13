//! The first values across the bridge: a Wren object driven from the Haxe
//! side, and a Haxe-shaped C function called from the Wren side through
//! Ash's dispatcher, with an error each way.
//!
//! One test, because the seams are process-global: both are installed
//! before either runtime allocates, the world registers both adapters
//! before the first Wren VM exists (registration writes the language id
//! into the descriptor every Wren object carries), and only then does a VM
//! run.

use std::ffi::c_void;
use std::ptr;

use caribou::bridge;
use caribou::diag::{self, NoSources};
use caribou::error::{Error, Str};
use caribou::heap;
use caribou::protocol::{Callable, Fault, Send};
use caribou::symbol::intern;
use caribou::world::{Config, World};
use caribou_abi::hl::{
    self, hl_type, hl_type_detail, hl_type_fun, hl_type_fun_closure, hl_type_fun_closure_type,
    vdynamic,
};
use caribou_abi::{ErrorKind, LangId, Value};
use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
use wren_lift::runtime::gc_trait::GcStrategy;
use wren_lift::runtime::vm::{VM, VMConfig};

const SCRIPT: &str = r#"
class Counter {
  construct new() { _n = 0 }
  inc() {
    _n = _n + 1
    return _n
  }
  n { _n }
}
var c = Counter.new()
"#;

/// An `HFUN` type over the given argument and return types, from ash's own
/// singletons so the dispatcher sees the kinds Haxe code would declare.
fn signature(args: &[*mut hl_type], ret: *mut hl_type) -> *const hl_type {
    let nargs = args.len() as i32;
    let args = Box::leak(args.to_vec().into_boxed_slice()).as_mut_ptr();
    let fun = Box::into_raw(Box::new(hl_type_fun {
        args,
        ret,
        nargs,
        parent: ptr::null_mut(),
        closure_type: hl_type_fun_closure_type {
            kind: hl::HVOID,
            p: ptr::null_mut(),
        },
        closure: hl_type_fun_closure {
            args: ptr::null_mut(),
            ret: ptr::null_mut(),
            nargs: 0,
            parent: ptr::null_mut(),
        },
    }));
    Box::into_raw(Box::new(hl_type {
        kind: hl::HFUN,
        detail: hl_type_detail { fun },
        vobj_proto: ptr::null_mut(),
        mark_bits: ptr::null_mut(),
    }))
}

extern "C" fn add_one(n: i32) -> i32 {
    n + 1
}

/// A Haxe-shaped native that raises the runtime's own error, as
/// `hl_error` does. The message outlives the throw: the error keeps the
/// pointer.
extern "C" fn boom(_n: i32) -> i32 {
    static MESSAGE: [u16; 5] = [b'b' as u16, b'o' as u16, b'o' as u16, b'm' as u16, 0];
    unsafe { ash_std::error::hlp_error(MESSAGE.as_ptr()) };
    0
}

fn vm(mode: ExecutionMode) -> VM {
    let mut vm = VM::new(VMConfig {
        execution_mode: mode,
        gc_strategy: GcStrategy::Immix,
        ..VMConfig::default()
    });
    vm.output_buffer = Some(String::new());
    vm
}

/// The Haxe side drives a Wren counter: two calls, a read, then a call the
/// class does not answer.
fn drive_counter(mode: ExecutionMode, ash: LangId, wren: LangId) {
    let mut vm = vm(mode);
    assert_eq!(vm.interpret("main", SCRIPT), InterpretResult::Success);
    let c = vm
        .find_imported_var_from("c", "main")
        .expect("`c` is defined");
    let c = caribou_wren::wrap(c);

    caribou_wren::with_vm(&mut vm, |vm| {
        let inc = intern("inc");
        assert_eq!(
            bridge::invoke(c, inc, &[], ash).map(|v| v.as_number()),
            Ok(Some(1.0))
        );
        assert_eq!(
            bridge::invoke(c, inc, &[], ash).map(|v| v.as_number()),
            Ok(Some(2.0))
        );
        assert_eq!(
            bridge::get(c, intern("n"), ash).map(|v| v.as_number()),
            Ok(Some(2.0))
        );

        let err = bridge::invoke(c, intern("nope"), &[], ash).unwrap_err();
        let e = unsafe { Error::from_value(err) }.expect("an Error value");
        let _root = heap::handle_new(e as *mut u8);
        unsafe {
            assert_eq!(Error::kind(e), ErrorKind::Runtime);
            assert_eq!(Error::origin(e), wren);
            assert_eq!(Error::message_str(e), "Counter does not implement 'nope()'");
            let frames = Error::frames(e);
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].lang, wren);
            assert_eq!(frames[0].name_str(), "nope");
        }
        // The native payload is Wren's own error value: its message string.
        let native = unsafe { Error::native(e) };
        let native = caribou_wren::unwrap(vm, native).expect("a Wren value");
        assert!(native.is_string_object());
        assert_eq!(
            wren_lift::runtime::core::as_string(native),
            "Counter does not implement 'nope()'"
        );
        let text = diag::render_string(&diag::report(err), &NoSources, false);
        assert!(text.contains("wren"), "{text}");
        assert!(text.contains("does not implement"), "{text}");

        // The counter answers the rest of the protocol through its own
        // methods too.
        let s = bridge::invoke(c, intern("toString"), &[], ash).unwrap();
        assert!(caribou_wren::unwrap(vm, s).unwrap().is_string_object());
        let obj = c.as_object().unwrap() as *mut u8;
        let rendered = unsafe { Send::to_string(obj) }.unwrap();
        assert_eq!(unsafe { Str::text(rendered) }, Some("instance of Counter"));
        assert_eq!(unsafe { Send::equals(obj, c) }, Ok(true));
        assert_eq!(unsafe { Send::len(obj) }, Err(Fault::Unsupported));
    });
}

#[test]
fn a_counter_crosses_from_wren_and_a_typed_call_crosses_from_haxe() {
    // Both seams before either runtime allocates.
    caribou_ash::install().expect("ash takes the table in a fresh process");
    caribou_wren::install().expect("wren_lift takes the table in a fresh process");

    let mut world = World::new(Config::default());
    let ash = world
        .register(Box::new(caribou_ash::Runtime::new()))
        .expect("haxe registers")[0];
    let wren = world
        .register(Box::new(caribou_wren::Runtime::new()))
        .expect("wren registers")[0];
    assert_eq!(caribou_ash::lang(), ash);
    assert_eq!(caribou_wren::lang(), wren);
    assert_ne!(ash, wren);

    drive_counter(ExecutionMode::Interpreter, ash, wren);
    drive_counter(ExecutionMode::Tiered, ash, wren);

    // The other direction: a typed callable of Haxe's language, marshalled
    // by Ash's dispatcher through `hlp_dyn_call`.
    let i32_t = ash_std::types::hlt_i32().cast::<hl_type>();
    let sig = signature(&[i32_t], i32_t);
    let r = bridge::call(
        Callable::Typed {
            func: add_one as *const c_void,
            signature: sig,
            lang: ash,
        },
        &[Value::int(41)],
        wren,
    );
    assert_eq!(r, Ok(Value::int(42)));
    // A number where an int is declared is converted, as ash's own cast does.
    let r = bridge::call(
        Callable::Typed {
            func: add_one as *const c_void,
            signature: sig,
            lang: ash,
        },
        &[Value::number(7.0)],
        wren,
    );
    assert_eq!(r, Ok(Value::int(8)));

    // A throw inside lands on the trap, not in Rust: the error carries the
    // thrown value, wrapped, and one frame for the Haxe segment.
    let err = bridge::call_named(
        Callable::Typed {
            func: boom as *const c_void,
            signature: sig,
            lang: ash,
        },
        &[Value::int(1)],
        wren,
        "boom",
    )
    .unwrap_err();
    let e = unsafe { Error::from_value(err) }.expect("an Error value");
    let _root = heap::handle_new(e as *mut u8);
    unsafe {
        assert_eq!(Error::kind(e), ErrorKind::Runtime);
        assert_eq!(Error::origin(e), ash);
        assert_eq!(Error::message_str(e), "boom");
        let frames = Error::frames(e);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].lang, ash);
        assert_eq!(frames[0].name_str(), "boom");
        let thrown: *mut vdynamic = caribou_ash::unwrap(Error::native(e)).expect("a Haxe value");
        assert_eq!((*(*thrown).t).kind, hl::HBYTES);
    }
    let text = diag::render_string(&diag::report(err), &NoSources, false);
    assert!(text.contains("haxe"), "{text}");
    // Back to a Haxe caller, the same error is the thrown value itself.
    let home = bridge::call(
        Callable::Typed {
            func: boom as *const c_void,
            signature: sig,
            lang: ash,
        },
        &[Value::int(1)],
        ash,
    )
    .unwrap_err();
    assert!(caribou_ash::unwrap(home).is_some());
}
