//! Hosts Ash on the caribou core.
//!
//! Ash depends on nothing; `ash_std::rt` is its runtime seam, a table of
//! every entry point its own code reaches its heap and scheduler through.
//! This crate fills that table with the core's heap (`heap.rs`) and
//! scheduler (`sched.rs`) and installs it before Ash's heap exists, so a
//! Haxe program's objects live in the core's heap and its threads are the
//! core's tasks. Nothing in Ash names this crate.
//!
//! The other half is the bridge (`proto.rs`): the typed dispatcher for
//! Haxe callables, the wrapper a Haxe object crosses in and the protocol it
//! answers; and `wrenref.rs`, the ref Haxe holds another language's object
//! through. [`Runtime`] registers Haxe with a world and the dispatcher with
//! the bridge. With the `runner` feature, `program` loads a `.hl` on ash's
//! interpreter, runs it and publishes its classes to the registry.
//!
//! Builds with `cargo +nightly`: ash_std needs it. The core stays stable.

mod heap;
mod import;
#[cfg(feature = "runner")]
pub mod program;
mod proto;
mod sched;
mod wrenref;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use ash_std::rt::RuntimeVTable;
use caribou::world::Adapter;
use caribou_abi::LangId;

#[cfg(feature = "runner")]
pub use program::{Mode, Options, Program, load, publish_module};
pub use proto::{construct, is_constructor, lang, unwrap, wrap};
pub use wrenref::{
    foreign_ref, unwrap_foreign, wrap_foreign, wrenref_as_abstract, wrenref_from_abstract,
};

/// Haxe as a resident of a world: one language, `haxe`. Registering it
/// gives Haxe objects their language id and the bridge its typed
/// dispatcher; do so before any Haxe object crosses.
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
        vec!["haxe".to_owned()]
    }

    fn assign_languages(&mut self, ids: &[LangId]) {
        let Some(&id) = ids.first() else {
            return;
        };
        self.lang = Some(id);
        proto::set_lang(id);
        wrenref::set_lang(id);
        caribou::bridge::set_typed_dispatch(id, proto::dispatch);
        // The dynamic-call hook `hlp_dyn_call` reaches native code through;
        // ash's own startup installs the same one, so this is idempotent.
        unsafe { ash_std::fun::hlp_install_static_call() };
    }
}

/// Why an install did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallError {
    /// `hlp_rt_install` returned false: the runtime's heap already exists,
    /// or its table has another version or size than this crate was built
    /// against.
    Refused,
    /// The hosted image lacks an export the seam needs.
    Missing(&'static str),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused => {
                f.write_str("ash refused the runtime table: its heap already exists or the table version differs")
            }
            Self::Missing(name) => write!(f, "the hosted ash_std exports no {name}"),
        }
    }
}

impl std::error::Error for InstallError {}

/// The entry points of the ash_std image being hosted. The linked copy is
/// the usual one; a program with HDLLs makes ash dlopen a second copy
/// beside the executable, and that image's exports are what count then.
pub struct Seam {
    install: unsafe extern "C" fn(*const RuntimeVTable) -> bool,
    switch_hook: sched::SwitchHookGetter,
    exc_swap: sched::ExcSwap,
}

impl Seam {
    /// The ash_std this crate links.
    pub fn linked() -> Self {
        Self {
            install: ash_std::rt::hlp_rt_install,
            switch_hook: ash_std::rt::hlp_rt_switch_hook,
            exc_swap: ash_std::rt::hlp_rt_exc_swap,
        }
    }

    /// Another image's exports, by name.
    ///
    /// # Safety
    /// Every address `lookup` returns must be that export of an ash_std
    /// built against the same `RuntimeVTable`.
    pub unsafe fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<usize>,
    ) -> Result<Self, InstallError> {
        let mut want = |name: &'static str| lookup(name).ok_or(InstallError::Missing(name));
        let install = want("hlp_rt_install")?;
        let switch_hook = want("hlp_rt_switch_hook")?;
        let exc_swap = want("hlp_rt_exc_swap")?;
        // SAFETY: the caller's contract, above.
        unsafe {
            Ok(Self {
                install: std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(*const RuntimeVTable) -> bool,
                >(install),
                switch_hook: std::mem::transmute::<usize, sched::SwitchHookGetter>(switch_hook),
                exc_swap: std::mem::transmute::<usize, sched::ExcSwap>(exc_swap),
            })
        }
    }
}

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install the core's heap and scheduler into the linked ash_std. Must run
/// before Ash allocates. A second call after a successful one is `Ok`.
pub fn install() -> Result<(), InstallError> {
    install_into(Seam::linked())
}

/// [`install`], into the image `seam` names.
pub fn install_into(seam: Seam) -> Result<(), InstallError> {
    if INSTALLED.load(Ordering::Acquire) {
        return Ok(());
    }
    sched::set_hooks(sched::AshHooks {
        switch_hook: seam.switch_hook,
        exc_swap: seam.exc_swap,
    });
    let table = table();
    // SAFETY: the table outlives the call, which copies its entries.
    if !unsafe { (seam.install)(&table) } {
        return Err(InstallError::Refused);
    }
    INSTALLED.store(true, Ordering::Release);
    Ok(())
}

/// Whether [`install`] or [`install_into`] has succeeded in this process.
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

