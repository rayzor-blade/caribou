//! A hasher for maps keyed by an address, which the protocols consult per
//! call: one multiply, since the keys are already well spread and the
//! default hasher's resistance to crafted keys buys nothing here.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Hashes one `usize` by multiplication; other writes fold in the same way.
#[derive(Default, Clone, Copy)]
pub struct AddressHasher(u64);

impl Hasher for AddressHasher {
    #[inline]
    fn finish(&self) -> u64 {
        // The high bits carry the mix; a table indexes by the low ones.
        self.0.rotate_right(26)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0 ^ n).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }

    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.write_u64(u64::from(n));
    }
}

pub type BuildAddressHasher = BuildHasherDefault<AddressHasher>;

/// A map keyed by an address.
pub type AddressMap<V> = HashMap<usize, V, BuildAddressHasher>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_spread_and_round_trip() {
        let mut m: AddressMap<u32> = AddressMap::default();
        for i in 0..10_000usize {
            m.insert(i * 16, i as u32);
        }
        for i in 0..10_000usize {
            assert_eq!(m.get(&(i * 16)), Some(&(i as u32)));
        }
        assert_eq!(m.get(&8), None);
    }
}
