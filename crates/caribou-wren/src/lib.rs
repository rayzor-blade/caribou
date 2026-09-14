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
//!
//! The other half is the bridge (`proto.rs`): the object protocol every Wren
//! object answers through the descriptor in its prefix, the conversions
//! between wren_lift's values and the core's, and the VM the entries run on.
//! [`Runtime`] registers Wren with a world. `import` answers a Wren
//! program's `import "game:Player"` with the class the registry publishes;
//! `publish` puts a Wren module's own classes there for another language.

pub mod describe;
mod heap;
pub mod import;
pub mod project;
mod proto;
pub mod publish;
pub mod report;
pub mod types;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use caribou::world::Adapter;
use caribou_abi::LangId;
use wren_lift::runtime::rt::{RuntimeVTable, wlift_rt_install};

pub use proto::{current_vm, enter_vm, from_wren, leave_vm, to_wren, unwrap, with_vm, wrap};
pub use publish::{PublishError, publish_module};

/// Wren as a resident of a world: one language, `wren`. Registering it gives
/// Wren objects their language id; do so before the first VM allocates, and
/// after [`install`].
#[derive(Default)]
pub struct Runtime {
    lang: Option<LangId>,
}

impl Runtime {
    pub fn new() -> Runtime {
        Runtime::default()
    }

    /// The id the world assigned, once registered.
    pub fn lang(&self) -> Option<LangId> {
        self.lang
    }
}

impl Adapter for Runtime {
    fn languages(&self) -> Vec<String> {
        vec!["wren".to_owned()]
    }

    fn assign_languages(&mut self, ids: &[LangId]) {
        self.lang = ids.first().copied();
        if let Some(&id) = ids.first() {
            heap::set_wren_lang(id);
            // A module first used loads from the project's sources.
            caribou::registry::set_loader(id, std::sync::Arc::new(project::load));
        }
    }
}

/// The language id Wren objects carry: what the world assigned through
/// [`Runtime`], or the core's id before any registration.
pub fn lang() -> LangId {
    heap::wren_lang()
}

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
        alloc_plain: Some(heap::alloc_plain),
        containing_allocation: Some(heap::containing_allocation),
        is_heap_ptr: Some(heap::is_heap_ptr),
        mark_allocation: Some(heap::mark_allocation),
        is_marked: Some(heap::is_marked),
        scan_range: Some(heap::scan_range),
        track_external: Some(heap::track_external),
        watch: Some(heap::watch),
        should_collect: Some(heap::should_collect),
        collect_begin: Some(heap::collect_begin),
        collect_end: Some(heap::collect_end),
        for_each_allocation: Some(heap::for_each_allocation),
        stats: Some(heap::stats),
        stack_new: Some(heap::stack_new),
        stack_suspended: Some(heap::stack_suspended),
        stack_drop: Some(heap::stack_drop),
        ..RuntimeVTable::new()
    }
}

/// Shared by the tests that need a VM on the core heap.
#[cfg(test)]
pub(crate) mod testutil {
    use wren_lift::runtime::engine::ExecutionMode;
    use wren_lift::runtime::gc_trait::GcStrategy;
    use wren_lift::runtime::vm::{VM, VMConfig};

    const CHILD_ENV: &str = "CARIBOU_WREN_INSTALL_CHILD";

