//! The window plugin: xwindow's `window` API over Caribou's carriers. The
//! model is generated from xwindow's declaration; on a desktop its backend
//! is winit, and in a page the page's canvas, driven by xwindow's agent,
//! which the host starts beside the program.
#![allow(non_snake_case, clippy::too_many_arguments)]
// The page backend reaches only part of what the model declares.
#![cfg_attr(target_os = "wasi", allow(dead_code))]
// The page backend's mailbox waits with wasm's atomic wait.
#![cfg_attr(
    all(target_os = "wasi", target_feature = "atomics"),
    feature(stdarch_wasm_atomic_wait)
)]

#[cfg(not(target_os = "wasi"))]
mod backend {
    include!(concat!(env!("OUT_DIR"), "/xwindow_backend/native.rs"));
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
