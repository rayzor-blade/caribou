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
//! It also sets what the program's `main` runs before the entry: the
//! world that gives each language its id, and the WrenLift modules linked
//! beside it, in a VM of their own ([`wren`]).

use std::process;

mod plugin;
mod wren;

#[used]
#[cfg_attr(
    any(target_os = "linux", target_os = "android", target_family = "wasm"),
    unsafe(link_section = ".init_array")
)]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func")
)]
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
    caribou_ash::on_program_start(program_start);
    #[cfg(target_family = "wasm")]
    caribou::heap::set_poll_guard(no_runtime_callouts);
}

/// Whether no Ash or WrenLift runtime frame on this thread is in a call
/// out to compiled code: such a frame holds objects in wasm locals, which
/// no scan reaches, so a compiled poll below it must not collect.
#[cfg(target_family = "wasm")]
fn no_runtime_callouts() -> bool {
    ash_std::gc::hlp_runtime_callout_depth() + wren_lift::capi::wlift_runtime_callout_depth() <= 0
}

/// The host table, for a plugin loaded as a side module: it imports this
/// rather than being handed the table by an entry nothing calls.
#[unsafe(no_mangle)]
pub extern "C" fn caribou_host_table() -> *const caribou_abi::host::Host {
    caribou_plugin::host_table()
}

/// Once the heap is up and before the program's entry: the world that
/// gives each language its id, as a hosted run's does, with the linked
/// plugins, then the linked Wren modules. The world lives as long as the
/// program.
fn program_start() -> i32 {
    let world = caribou::world::World::new(caribou::world::Config::default());
    for adapter in [
        Box::new(caribou_ash::Runtime::new()) as Box<dyn caribou::world::Adapter>,
        Box::new(caribou_wren::Runtime::new()),
    ] {
        if let Err(e) = world.register(adapter) {
            eprintln!("caribou: registering a language: {e:?}");
            return 70;
        }
    }
    if let Err(e) = plugin::start(world) {
        eprintln!("caribou: registering the linked plugins: {e}");
        return 70;
    }
    wren::start()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_constructor_filled_both_seams() {
        assert!(caribou_ash::installed());
        assert!(caribou_wren::installed());
    }
}
