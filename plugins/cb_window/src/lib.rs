//! The window plugin: xwindow's `window` API over Caribou's carriers. The
//! model is generated from xwindow's declaration; on a desktop its backend
//! is winit, and in a page the page's canvas, driven by xwindow's agent,
//! which the host starts beside the program.
#![allow(non_snake_case)]
// Nearly all of the crate is xwindow's generated model and backend, which
// xwindow lints.
#![allow(clippy::all)]
// The page backend reaches only part of what the model declares.
#![cfg_attr(target_os = "wasi", allow(dead_code))]
// The page backend's mailbox waits with wasm's atomic wait.
#![cfg_attr(
    all(target_os = "wasi", target_feature = "atomics"),
    feature(stdarch_wasm_atomic_wait)
)]

#[cfg(not(target_os = "wasi"))]
mod backend {
    // The external-pump hooks are for a host that owns the loop; a window's
    // own wait joins the world's scheduler instead (`window_wait`).
    #[allow(dead_code)]
    mod native {
        include!(concat!(env!("OUT_DIR"), "/xwindow_backend/native.rs"));
    }
    pub use native::*;

    /// A window's blocking wait, joined to the world's scheduler: the
    /// world's ready tasks run first, the wait lasts no longer than its
    /// next timer, and work queued for the world meanwhile wakes it.
    pub unsafe fn window_wait(handle: i32, timeout: f64) -> crate::Event {
        unsafe extern "C" fn wake() {
            unsafe { native::window_wake() };
        }
        struct Resumed;
        impl Drop for Resumed {
            fn drop(&mut self) {
                caribou_abi::host::host_resumed();
            }
        }
        // Prepared before the backend borrows its loop: a task the turn
        // runs may itself call a window's API.
        let timeout = caribou_abi::host::host_wait(timeout, wake);
        let _resumed = Resumed;
        unsafe { native::window_wait(handle, timeout) }
    }
}

/// The host's hook, for a host that builds the event loop (on Android, with
/// its `AndroidApp`) or runs it and gives the program turns (on iOS), with
/// the winit it is built from.
#[cfg(not(target_os = "wasi"))]
pub use backend::{Drive, attach};
#[cfg(not(target_os = "wasi"))]
pub use winit;

/// For an app that links the plugin from C on Android: winit's activity
/// calls this, which keeps the app for the backend and starts the app's
/// program, `xwindow_main`.
#[cfg(all(target_os = "android", feature = "android-main"))]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    unsafe extern "C" {
        fn xwindow_main();
    }
    backend::android_app(app);
    unsafe { xwindow_main() }
}

#[cfg(target_os = "wasi")]
mod web {
    include!(concat!(env!("OUT_DIR"), "/xwindow_backend/web.rs"));
}
/// The page backend: what `web` defines, and a refusal for the rest.
#[cfg(target_os = "wasi")]
#[allow(clippy::all)]
mod backend {
    use super::*;
    include!(concat!(env!("OUT_DIR"), "/window_web_backend.rs"));
}
/// The wire to the page's agent.
#[cfg(target_os = "wasi")]
#[allow(dead_code, non_camel_case_types, unused_variables, clippy::all)]
mod wire {
    include!(concat!(env!("OUT_DIR"), "/window_wire.rs"));
}
mod runtime {
    pub use caribou_abi::{Buffer, BufferMut, Enum, ErrorKind, Future, Text, host};
}

#[allow(unused_imports)]
use runtime::{Buffer, BufferMut, Enum, Future, Text};
include!(concat!(env!("OUT_DIR"), "/window.rs"));