    /// The table is process-global and sealed by the first Immix VM, so a
    /// test that installs runs in a process of its own: the parent re-runs
    /// its binary with only the test at `path` selected, checks the exit,
    /// and returns true; the child returns false and goes on.
    pub(crate) fn parent_of(path: &str, env: &[(&str, &str)]) -> bool {
        if std::env::var_os(CHILD_ENV).is_some() {
            return false;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", path, "--test-threads=1"])
            .env(CHILD_ENV, "1")
            .envs(env.iter().copied())
            .status()
            .expect("re-run the test binary");
        assert!(status.success(), "child test process failed: {status}");
        true
    }

    pub(crate) fn immix_vm(mode: ExecutionMode) -> VM {
        let mut vm = VM::new(VMConfig {
            execution_mode: mode,
            gc_strategy: GcStrategy::Immix,
            ..VMConfig::default()
        });
        vm.output_buffer = Some(String::new());
        vm
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{immix_vm, parent_of};
    use super::*;
    use wren_lift::runtime::engine::{ExecutionMode, InterpretResult};
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

    #[test]
    fn install_takes_and_a_vm_then_allocates_through_the_core() {
        if parent_of(
            "tests::install_takes_and_a_vm_then_allocates_through_the_core",
            &[],
        ) {
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

    /// A core collection started on another thread waits for the VM's
    /// thread to park. The other thread is a plain mutator forcing
    /// collections while the VM allocates; a stop that never reached the VM
    /// would hold each of them for the collector's deadline and abandon it.
    #[test]
    fn a_core_collection_from_another_thread_stops_the_vm_thread() {
        if parent_of(
            "tests::a_core_collection_from_another_thread_stops_the_vm_thread",
            &[],
        ) {
            return;
        }
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};

        install().expect("a fresh process takes the table");
        // The bump path expects the heap to exist, as it does under ash.
        caribou::heap::init();
        let stop = Arc::new(AtomicBool::new(false));
        let mutator = std::thread::spawn({
            let stop = stop.clone();
            move || {
                caribou::heap::gc_register_current_os_thread();
                let mut majors = 0u64;
                let mut slowest = Duration::ZERO;
                let mut mutators = 0;
                let mut threads = [0u64; 8];
                while !stop.load(Ordering::Acquire) {
                    for _ in 0..64 {
                        std::hint::black_box(caribou::heap::gc_alloc(64));
                    }
                    let started = Instant::now();
                    caribou::heap::major();
                    slowest = slowest.max(started.elapsed());
                    majors += 1;
                    let n = unsafe { caribou::heap::registered_threads(threads.as_mut_ptr(), 8) };
                    mutators = mutators.max(n);
                    std::thread::sleep(Duration::from_millis(5));
                }
                caribou::heap::gc_unregister_current_os_thread();
                (majors, slowest, mutators)
            }
        });

        let before = caribou::heap::collections();
        let mut vm = immix_vm(ExecutionMode::Tiered);
        let result = vm.interpret(
            "main",
            // Lists of lists: a loop-local list of strings puts almost nothing
            // on the heap.
            r#"
                var start = System.clock
                var total = 0
                while (System.clock - start < 0.5) {
                    var xs = []
                    for (i in 0...1000) xs.add([i, "item-%(i)-padding"])
                    total = total + xs.count
                }
                System.print(total % 1000 == 0 && total > 0)
            "#,
        );
        let output = vm.take_output();
        let stats = vm.gc.stats();
        drop(vm);
        stop.store(true, Ordering::Release);
        let (majors, slowest, mutators) = mutator.join().unwrap();
        let grew = caribou::heap::collections() - before;

        assert_eq!(result, InterpretResult::Success);
        assert_eq!(output.trim(), "true");
        assert_eq!(mutators, 2, "the VM thread was not a registered mutator");
        assert!(
            majors >= 5,
            "the other thread collected only {majors} times"
        );
        assert!(
            slowest < Duration::from_secs(1),
            "a collection waited {slowest:?} for the VM thread: the stop never reached it"
        );
        assert!(
            grew >= majors,
            "{grew} collections completed for {majors} forced"
        );
        // Its own trigger, under a shared one the other thread kept resetting.
        assert!(
            stats.major_collections >= 1,
            "the VM ran no cycle of its own"
        );
        assert!(stats.objects_freed > 0);
    }

    /// Garbage left by a VM that polls without allocating is reclaimed on
    /// the heartbeat. Interpreted, since a compiled loop without an
    /// allocation never polls.
    #[test]
    fn idle_garbage_is_collected_on_the_heartbeat() {
        if parent_of(
            "tests::idle_garbage_is_collected_on_the_heartbeat",
            &[("CARIBOU_GC_HEARTBEAT_MS", "200")],
        ) {
            return;
        }
        install().expect("a fresh process takes the table");
        let mut vm = immix_vm(ExecutionMode::Interpreter);
        // Below the first trigger, so nothing collects for pressure.
        let result = vm.interpret(
            "alloc",
            r#"
                var xs = []
                for (i in 0...5000) xs.add("s%(i)")
                xs = null
            "#,
        );
        assert_eq!(result, InterpretResult::Success);
        assert_eq!(
            vm.gc.stats().major_collections,
            0,
            "a cycle ran before the idle"
        );

        let result = vm.interpret(
            "idle",
            r#"
                var start = System.clock
                var n = 0
                while (System.clock - start < 1.5) n = n + 1
                System.print(n > 0)
            "#,
        );
        assert_eq!(result, InterpretResult::Success);
        assert_eq!(vm.take_output().trim(), "true");
        let stats = vm.gc.stats();
        assert!(stats.major_collections >= 1, "no cycle ran during the idle");
        assert!(
            stats.objects_freed >= 5000,
            "the idle cycle freed {}",
            stats.objects_freed
        );
    }
}
