//! Hash maps for keys the collector derives from its own allocations and
//! types.
//!
//! Heap and chunk addresses, metadata pointers and type ids never come from
//! data a collector client stores, so nobody can choose them to provoke
//! collisions. These maps use a minimal integer hasher instead of the
//! standard library's SipHash. It has no collision resistance, which is
//! acceptable only for such keys.
//!
//! `TrustedState<K>` requires `K: TrustedKey`, and every `TrustedKey`
//! implementation lives in this file with its reason, so adding a key type is
//! a reviewed change. The `glam` crate keeps the same design for its runtime
//! ids in its own `trusted_hash` module.

use std::any::TypeId;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, Hasher};
use std::marker::PhantomData;

/// A key derived from the collector's own allocations or types.
pub(crate) trait TrustedKey: Hash + Eq {}

// The address of a heap's shared state.
impl TrustedKey for crate::thread_cache::HeapCacheKey {}
// The aligned base address of an arena chunk.
impl TrustedKey for crate::arena::ChunkBase {}
// Compiler-assigned type identities of managed types.
impl TrustedKey for TypeId {}
// The address of a type's static metadata.
impl TrustedKey for crate::class::MetadataIdentity {}

/// The hasher state for one trusted key type.
pub(crate) struct TrustedState<K: TrustedKey>(PhantomData<fn(&K)>);

impl<K: TrustedKey> Default for TrustedState<K> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<K: TrustedKey> Clone for TrustedState<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K: TrustedKey> Copy for TrustedState<K> {}

impl<K: TrustedKey> std::fmt::Debug for TrustedState<K> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TrustedState")
    }
}

impl<K: TrustedKey> BuildHasher for TrustedState<K> {
    type Hasher = IdHasher;

    fn build_hasher(&self) -> IdHasher {
        IdHasher(0)
    }
}

pub(crate) type TrustedHashMap<K, V> = HashMap<K, V, TrustedState<K>>;

/// One widening multiply per integer written, folded, then one rotation.
///
/// Folding the product's high half into its low half carries every key bit
/// into the low bits that `hashbrown` uses to pick a bucket, so aligned
/// addresses spread as well as counters do.
#[derive(Debug, Default)]
pub(crate) struct IdHasher(u64);

impl IdHasher {
    /// The 64-bit golden-ratio constant: odd, with well-spread bits.
    const MULTIPLIER: u64 = 0x9e37_79b9_7f4a_7c15;

    #[inline(always)]
    fn mix(&mut self, value: u64) {
        let product = u128::from(self.0 ^ value) * u128::from(Self::MULTIPLIER);
        self.0 = (product as u64) ^ ((product >> 64) as u64);
    }
}

impl Hasher for IdHasher {
    /// The rotation moves the product's well-mixed high bits into the low
    /// bits, which keeps large power-of-two strides from clustering.
    #[inline(always)]
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }

    /// Trusted keys hash as integers; this path only keeps the hasher total.
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.mix(u64::from_le_bytes(word));
        }
    }

    #[inline(always)]
    fn write_u8(&mut self, value: u8) {
        self.mix(u64::from(value));
    }

    #[inline(always)]
    fn write_u16(&mut self, value: u16) {
        self.mix(u64::from(value));
    }

    #[inline(always)]
    fn write_u32(&mut self, value: u32) {
        self.mix(u64::from(value));
    }

    #[inline(always)]
    fn write_u64(&mut self, value: u64) {
        self.mix(value);
    }

    #[inline(always)]
    fn write_u128(&mut self, value: u128) {
        self.mix(value as u64);
        self.mix((value >> 64) as u64);
    }

    #[inline(always)]
    fn write_usize(&mut self, value: usize) {
        self.mix(value as u64);
    }

    #[inline(always)]
    fn write_isize(&mut self, value: isize) {
        self.mix(value as u64);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn hash(value: u64) -> u64 {
        let mut hasher = IdHasher::default();
        hasher.write_u64(value);
        hasher.finish()
    }

    #[test]
    fn aligned_addresses_spread_across_buckets() {
        // Uniform hashing fills about 63% of 4,096 buckets with 4,096 keys.
        for stride_bits in [3, 6, 12, 20, 30] {
            let spread = (1..=4096)
                .map(|key| hash(key << stride_bits) & 0xfff)
                .collect::<HashSet<_>>()
                .len();
            assert!(
                spread > 2_400,
                "keys strided by 2^{stride_bits} fill {spread} of 4096 buckets"
            );
        }
    }

    /// Every trusted key is declared in this file, where the claim that no
    /// client chooses it is reviewed.
    #[test]
    fn trusted_keys_are_declared_only_here() {
        let mut pending = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        let this_file =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/trusted_hash.rs");
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).expect("source directories are readable") {
                let path = entry.expect("source entries are readable").path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs")
                    && path != this_file
                {
                    let source = std::fs::read_to_string(&path).expect("sources are readable");
                    assert!(
                        !source.contains("TrustedKey for"),
                        "{} declares a trusted key; declare it in src/trusted_hash.rs \
                         with the reason no client chooses it",
                        path.display()
                    );
                }
            }
        }
    }
}
