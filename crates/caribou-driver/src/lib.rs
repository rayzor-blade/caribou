//! The driver: what runs a Caribou program, for an embedder or the
//! `caribou` command alike.
//!
//! A program is a Haxe `.hl` and the other languages' modules beside it
//! in the project. A [`Session`] is one world with every resident language
//! around one program: it reads the program's natives for the namespaces
//! it imports, finds the project's source roots, publishes the program's
//! own classes, and runs the program; every other language's module loads
//! on first use, when the program that uses it is running. Nothing is
//! configured: the project's layout is the configuration. [`run`] is the
//! whole thing in one call. A bundle ([`bundle`]) is the program and its
//! modules in one file, opened the same way.

pub mod bundle;
pub mod project;
pub mod session;

pub use session::{Options, Session, run};
