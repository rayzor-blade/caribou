//! What a linked program runs on: `ash_std`, WrenLift's runtime and the
//! caribou core with both adapters, in one library, so every runtime export
//! is defined once. A program compiled ahead of time, native or wasm, is
//! linked against it in place of Ash's own runtime object.
//!
//! Nothing loads a program here, so nothing calls the adapters' `install`
//! at run time the way the driver does. A static constructor does, before
//! the program's entry point creates the heap: the engine runs a wasm
//! module's constructors at instantiation, and a native loader runs them
//! before `main`. Each seam refuses an install once its heap exists.

use std::process;

#[used]
#[cfg_attr(
    any(target_os = "linux", target_os = "android", target_family = "wasm"),
    unsafe(link_section = ".init_array")
)]
#[cfg_attr(target_vendor = "apple", unsafe(link_section = "__DATA,__mod_init_func"))]
#[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
static START: extern "C" fn() = start;

/// Fill Ash's and WrenLift's seams with the core. A program whose seams
/// cannot be filled cannot run, so a refusal ends it here.
extern "C" fn start() {
    if let Err(e) = caribou_ash::install() {
        eprintln!("caribou: installing the core into Ash: {e}");
        process::abort();
    }
    if let Err(e) = caribou_wren::install() {
        eprintln!("caribou: installing the core into WrenLift: {e}");
        process::abort();
    }
}

/// Where a plugin linked into the program registers, from its own
/// constructor (`caribou_abi::plugin!` with the `linked` feature).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn caribou_plugin_register(entry: caribou_plugin::Entry) {
    caribou_plugin::register_linked(entry);
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_constructor_filled_both_seams() {
        assert!(caribou_ash::installed());
        assert!(caribou_wren::installed());
    }
}
