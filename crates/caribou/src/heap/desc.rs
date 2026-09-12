//! Type descriptors: what word zero of a traced object points at.
//!
//! A descriptor is not a heap object. It lives in a static or in memory the
//! defining runtime owns for the life of its type, and the collector never
//! follows word zero of a traced object.

use super::immix::Tracer;
use caribou_abi::hl::hl_type;
use std::sync::atomic::AtomicU32;

/// The precise-marking hook: mark, through `tracer`, every heap pointer the
/// object holds, in its fields or in the Rust containers it owns. Runs inside
/// a stopped world and may run on a marking thread; it must only read the
/// object.
pub type TraceFn = unsafe extern "C" fn(obj: *mut u8, tracer: *mut Tracer);

/// The reclamation hook: release what a dead object owns outside the heap.
/// Runs inside the collector, on the collecting thread, before the object's
/// lines are recycled; it must not allocate on the heap or take the GC lock.
pub type DropFn = unsafe extern "C" fn(obj: *mut u8);

/// A type descriptor. C code reading word zero of an object sees an
/// `hl_type*`; the tail is the core's.
#[repr(C)]
pub struct TypeDesc {
    /// Exactly an `hl_type`, first, so word zero is an `hl_type*` to C.
    pub hl: hl_type,
    /// Absent: the object is scanned conservatively.
    pub trace: Option<TraceFn>,
    /// Absent: nothing runs when the object dies.
    pub drop: Option<DropFn>,
    /// UTF-8, not NUL-terminated.
    pub name: *const u8,
    pub name_len: usize,
    /// Which runtime defines the type's semantics (`caribou_abi::LangId`).
    pub lang: u32,
    /// Bumped by hot reload when the type's shape changes.
    pub epoch: AtomicU32,
    /// The defining runtime's own data.
    pub ext: *mut (),
}

impl TypeDesc {
    /// A descriptor with no hooks, no name and no extension: what an
    /// `hl_type` alone would be.
    pub const fn new(hl: hl_type) -> TypeDesc {
        TypeDesc {
            hl,
            trace: None,
            drop: None,
            name: std::ptr::null(),
            name_len: 0,
            lang: 0,
            epoch: AtomicU32::new(0),
            ext: std::ptr::null_mut(),
        }
    }
}

// Immutable after construction but for `epoch`, which is atomic; the raw
// pointers are to memory the defining runtime keeps alive for the type.
unsafe impl Sync for TypeDesc {}
unsafe impl Send for TypeDesc {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn a_descriptor_begins_with_exactly_an_hl_type() {
        assert_eq!(offset_of!(TypeDesc, hl), 0);
        assert_eq!(size_of::<hl_type>(), 32);
        assert_eq!(offset_of!(TypeDesc, trace), 32);
        assert_eq!(offset_of!(TypeDesc, drop), 40);
    }
}
