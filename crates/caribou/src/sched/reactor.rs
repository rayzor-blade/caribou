//! The reactor: a world's sources of external wakeups. A source is a
//! handler the world runs on its main context, between turns, each time
//! its signal is raised; the raise may come from any thread, a file
//! watcher's, a socket poller's, one the runtime never made, and reaches
//! the world through its endpoint, the command that wakes it from idle.
//! A task waiting for the outside parks on a token instead, and is woken
//! by `wake`; a source is for work the world itself must do when
//! something outside happens.

use std::sync::atomic::AtomicU32;

use super::world::{self, WorldCommand};

/// A source's handle, raised from any thread. A raise after the source is
/// removed, or once its world is gone, does nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signal {
    world: u64,
    id: u64,
}

impl Signal {
    /// Ask the source's world to run the handler at its next turn. `false`
    /// when the world is gone.
    pub fn raise(&self) -> bool {
        match world::endpoint_of(self.world) {
            Some(endpoint) => {
                endpoint.push(WorldCommand::Ready(self.id));
                true
            }
            None => false,
        }
    }
}

/// Add a source to this thread's world. `handler` runs on the world's main
/// context, between turns, once per raise of the signal that comes back;
/// raises while it runs fold into one more run.
pub fn add_source(handler: impl FnMut() + 'static) -> Signal {
    let (world, id) = world::add_source(Box::new(handler));
    Signal { world, id }
}

/// Forget a source of this thread's world; its handler is dropped once
/// it is not running.
pub fn remove_source(signal: &Signal) {
    world::remove_source(signal.id);
}

/// A watched word's handle.
#[derive(Debug, PartialEq, Eq)]
pub struct Watch(u64);

/// Watch `word` on this thread's world: `handler` runs on the world's main
/// context, between turns, each time the word no longer holds the value it
/// held when last looked at. Something outside the world that changes it,
/// such as an agent sharing a wasm program's memory, then adds one to the
/// wake word this returns and notifies it, which ends the world's idle
/// wait on wasm; elsewhere the change is seen at the world's next turn.
pub fn watch(
    word: &'static AtomicU32,
    handler: impl FnMut() + 'static,
) -> (Watch, &'static AtomicU32) {
    let (id, wake) = world::add_watch(word, Box::new(handler));
    (Watch(id), wake)
}

/// Stop watching; the handler is dropped once it is not running.
pub fn unwatch(watch: &Watch) {
    world::remove_watch(watch.0);
}
