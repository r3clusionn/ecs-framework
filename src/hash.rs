//! A fast hasher for the small keys used here (type ids, component ids, archetype keys). The
//! standard SipHash is built to resist hash flooding, which these internal tables do not need.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

const K: u64 = 0x517c_c1b7_2722_0a95;

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for c in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..c.len()].copy_from_slice(c);
            self.write_u64(u64::from_le_bytes(b));
        }
    }
    fn write_u32(&mut self, i: u32) {
        self.write_u64(i as u64);
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(K);
    }
    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;
