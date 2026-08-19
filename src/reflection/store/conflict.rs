//! Conflict-address vocabulary and pluggable read-observation strategies.
//!
//! Strategies summarize reads only. The parent store retains exact edit paths,
//! so strategy selection cannot change edit or rebase semantics.

use std::collections::{BTreeSet, hash_map::RandomState};
use std::hash::BuildHasher;
use std::sync::Arc;

use crate::core::Key;

use super::VolumeId;

/// A hierarchical address in the shared reflection heap.
///
/// Core keys remain an implementation detail, but paths can be retained in
/// custom indexes and compared for exact or ancestor relationships.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConflictPath(Arc<[Key]>);

impl ConflictPath {
    pub fn root() -> Self {
        Self(Arc::from([]))
    }

    pub fn depth(&self) -> usize {
        self.0.len()
    }

    pub fn is_prefix_of(&self, other: &Self) -> bool {
        other.0.starts_with(&self.0)
    }

    pub fn overlaps(&self, other: &Self) -> bool {
        self.is_prefix_of(other) || other.is_prefix_of(self)
    }

    pub(super) fn from_keys(keys: Vec<Key>) -> Self {
        Self(Arc::from(keys))
    }

    fn prefixes(&self) -> impl Iterator<Item = Self> + '_ {
        (0..=self.0.len()).map(|length| Self(Arc::from(&self.0[..length])))
    }

    pub(super) fn keys(&self) -> &[Key] {
        &self.0
    }
}

impl std::fmt::Debug for ConflictPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ConflictPath")
            .field(&self.0)
            .finish()
    }
}

/// An address understood by the runtime's conflict-analysis strategy.
///
/// Reflection paths retain hierarchical overlap within one volume. Buffered
/// runtime inputs use their own FIFO cursor validation rather than this
/// hierarchical store policy.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConflictAddress {
    Reflection {
        volume: VolumeId,
        path: ConflictPath,
    },
}

impl ConflictAddress {
    pub(super) fn reflection(volume: VolumeId, path: ConflictPath) -> Self {
        Self::Reflection { volume, path }
    }

    pub(super) fn reflection_root(volume: VolumeId) -> Self {
        Self::reflection(volume, ConflictPath::root())
    }

    pub(super) fn is_prefix_of(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Reflection {
                    volume: left_volume,
                    path: left_path,
                },
                Self::Reflection {
                    volume: right_volume,
                    path: right_path,
                },
            ) => left_volume == right_volume && left_path.is_prefix_of(right_path),
        }
    }

    fn overlaps(&self, other: &Self) -> bool {
        self.is_prefix_of(other) || other.is_prefix_of(self)
    }

    fn prefixes(&self) -> Vec<Self> {
        match self {
            Self::Reflection { volume, path } => path
                .prefixes()
                .map(|path| Self::reflection(*volume, path))
                .collect(),
        }
    }

    pub(super) fn reflection_parts(&self) -> (VolumeId, &ConflictPath) {
        match self {
            Self::Reflection { volume, path } => (*volume, path),
        }
    }
}

/// Creates the read index used by one optimistic transaction.
pub trait ConflictAnalysisStrategy: Send + Sync {
    fn begin(&self) -> Box<dyn ConflictObservationIndex>;

    /// A stable descriptive name for diagnostics and configuration displays.
    fn name(&self) -> &'static str;
}

/// A cloneable summary of paths observed by one transaction branch.
pub trait ConflictObservationIndex: Send + Sync {
    fn clone_box(&self) -> Box<dyn ConflictObservationIndex>;
    fn observe(&mut self, address: &ConflictAddress);
    fn may_conflict(&self, changed: &ConflictAddress) -> bool;
}

impl Clone for Box<dyn ConflictObservationIndex> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Exact path-overlap analysis. This is the reference implementation.
#[derive(Debug, Default)]
pub struct ExactConflictAnalysis;

impl ConflictAnalysisStrategy for ExactConflictAnalysis {
    fn begin(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(ExactObservationIndex::default())
    }

    fn name(&self) -> &'static str {
        "exact"
    }
}

#[derive(Clone, Default)]
struct ExactObservationIndex {
    addresses: BTreeSet<ConflictAddress>,
}

impl ConflictObservationIndex for ExactObservationIndex {
    fn clone_box(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(self.clone())
    }

    fn observe(&mut self, address: &ConflictAddress) {
        if self.addresses.iter().any(|seen| seen.is_prefix_of(address)) {
            return;
        }
        self.addresses.retain(|seen| !address.is_prefix_of(seen));
        self.addresses.insert(address.clone());
    }

    fn may_conflict(&self, changed: &ConflictAddress) -> bool {
        self.addresses.iter().any(|read| read.overlaps(changed))
    }
}

