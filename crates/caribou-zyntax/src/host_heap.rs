//! The core heap as Zyntax's: the table Zyntax's allocation entry points
//! delegate to once it is installed (`zyntax_compiler::host_heap`). What a
//! program allocates comes from the core heap and the core's collector
//! reclaims what drop insertion does not free; what the runtime keeps from
//! its own tables is held, rooted until the runtime frees it; Zyntax's
//! roots and threads are the core collector's.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{LazyLock, Mutex, Once};

use caribou::heap::{self, Handle};
use zyntax_compiler::host_heap::{HOST_HEAP_VERSION, HeapKind, HostHeap, SpanReader};

/// The held blocks, each rooted by its handle until freed.
static HELD: LazyLock<Mutex<HashMap<usize, Handle>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

unsafe extern "C" fn alloc(_cx: *mut c_void, size: usize, kind: HeapKind) -> *mut u8 {
    // Zeroed and 16-byte aligned, as the core heap gives every block.
    let Some(block) = heap::gc_alloc(size.max(1)) else {
        return std::ptr::null_mut();
    };
    let block = block.as_ptr();
    if kind == HeapKind::Held {
        HELD.lock()
            .unwrap()
            .insert(block as usize, heap::handle_new(block));
    }
    block
}

unsafe extern "C" fn free(_cx: *mut c_void, ptr: *mut u8) {
    if let Some(handle) = HELD.lock().unwrap().remove(&(ptr as usize)) {
        heap::handle_release(handle);
    }
    // Only the start of a live block: one the collector already reclaimed,
    // or an interior address, is left alone.
    if heap::containing_allocation(ptr as usize).is_some_and(|(start, _)| start == ptr as usize) {
        unsafe { heap::free_allocation(ptr) };
    }
}

unsafe extern "C" fn realloc(cx: *mut c_void, ptr: *mut u8, size: usize) -> *mut u8 {
    let kind = if HELD.lock().unwrap().contains_key(&(ptr as usize)) {
        HeapKind::Held
    } else {
        HeapKind::Collected
    };
    let old = unsafe { heap::allocation_size(ptr.cast()) };
    let block = unsafe { alloc(cx, size, kind) };
    if block.is_null() {
        return block;
    }
    // The new block is zeroed, so what it gains past the old is zero.
    unsafe { std::ptr::copy_nonoverlapping(ptr, block, old.min(size)) };
    unsafe { free(cx, ptr) };
    block
}

unsafe extern "C" fn owns(_cx: *mut c_void, ptr: *const u8) -> bool {
    heap::in_heap(ptr.cast())
}

unsafe extern "C" fn add_root_range(_cx: *mut c_void, start: *const u8, len: usize) {
    unsafe { heap::add_root_range(start, len) };
}

unsafe extern "C" fn add_root_spans(
    _cx: *mut c_void,
    start: *const u8,
    len: usize,
    reader: SpanReader,
) {
    unsafe { heap::add_root_spans(start, len, reader) };
}

unsafe extern "C" fn remove_root_range(_cx: *mut c_void, start: *const u8) {
    heap::remove_root(start);
}

unsafe extern "C" fn thread_enter(_cx: *mut c_void, stack_top: *const u8) {
    unsafe { heap::register_thread(stack_top.cast_mut().cast()) };
}

unsafe extern "C" fn thread_leave(_cx: *mut c_void) {
    heap::unregister_thread();
}

/// Make the core heap Zyntax's, once per process, before the first
/// runtime is made.
pub fn install() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let table = HostHeap {
            version: HOST_HEAP_VERSION,
            size: std::mem::size_of::<HostHeap>() as u32,
            context: std::ptr::null_mut(),
            alloc,
            free,
            realloc,
            owns,
            add_root_range,
            add_root_spans,
            remove_root_range,
            thread_enter,
            thread_leave,
        };
        // SAFETY: every slot is callable from any thread for the rest of
        // the process, and there is no context to outlive.
        if let Err(e) = unsafe { zyntax_compiler::host_heap::install(&table) } {
            eprintln!("caribou: zyntax keeps its own heap: {e}");
        }
    });
}
