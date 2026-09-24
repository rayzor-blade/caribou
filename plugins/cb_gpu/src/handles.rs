//! Integer handles for objects that live in Rust.
//!
//! Layout: kind in bits 26..30, generation in 19..25, slot index in 0..18.
//! Zero is never a handle. The generation makes a destroyed handle raise
//! rather than reach whatever took its slot; the kind stops a buffer being
//! read as a texture. The layout is private to this plugin.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::types::Kind;

const INDEX_BITS: i32 = 19;
const GEN_BITS: i32 = 7;
const INDEX_MASK: i32 = (1 << INDEX_BITS) - 1;
const GEN_MASK: i32 = (1 << GEN_BITS) - 1;

/// The kind a handle carries, or 0 for one that is not a handle at all.
///
/// What lets a bind group take buffers, views and samplers without being told
/// which is which.
pub fn kind_of(handle: i32) -> i32 {
    if handle <= 0 {
        0
    } else {
        handle >> (INDEX_BITS + GEN_BITS)
    }
}

/// Live objects of one kind.
pub struct Slab<T> {
    kind: Kind,
    obj: Vec<Option<Arc<T>>>,
    generation: Vec<i32>,
    free: VecDeque<usize>,
}

impl<T> Slab<T> {
    pub const fn new(kind: Kind) -> Self {
        Slab {
            kind,
            obj: Vec::new(),
            generation: Vec::new(),
            free: VecDeque::new(),
        }
    }

    /// Stores `value`; 0 if this kind is full.
    pub fn put(&mut self, value: T) -> i32 {
        let index = match self.free.pop_front() {
            // Oldest free slot first: taking the newest would cycle one slot's
            // generation back to a value a stale handle still holds.
            Some(i) => {
                self.generation[i] = (self.generation[i] + 1) & GEN_MASK;
                i
            }
            None => {
                let i = self.obj.len();
                if i as i32 > INDEX_MASK {
                    return 0;
                }
                self.obj.push(None);
                self.generation.push(0);
                i
            }
        };
        self.obj[index] = Some(Arc::new(value));
        ((self.kind as i32) << (INDEX_BITS + GEN_BITS))
            | (self.generation[index] << INDEX_BITS)
            | index as i32
    }

    /// The object, if the handle is live and of this kind.
    pub fn get(&self, handle: i32) -> Option<Arc<T>> {
        let index = self.slot(handle)?;
        self.obj[index].clone()
    }

    /// Releases a native resource. Repeated explicit destruction is harmless.
    pub fn remove(&mut self, handle: i32) {
        let Some(index) = self.slot(handle) else {
            return;
        };
        if self.obj[index].take().is_some() {
            self.free.push_back(index);
        }
    }

    /// Live handles of this kind. What a leak check reads.
    #[cfg(test)]
    pub fn live(&self) -> usize {
        self.obj.iter().filter(|slot| slot.is_some()).count()
    }

    /// The slot a handle names, if kind and generation both match.
    fn slot(&self, handle: i32) -> Option<usize> {
        if handle <= 0 || handle >> (INDEX_BITS + GEN_BITS) != self.kind as i32 {
            return None;
        }
        let index = (handle & INDEX_MASK) as usize;
        if self.generation.get(index).copied()? != (handle >> INDEX_BITS) & GEN_MASK {
            return None;
        }
        Some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slab() -> Slab<i32> {
        Slab::new(Kind::Buffer)
    }

    #[test]
    fn a_handle_finds_what_was_put_in_it() {
        let mut s = slab();
        let h = s.put(7);
        assert_ne!(h, 0, "a handle is never zero");
        assert_eq!(s.get(h).as_deref(), Some(&7));
        assert_eq!(s.live(), 1);
    }

    #[test]
    fn a_destroyed_handle_stops_resolving() {
        let mut s = slab();
        let h = s.put(7);
        s.remove(h);
        assert!(s.get(h).is_none());
        assert_eq!(s.live(), 0);
        // Repeated destruction must be harmless.
        s.remove(h);
    }

    #[test]
    fn a_reused_slot_does_not_answer_the_old_handle() {
        let mut s = slab();
        let old = s.put(7);
        s.remove(old);
        let new = s.put(9);
        assert_ne!(old, new, "the generation makes the handle different");
        assert!(s.get(old).is_none());
        assert_eq!(s.get(new).as_deref(), Some(&9));
    }

    #[test]
    fn a_handle_of_another_kind_is_refused() {
        let mut buffers = slab();
        let h = buffers.put(7);
        let textures: Slab<i32> = Slab::new(Kind::Texture);
        assert!(
            textures.get(h).is_none(),
            "kind is checked, not just the slot"
        );
    }

    #[test]
    fn slots_are_reused_oldest_first() {
        // Taking the newest free slot would cycle one slot's generation back
        // to a value a stale handle still holds.
        let mut s = slab();
        let (a, b) = (s.put(1), s.put(2));
        s.remove(a);
        s.remove(b);
        assert_eq!(s.put(3) & INDEX_MASK, a & INDEX_MASK);
        assert_eq!(s.put(4) & INDEX_MASK, b & INDEX_MASK);
    }

    #[test]
    fn a_handle_says_what_kind_it_is() {
        let mut s = slab();
        let h = s.put(1);
        assert_eq!(kind_of(h), Kind::Buffer as i32);
        assert_eq!(kind_of(0), 0, "and zero is nothing");
    }
}