/// Conservative fingerprint analysis. Hash collisions cause retries, never
/// missed conflicts.
#[derive(Debug, Default)]
pub struct FingerprintConflictAnalysis;

impl ConflictAnalysisStrategy for FingerprintConflictAnalysis {
    fn begin(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(FingerprintObservationIndex {
            hash_builder: RandomState::new(),
            complete_reads: BTreeSet::new(),
            read_prefixes: BTreeSet::new(),
        })
    }

    fn name(&self) -> &'static str {
        "fingerprint"
    }
}

#[derive(Clone)]
struct FingerprintObservationIndex {
    hash_builder: RandomState,
    complete_reads: BTreeSet<u64>,
    read_prefixes: BTreeSet<u64>,
}

impl FingerprintObservationIndex {
    fn fingerprint(&self, address: &ConflictAddress) -> u64 {
        self.hash_builder.hash_one(address)
    }
}

impl ConflictObservationIndex for FingerprintObservationIndex {
    fn clone_box(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(self.clone())
    }

    fn observe(&mut self, address: &ConflictAddress) {
        self.complete_reads.insert(self.fingerprint(address));
        for prefix in address.prefixes() {
            self.read_prefixes.insert(self.fingerprint(&prefix));
        }
    }

    fn may_conflict(&self, changed: &ConflictAddress) -> bool {
        self.read_prefixes.contains(&self.fingerprint(changed))
            || changed
                .prefixes()
                .into_iter()
                .any(|prefix| self.complete_reads.contains(&self.fingerprint(&prefix)))
    }
}

/// Coarse analysis matching the former host-generation behavior: once a
/// transaction reads the heap, every committed heap write conflicts.
#[derive(Debug, Default)]
pub struct CoarseConflictAnalysis;

impl ConflictAnalysisStrategy for CoarseConflictAnalysis {
    fn begin(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(CoarseObservationIndex(false))
    }

    fn name(&self) -> &'static str {
        "coarse"
    }
}

#[derive(Clone)]
struct CoarseObservationIndex(bool);

impl ConflictObservationIndex for CoarseObservationIndex {
    fn clone_box(&self) -> Box<dyn ConflictObservationIndex> {
        Box::new(self.clone())
    }

    fn observe(&mut self, _address: &ConflictAddress) {
        self.0 = true;
    }

    fn may_conflict(&self, _changed: &ConflictAddress) -> bool {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(parts: &[&str]) -> Vec<Key> {
        parts.iter().map(Key::atom_from_text).collect()
    }

    fn address(volume: VolumeId, parts: &[&str]) -> ConflictAddress {
        ConflictAddress::reflection(volume, ConflictPath::from_keys(path(parts)))
    }

    #[test]
    fn exact_strategy_detects_both_overlap_directions() {
        let strategy = ExactConflictAnalysis;
        let mut observations = strategy.begin();
        let volume = VolumeId::from_u64(1).unwrap();
        observations.observe(&address(volume, &["a", "b"]));
        assert!(observations.may_conflict(&address(volume, &["a"])));
        assert!(observations.may_conflict(&address(volume, &["a", "b", "c"])));
        assert!(!observations.may_conflict(&address(volume, &["z"])));
    }

    #[test]
    fn fingerprint_strategy_is_conservative_for_path_overlap() {
        let strategy = FingerprintConflictAnalysis;
        let mut observations = strategy.begin();
        let volume = VolumeId::from_u64(1).unwrap();
        observations.observe(&address(volume, &["a", "b"]));
        assert!(observations.may_conflict(&address(volume, &["a"])));
        assert!(observations.may_conflict(&address(volume, &["a", "b", "c"])));
    }

    #[test]
    fn exact_strategy_treats_distinct_volumes_as_disjoint() {
        let strategy = ExactConflictAnalysis;
        let mut observations = strategy.begin();
        let first = VolumeId::from_u64(1).unwrap();
        let second = VolumeId::from_u64(2).unwrap();
        observations.observe(&address(first, &["same"]));

        assert!(!observations.may_conflict(&address(second, &["same"])));
    }

    #[test]
    fn fingerprint_strategy_treats_distinct_volumes_as_disjoint() {
        let strategy = FingerprintConflictAnalysis;
        let mut observations = strategy.begin();
        let first = VolumeId::from_u64(1).unwrap();
        let second = VolumeId::from_u64(2).unwrap();
        observations.observe(&address(first, &["same"]));

        assert!(!observations.may_conflict(&address(second, &["same"])));
    }

    #[test]
    fn coarse_strategy_conflicts_after_any_observation() {
        let strategy = CoarseConflictAnalysis;
        let mut observations = strategy.begin();
        let volume = VolumeId::from_u64(1).unwrap();
        assert!(!observations.may_conflict(&address(volume, &[])));
        observations.observe(&address(volume, &["a"]));
        assert!(observations.may_conflict(&address(volume, &["z"])));
    }
}
