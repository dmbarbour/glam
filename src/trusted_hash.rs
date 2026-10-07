//! Hash maps for keys the runtime allocates itself.
//!
//! Node, port, copy, fan-site, work, wait, session and task identities come
//! from runtime counters, so a Glam program cannot choose them to provoke
//! collisions. These maps use a minimal integer hasher instead of the
//! standard library's SipHash. It has no collision resistance, which is
//! acceptable only for such keys.
//!
//! **The key type is checked.** `TrustedState<K>` requires `K: TrustedKey`,
//! so a trusted map keyed by anything else does not compile. Every
//! `TrustedKey` implementation lives in this file with its reason, so adding
//! one is the point where the claim is reviewed. Keys that a Glam program can
//! influence, such as dictionary keys and names, keep the standard
//! `RandomState`.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hash, Hasher};
use std::marker::PhantomData;

/// An identity the runtime allocates itself, from a counter or an
/// allocation, which a Glam program cannot choose. Only these may key a
/// trusted map.
pub(crate) trait TrustedKey: Hash + Eq {}

// Runtime counters, one per id space; never derived from program data.
impl TrustedKey for crate::evaluation::EvaluationWorkId {}
impl TrustedKey for crate::evaluation::EvaluationTaskId {}
impl TrustedKey for crate::evaluation::EvaluationSessionId {}
// Hashes its wait state's counter id.
impl TrustedKey for crate::evaluation::EvaluationWaitToken {}
// The managed lazy or promise cell's runtime id.
impl TrustedKey for crate::core::DeferredValueId {}
// Interaction-net node, port, copy and fan-site counters.
impl TrustedKey for crate::interaction_net::NodeId {}
impl TrustedKey for crate::interaction_net::Port {}
impl TrustedKey for crate::interaction_net::CopyId {}
impl TrustedKey for crate::interaction_net::FanSite {}
// Counter ids of external owners, effect tokens and captured continuations.
impl TrustedKey for crate::core::ExternalOwnerKey {}
impl TrustedKey for crate::api::EffectTokenKey {}
impl TrustedKey for crate::reflection::ContinuationKey {}

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
pub(crate) type TrustedHashSet<K> = HashSet<K, TrustedState<K>>;

/// One widening multiply per integer written, folded.
///
/// Folding the product's high half into its low half carries every key bit
/// into the low bits that `hashbrown` uses to pick a bucket. Strided keys
/// therefore spread as well as consecutive counters do; a plain multiply
/// would leave their low bits constant.
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
    #[inline(always)]
    fn finish(&self) -> u64 {
        self.0
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
    use super::*;

    fn hash(value: u64) -> u64 {
        let mut hasher = IdHasher::default();
        hasher.write_u64(value);
        hasher.finish()
    }

    /// Distinct low 12-bit buckets among 4,096 keys. Uniform hashing fills
    /// about 63% of them (1 - 1/e).
    fn low_bucket_spread(keys: impl Iterator<Item = u64>) -> usize {
        keys.map(|key| hash(key) & 0xfff)
            .collect::<HashSet<_>>()
            .len()
    }

    #[test]
    fn counters_and_strided_keys_spread_across_buckets() {
        let counters = low_bucket_spread(1..=4096);
        let strided = low_bucket_spread((1..=4096).map(|key| key * 64));
        assert!(counters > 2_400, "counters fill {counters} of 4096 buckets");
        assert!(
            strided > 2_400,
            "strided keys fill {strided} of 4096 buckets"
        );
    }

    /// Every trusted key is declared in this file, where the claim that a
    /// program cannot choose it is reviewed.
    #[test]
    fn trusted_keys_are_declared_only_here() {
        let mut pending = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        let this_file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file!());
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
                         with the reason a program cannot choose it",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn hashing_is_deterministic() {
        assert_eq!(hash(42), hash(42));
        assert_ne!(hash(1), hash(2));
    }
}
