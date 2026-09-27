//! Plugins in a linked program: those linked in, registered when the
//! program starts, and those a wasm program finds as side modules beside
//! it, registered on first need; and the casts a Haxe call to a plugin
//! class's member makes at the boundary. A plugin instance is its payload
//! to the plugin and a core object to the core, whose Haxe face stands for
//! it; the face's class names the plugin class (`math.Vec2`).

use std::cell::RefCell;
use std::ffi::c_void;

use caribou::world::World;
use caribou_abi::hl::{hl_type, vdynamic};

thread_local! {
    /// The program's world, for plugins registered after it started.
    static WORLD: RefCell<Option<World>> = const { RefCell::new(None) };
}

/// Register the linked plugins with `world`, and keep it for those found
/// later.
pub(crate) fn start(world: World) -> Result<(), String> {
    let linked = caribou_plugin::linked().map_err(|e| e.to_string())?;
    if !linked.is_empty() {
        world
            .register(Box::new(caribou_plugin::Runtime::new(linked)))
            .map_err(|e| format!("{e:?}"))?;
    }
    WORLD.with(|w| *w.borrow_mut() = Some(world));
    caribou_plugin::set_loader(load);
    Ok(())
}

/// The side module `name` beside the program, registered as the plugin it
/// holds. False when there is none, or no world to register it with.
fn load(name: &str) -> bool {
    #[cfg(target_family = "wasm")]
    {
        let plugin = match caribou_plugin::load_side_module(name) {
            Ok(plugin) => plugin,
            Err(e) => {
                eprintln!("caribou: plugin {name}: {e}");
                return false;
            }
        };
        WORLD.with(|w| {
            w.borrow()
                .as_ref()
                .is_some_and(|w| w.register(Box::new(caribou_plugin::Runtime::new(vec![plugin]))).is_ok())
        })
    }
    #[cfg(not(target_family = "wasm"))]
    {
        let _ = name;
        false
    }
}

/// The plugin object a Haxe face stands for, as its payload: a receiver,
/// or an object passed. A face that stands for none raises rather than
/// hand the plugin a null.
///
/// # Safety
/// `face` is null or a live Haxe object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_plugin_from_haxe_face(face: *mut vdynamic, _t: *mut hl_type) -> *mut c_void {
    let payload = {
        let _kept = (!face.is_null()).then(|| caribou::heap::keep(face.cast()));
        unsafe { caribou_ash::link::behind(face) }.and_then(caribou_plugin::payload)
    };
    match payload {
        Some(payload) => payload,
        None => raise("the value is not a plugin object"),
    }
}

/// Bind the face Haxe just constructed to the plugin object the linked
/// constructor made, of the class the face's type names.
///
/// # Safety
/// `face` is a live instance of a face class, of the program's type `t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_plugin_bind_face(face: *mut vdynamic, payload: *mut c_void, t: *mut hl_type) {
    let bound = {
        let _kept = caribou::heap::keep(face.cast());
        unsafe { object(payload, t) }.map(|object| unsafe { caribou_ash::link::bind(face, object) })
    };
    if bound.is_none() {
        raise("no plugin declares the class this face stands for");
    }
}

/// A plugin object a linked call returned, as a face of the program's
/// type `t`.
///
/// # Safety
/// `t` is the program's type of a face class.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_plugin_to_haxe_face(payload: *mut c_void, t: *mut hl_type) -> *mut vdynamic {
    if payload.is_null() {
        return std::ptr::null_mut();
    }
    match unsafe { object(payload, t) } {
        Some(object) => unsafe { caribou_ash::link::face(object, t) },
        None => raise("no plugin declares the class this face stands for"),
    }
}

/// Throw `message` into the Haxe caller. Called with no guard alive: the
/// throw leaves this frame without running its drops.
fn raise(message: &str) -> ! {
    caribou_ash::link::raise(message, caribou::world::LANG_CORE);
    unreachable!("a raise into Haxe does not return")
}

/// A core object of the plugin class the face type `t` names, holding
/// `payload`.
unsafe fn object(payload: *mut c_void, t: *mut hl_type) -> Option<caribou_abi::Value> {
    let name = unsafe { caribou_ash::link::type_name(t) }?;
    caribou_plugin::object(&name, payload)
}
