//! Hosts WrenLift on the caribou core.
//!
//! wren_lift depends on nothing; `wren_lift::runtime::rt` is its runtime seam,
//! a table of every operation its Immix strategy performs on memory. This
//! crate fills that table with the core's heap (`heap.rs`) and installs it
//! before wren_lift's first Immix VM exists, so a Wren program's objects live
//! in the core's heap. Nothing in wren_lift names this crate. The strategy
//! stays wren_lift's: its object layouts, its precise trace, its roots and its
//! cycle; the two slots it fills for a host, `object_trace` and `object_drop`,
//! are left to it.

mod heap;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use wren_lift::runtime::rt::{RuntimeVTable, wlift_rt_install};

/// Why an install did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallError {
    /// `wlift_rt_install` returned false: an Immix VM already exists, another
    /// table was installed first, or the table has another version or size
    /// than this crate was built against.
    Refused,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused => f.write_str(
                "wren_lift refused the runtime table: an Immix VM already exists, a table is already installed, or the table version differs",
            ),
        }
    }
}

impl std::error::Error for InstallError {}

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install the core's heap into the linked wren_lift. Must run before its
/// first Immix VM is created. A second call after a successful one is `Ok`.
pub fn install() -> Result<(), InstallError> {
    if INSTALLED.load(Ordering::Acquire) {
        return Ok(());
    }
    let table = table();
    // SAFETY: the table outlives the call, which copies its entries, and
    // every entry has its slot's signature and contract.
    if !unsafe { wlift_rt_install(&table) } {
        return Err(InstallError::Refused);
    }
    INSTALLED.store(true, Ordering::Release);
    Ok(())
}

/// Whether [`install`] has succeeded in this process.
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

/// Every memory slot filled; `object_trace` and `object_drop` stay
/// wren_lift's.
fn table() -> RuntimeVTable {
    RuntimeVTable {
        heap_new: Some(heap::heap_new),
        heap_drop: Some(heap::heap_drop),
        alloc_raw: Some(heap::alloc_raw),
        containing_allocation: Some(heap::containing_allocation),
        is_heap_ptr: Some(heap::is_heap_ptr),
        mark_allocation: Some(heap::mark_allocation),
        is_marked: Some(heap::is_marked),
        scan_range: Some(heap::scan_range),
        track_external: Some(heap::track_external),
        should_collect: Some(heap::should_collect),
        collect_begin: Some(heap::collect_begin),
        collect_end: Some(heap::collect_end),
        for_each_allocation: Some(heap::for_each_allocation),
        stats: Some(heap::stats),
        ..RuntimeVTable::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wren_lift::runtime::engine::InterpretResult;
    use wren_lift::runtime::gc_trait::GcStrategy;
    use wren_lift::runtime::rt::{RT_VERSION, wlift_rt_installed};
    use wren_lift::runtime::vm::{VM, VMConfig};

    /// The header is one word; every word after it is a memory slot until the
    /// last two, which are wren_lift's.
    #[test]
    fn every_memory_slot_is_filled_and_wren_lifts_two_are_not() {
        let table = table();
        assert_eq!(table.version, RT_VERSION);
        assert_eq!(table.size as usize, std::mem::size_of::<RuntimeVTable>());
        let words = std::mem::size_of::<RuntimeVTable>() / std::mem::size_of::<usize>();
        // SAFETY: the table is repr(C) of u32, u32 and word-sized nullable
        // function pointers, read here as plain words.
        let raw = unsafe {
            std::slice::from_raw_parts(&table as *const RuntimeVTable as *const usize, words)
        };
        for (i, word) in raw.iter().enumerate().skip(1).take(words - 3) {
            assert_ne!(*word, 0, "slot {i} is None");
        }
        assert!(table.object_trace.is_none());
        assert!(table.object_drop.is_none());
    }

    const CHILD_ENV: &str = "CARIBOU_WREN_INSTALL_CHILD";

    /// The table is process-global and sealed by the first Immix VM, so the
    /// install runs in a process of its own: the test re-runs its binary with
    /// only itself selected and checks the exit.
    #[test]
    fn install_takes_and_a_vm_then_allocates_through_the_core() {
        if std::env::var_os(CHILD_ENV).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::install_takes_and_a_vm_then_allocates_through_the_core",
                    "--test-threads=1",
                ])
                .env(CHILD_ENV, "1")
                .status()
                .expect("re-run the test binary");
            assert!(status.success(), "child test process failed: {status}");
            return;
        }

        assert!(!wlift_rt_installed());
        assert!(!installed());
        install().expect("a fresh process takes the table");
        assert!(wlift_rt_installed());
        assert!(installed());
        install().expect("a second install after a successful one is Ok");

        let core_allocated = || {
            let mut total = 0f64;
            unsafe { caribou::heap::stats(&mut total, std::ptr::null_mut(), std::ptr::null_mut()) };
            total as usize
        };
        let before = core_allocated();
        let mut vm = VM::new(VMConfig {
            gc_strategy: GcStrategy::Immix,
            ..VMConfig::default()
        });
        // Sealed by the VM above.
        assert!(!unsafe { wlift_rt_install(&table()) });
        // Enough to cross the core's first trigger, so a cycle runs too.
        let result = vm.interpret(
            "main",
            r#"
                var xs = []
                for (i in 0...100000) xs.add("s%(i)")
                System.print(xs.count)
            "#,
        );
        assert_eq!(result, InterpretResult::Success);
        assert!(
            core_allocated() >= before + 100_000 * 32,
            "the VM's objects did not come from the core's heap"
        );
        let stats = vm.gc.stats();
        assert!(stats.major_collections >= 1, "no cycle ran");
        assert!(stats.objects_freed > 0, "a cycle reclaimed nothing");
        assert!(stats.total_allocated >= 100_000 * 32);
        drop(vm);
    }
}