/// Every slot, filled: a `None` would leave Ash's own implementation
/// sharing a process with the core's.
fn table() -> RuntimeVTable {
    RuntimeVTable {
        version: ash_std::rt::RT_VERSION,
        size: std::mem::size_of::<RuntimeVTable>() as u32,
        // Heap.
        gc_alloc: Some(heap::gc_alloc),
        alloc_locked: Some(heap::alloc_locked),
        alloc_immortal: Some(heap::alloc_immortal),
        alloc_with_finalizer: Some(heap::alloc_with_finalizer),
        allocation_size: Some(heap::allocation_size),
        is_gc_ptr: Some(heap::is_gc_ptr),
        out_of_memory: Some(heap::out_of_memory),
        gc_safepoint: Some(heap::gc_safepoint),
        gc_set_blocking: Some(heap::gc_set_blocking),
        mark_site: Some(heap::mark_site),
        gc_register_current_os_thread: Some(heap::gc_register_current_os_thread),
        gc_unregister_current_os_thread: Some(heap::gc_unregister_current_os_thread),
        gc_register_fiber_stack: Some(heap::gc_register_fiber_stack),
        gc_update_fiber_sp: Some(heap::gc_update_fiber_sp),
        gc_unregister_fiber_stack: Some(heap::gc_unregister_fiber_stack),
        gc_add_persistent: Some(heap::gc_add_persistent),
        gc_remove_persistent: Some(heap::gc_remove_persistent),
        add_root_slot: Some(heap::add_root_slot),
        remove_root_slot: Some(heap::remove_root_slot),
        gc_lock: Some(heap::gc_lock),
        gc_unlock: Some(heap::gc_unlock),
        gc_lock_held_depth: Some(heap::gc_lock_held_depth),
        gc_lock_unwind_to: Some(heap::gc_lock_unwind_to),
        gc_registered_threads: Some(heap::gc_registered_threads),
        gc_print_stats: Some(heap::gc_print_stats),
        mark_size: Some(heap::mark_size),
        gc_walk_heap: Some(heap::gc_walk_heap),
        gc_init: Some(heap::gc_init),
        gc_set_stack_top: Some(heap::gc_set_stack_top),
        register_thread: Some(heap::register_thread),
        unregister_thread: Some(heap::unregister_thread),
        gc_set_globals: Some(heap::gc_set_globals),
        gc_scan_roots_done: Some(heap::gc_scan_roots_done),
        gc_clear_scan_roots: Some(heap::gc_clear_scan_roots),
        gc_add_scan_root: Some(heap::gc_add_scan_root),
        gc_set_scan_roots_live: Some(heap::gc_set_scan_roots_live),
        gc_set_scan_roots: Some(heap::gc_set_scan_roots),
        gc_track_external: Some(heap::gc_track_external),
        gc_enable: Some(heap::gc_enable),
        gc_get_flags: Some(heap::gc_get_flags),
        gc_set_flags: Some(heap::gc_set_flags),
        gc_major: Some(heap::gc_major),
        gc_stats: Some(heap::gc_stats),
        gc_profile: Some(heap::gc_profile),
        gc_get_live_objects: Some(heap::gc_get_live_objects),
        gc_dump_memory: Some(heap::gc_dump_memory),
        // Scheduler.
        new_waiter: Some(sched::new_waiter),
        wake: Some(sched::wake),
        park: Some(sched::park),
        sleep_ns: Some(sched::sleep_ns),
        block_yield: Some(sched::block_yield),
        schedule_step: Some(sched::schedule_step),
        thread_create: Some(sched::thread_create),
        fiber_poll: Some(sched::fiber_poll),
        fibers_active: Some(sched::fibers_active),
        current_id: Some(sched::current_id),
        current_handle: Some(sched::current_handle),
        current_owner: Some(sched::current_owner),
        current_ctx: Some(sched::current_ctx),
        update_gc_blocking_depth: Some(sched::update_gc_blocking_depth),
        is_gc_blocking: Some(sched::is_gc_blocking),
        request_fiber_poll: Some(sched::request_fiber_poll),
        fiber_poll_epoch_address: Some(sched::fiber_poll_epoch_address),
        is_worker_lane: Some(sched::is_worker_lane),
        mark_main_thread: Some(sched::mark_main_thread),
        is_main_thread: Some(sched::is_main_thread),
        foreign_threads_seen: Some(sched::foreign_threads_seen),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header is one word; every word after it is a slot.
    #[test]
    fn every_slot_is_filled() {
        let table = table();
        let words = std::mem::size_of::<RuntimeVTable>() / std::mem::size_of::<usize>();
        // SAFETY: the table is repr(C) of u32, u32 and word-sized nullable
        // function pointers, read here as plain words.
        let raw = unsafe {
            std::slice::from_raw_parts(&table as *const RuntimeVTable as *const usize, words)
        };
        for (i, word) in raw.iter().enumerate().skip(1) {
            assert_ne!(*word, 0, "slot {i} is None");
        }
    }

    const CHILD_ENV: &str = "CARIBOU_ASH_INSTALL_CHILD";

    /// The table is process-global and sealed by the first heap init, so
    /// the install runs in a process of its own: the test re-runs its
    /// binary with only itself selected and checks the exit.
    #[test]
    fn install_takes_and_is_idempotent() {
        if std::env::var_os(CHILD_ENV).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::install_takes_and_is_idempotent",
                    "--test-threads=1",
                ])
                .env(CHILD_ENV, "1")
                .status()
                .expect("re-run the test binary");
            assert!(status.success(), "child test process failed: {status}");
            return;
        }

        assert!(!ash_std::rt::hlp_rt_installed());
        assert!(!installed());
        install().expect("a fresh process takes the table");
        assert!(ash_std::rt::hlp_rt_installed());
        assert!(installed());

        // Ash's heap now exists through the core; the seam is sealed.
        unsafe { ash_std::gc::hlp_gc_init() };
        assert!(!unsafe { ash_std::rt::hlp_rt_install(&table()) });
        install().expect("a second install after a successful one is Ok");
        assert!(ash_std::rt::gc_alloc(64).is_some());
    }
}
