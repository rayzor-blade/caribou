//! Host state per stack: what an adapter keeps in the thread's live cells
//! that belongs to a stack rather than a task, Ash's trap chain above all,
//! since a trap is a frame; the bridge's own count of guards is the same
//! kind of thing. It is swapped on every switch of stacks on the thread:
//! the world's own, around a fiber task's turn, and a hosted runtime's
//! own, told through its adapter. A stack is krio's id for it, 0 the
//! thread's own, as in the heap's registry.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;

use super::task::HostState;

thread_local! {
    /// The states of every stack seen on this thread, by stack.
    static STACKS: RefCell<HashMap<u64, Vec<Box<dyn HostState>>>> = RefCell::new(HashMap::new());
}

/// Runs on the thread before a stack's first turn, for every stack: where
/// an adapter attaches the state it keeps per stack.
static STACK_HOOK: Mutex<Vec<fn(u64)>> = Mutex::new(Vec::new());

fn type_of(state: &dyn HostState) -> std::any::TypeId {
    let any: &dyn Any = state;
    any.type_id()
}

/// Add a hook that runs before a stack's first turn on a thread.
pub fn add_stack_hook(hook: fn(u64)) {
    STACK_HOOK.lock().unwrap().push(hook);
}

/// Attach state swapped around `stack`'s turns on this thread. One of the
/// same type already attached is replaced and returned.
pub fn attach_stack_host_state(
    stack: u64,
    state: Box<dyn HostState>,
) -> Option<Box<dyn HostState>> {
    let kind = type_of(&*state);
    STACKS.with(|stacks| {
        let mut stacks = stacks.borrow_mut();
        let slot = stacks.entry(stack).or_default();
        match slot.iter_mut().find(|s| type_of(&***s) == kind) {
            Some(existing) => Some(std::mem::replace(existing, state)),
            None => {
                slot.push(state);
                None
            }
        }
    })
}

/// Borrow the state of type `T` attached to `stack` on this thread. Not
/// available from inside a swap.
pub fn with_stack_host_state<T: HostState, R>(
    stack: u64,
    f: impl FnOnce(&mut T) -> R,
) -> Option<R> {
    STACKS.with(|stacks| {
        let mut stacks = stacks.borrow_mut();
        stacks
            .get_mut(&stack)?
            .iter_mut()
            .find_map(|s| {
                let any: &mut dyn Any = &mut **s;
                any.downcast_mut::<T>()
            })
            .map(f)
    })
}

/// The thread is switching from stack `from` to stack `to`: `from`'s
/// states are swapped out, `to`'s in. A stack seen for the first time
/// gets its states from the hooks first. The states are out of their
/// slot for the calls, so a swap may attach.
pub fn switch_stack(from: u64, to: u64) {
    if from == to {
        return;
    }
    crate::bridge::switch_runs(from, to);
    let mut out = STACKS
        .with(|stacks| stacks.borrow_mut().remove(&from))
        .unwrap_or_default();
    for state in &mut out {
        state.swap_out();
    }
    put_back(from, out);
    let fresh = STACKS.with(|stacks| !stacks.borrow().contains_key(&to));
    if fresh {
        STACKS.with(|stacks| stacks.borrow_mut().insert(to, Vec::new()));
        let hooks: Vec<fn(u64)> = STACK_HOOK.lock().unwrap().clone();
        for hook in hooks {
            hook(to);
        }
    }
    let mut r#in = STACKS
        .with(|stacks| stacks.borrow_mut().remove(&to))
        .unwrap_or_default();
    for state in &mut r#in {
        state.swap_in();
    }
    put_back(to, r#in);
}

fn put_back(stack: u64, mut states: Vec<Box<dyn HostState>>) {
    STACKS.with(|stacks| {
        let mut stacks = stacks.borrow_mut();
        match stacks.get_mut(&stack) {
            // Attached during the swap: kept beside what was out.
            Some(added) => {
                states.append(added);
                *added = states;
            }
            None => {
                stacks.insert(stack, states);
            }
        }
    });
}

/// The stack `id` is gone: its states with it.
pub fn forget_stack(id: u64) {
    crate::bridge::forget_runs(id);
    STACKS.with(|stacks| stacks.borrow_mut().remove(&id));
}
