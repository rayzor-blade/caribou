//! The driver: what runs a Caribou program, for an embedder or the
//! `caribou` command alike.
//!
//! A program is a Haxe `.hl` and the other languages' modules beside it
//! in the project. A [`Session`] is one world with every resident language
//! around one program: it reads the program's natives for the namespaces
//! it imports, finds the project's source roots, publishes the program's
//! own classes, and runs the program; every other language's module loads
//! on first use, when the program that uses it is running. A project
//! file ([`cbproj`]) declares the project: its entry, compiled here, and
//! its languages, plugins and packages ([`run_project`]). A `.hl` named
//! directly runs with its layout found instead ([`run`]). A bundle ([`bundle`]) is the program and its
//! modules in one file, opened the same way. With the `llvm` feature,
//! [`aot`] builds the program ahead of time instead, for a target with no
//! interpreter.

#[cfg(feature = "llvm")]
pub mod aot;
pub mod bundle;
pub mod cbproj;
pub mod deps;
pub mod project;
pub mod session;

pub use session::{Options, Session, run, run_project};
