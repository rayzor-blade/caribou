//! The media plugin: xavi's `media` API over Caribou's carriers. The model
//! is generated from xavi's declaration; natively its backend is xavi's,
//! over each platform's codecs and audio output, and in a page the page's
//! media player, driven by xavi's agent, which the host starts beside the
//! program.
#![allow(non_snake_case, unused_unsafe)]
// Nearly all of the crate is xavi's generated model and backend, which
// xavi lints.
#![allow(clippy::all)]
// The page backend reaches only part of what the model declares.
#![cfg_attr(target_os = "wasi", allow(dead_code))]

/// One set of handle tables for the process: a handle may cross between
/// the world's threads, and the backend locks its tables.
static MEDIA: xavi_backend::MediaBackend = xavi_backend::MediaBackend::new();

fn with_media<T>(operation: impl FnOnce(&xavi_backend::MediaBackend) -> T) -> T {
    operation(&MEDIA)
}

#[cfg(not(target_os = "wasi"))]
mod backend {
    include!(concat!(env!("OUT_DIR"), "/xavi_backend/native.rs"));
}

#[cfg(target_os = "wasi")]
mod web {
    include!(concat!(env!("OUT_DIR"), "/xavi_backend/web.rs"));
}
/// The page backend: what `web` defines, and a refusal for the rest.
#[cfg(target_os = "wasi")]
#[allow(clippy::all)]
mod backend {
    use super::*;
    include!(concat!(env!("OUT_DIR"), "/media_web_backend.rs"));
}
/// The wire to the page's agent.
#[cfg(target_os = "wasi")]
#[allow(dead_code, non_camel_case_types, unused_variables, clippy::all)]
mod wire {
    include!(concat!(env!("OUT_DIR"), "/media_wire.rs"));
}

mod runtime {
    pub use caribou_abi::{Buffer, BufferMut, Enum, ErrorKind, Future, Rooted, Text, host};
}

#[allow(unused_imports)]
use runtime::{Buffer, BufferMut, Enum, ErrorKind, Future, Rooted, Text, host};
include!(concat!(env!("OUT_DIR"), "/media.rs"));
