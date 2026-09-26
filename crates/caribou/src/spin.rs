//! A lock for data held for a handful of instructions: taken with one
//! atomic exchange when free; when not, it spins briefly and then yields.
//! No poisoning, and not reentrant: in a debug build, a thread that asks
//! for a lock it already holds panics rather than spinning forever.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
#[cfg(debug_assertions)]
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::{AtomicBool, Ordering};

pub struct SpinLock<T> {
    held: AtomicBool,
    /// The holding thread, by [`this_thread`]; zero when free.
    #[cfg(debug_assertions)]
    owner: AtomicUsize,
    value: UnsafeCell<T>,
}

/// This thread, as an address no other live thread shares.
#[cfg(debug_assertions)]
fn this_thread() -> usize {
    std::thread_local!(static ME: u8 = const { 0 });
    ME.with(|me| me as *const u8 as usize)
}

// SAFETY: the value is reached only through a guard, one at a time.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> SpinLock<T> {
        SpinLock {
            held: AtomicBool::new(false),
            #[cfg(debug_assertions)]
            owner: AtomicUsize::new(0),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> SpinGuard<'_, T> {
        /// Spins before a wait yields the thread: a holder preempted
        /// mid-section is waited out without burning its core.
        const SPINS: u32 = 64;
        let mut spins = 0;
        while self
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            #[cfg(debug_assertions)]
            assert_ne!(
                self.owner.load(Ordering::Relaxed),
                this_thread(),
                "a SpinLock is not reentrant: this thread holds it already"
            );
            while self.held.load(Ordering::Relaxed) {
                if spins < SPINS {
                    spins += 1;
                    core::hint::spin_loop();
                } else {
                    std::thread::yield_now();
                }
            }
        }
        #[cfg(debug_assertions)]
        self.owner.store(this_thread(), Ordering::Relaxed);
        SpinGuard { lock: self }
    }
}

pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard holds the lock.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard holds the lock.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        self.lock.owner.store(0, Ordering::Relaxed);
        self.lock.held.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(debug_assertions)]
    #[cfg_attr(target_family = "wasm", ignore = "a panic aborts on wasm")]
    #[should_panic(expected = "not reentrant")]
    fn a_thread_that_asks_again_panics() {
        let lock = SpinLock::new(0);
        let _held = lock.lock();
        let _again = lock.lock();
    }

    #[test]
    #[cfg_attr(
        all(target_family = "wasm", not(target_feature = "atomics")),
        ignore = "needs threads"
    )]
    fn one_holder_at_a_time() {
        static COUNT: SpinLock<u64> = SpinLock::new(0);
        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..10_000 {
                        *COUNT.lock() += 1;
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(*COUNT.lock(), 40_000);
    }
}
