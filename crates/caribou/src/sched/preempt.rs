//! The poll epoch: the one word compiled loops read on every back-edge.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Changes only when the runtime needs attention: a quantum elapsed, work
/// reached a world, or the collector wants the world stopped. A loop pays
/// one load and compare per back-edge and nothing else. Exported by name
/// so an object file can reference it; a JIT takes
/// [`poll_epoch_address`].
#[unsafe(export_name = "caribou_poll_epoch")]
pub static POLL_EPOCH: AtomicU64 = AtomicU64::new(1);

/// Make every running compiled activation reach a safepoint. Monotonic, so
/// one world cannot consume another's request.
pub fn request_poll() {
    POLL_EPOCH.fetch_add(1, Ordering::Release);
}

/// For code generators that bake the address in. Emit a monotonic load;
/// a sequentially consistent one made the safepoint itself the bottleneck.
pub fn poll_epoch_address() -> *const u64 {
    POLL_EPOCH.as_ptr()
}

/// Tasks alive across every world. The timer runs only while this is
/// non-zero: one task and the main context are already two parties that
/// must take turns.
static LIVE_TASKS: AtomicUsize = AtomicUsize::new(0);

#[cfg(not(target_family = "wasm"))]
const QUANTUM: std::time::Duration = std::time::Duration::from_millis(2);

pub(super) fn task_created() {
    LIVE_TASKS.fetch_add(1, Ordering::Release);
    ensure_preemption_timer();
    request_poll();
}

pub(super) fn task_removed() {
    LIVE_TASKS.fetch_sub(1, Ordering::Release);
}

pub(super) fn tasks_removed(count: usize) {
    LIVE_TASKS.fetch_sub(count, Ordering::Release);
}

pub(super) fn any_live_tasks() -> bool {
    LIVE_TASKS.load(Ordering::Acquire) != 0
}

/// A wasm module has one thread and yields through the host, so there is
/// no timer to start and no thread to start it on: naming `spawn` alone
/// makes the module import `pthread_create`.
#[cfg(target_family = "wasm")]
fn ensure_preemption_timer() {}

#[cfg(not(target_family = "wasm"))]
fn ensure_preemption_timer() {
    use std::sync::{Condvar, LazyLock, Mutex, OnceLock};

    static STARTED: OnceLock<()> = OnceLock::new();
    static WAKE: LazyLock<(Mutex<()>, Condvar)> =
        LazyLock::new(|| (Mutex::new(()), Condvar::new()));

    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new()
            .name("caribou-sched-timer".into())
            .spawn(|| {
                loop {
                    let (lock, changed) = &*WAKE;
                    let mut guard = lock.lock().unwrap();
                    while !any_live_tasks() {
                        guard = changed.wait(guard).unwrap();
                    }
                    drop(guard);

                    std::thread::sleep(QUANTUM);
                    if any_live_tasks() {
                        request_poll();
                    }
                }
            });
    });
    WAKE.1.notify_one();
}
