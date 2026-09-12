//! The shared runtime core: one heap, one scheduler, one module registry,
//! one hot-reload pipeline, under every resident runtime.
//!
//! Layouts and constants that cross the boundary live in [`caribou_abi`];
//! this crate defines behaviour.

pub use caribou_abi as abi;

pub mod heap;
pub mod protocol;
pub mod sched;
pub mod world;
