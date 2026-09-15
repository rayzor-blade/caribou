//! The reactor: a world's sources of external wakeups. A source is a
//! handler the world runs on its main context, between turns, each time
//! its signal is raised; the raise may come from any thread, a file
//! watcher's, a socket poller's, one the runtime never made, and reaches
//! the world through its endpoint, the command that wakes it from idle.
//! A task waiting for the outside parks on a token instead, and is woken
//! by `wake`; a source is for work the world itself must do when
//! something outside happens.

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
