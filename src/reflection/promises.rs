//! Promises that machines and reflection allocate and resolve
//! (`machines-inherit-an-environment`, `requests-travel-on-ports`,
//! `promise-cells-are-weakly-resolved`).
//!
//! A promise has one identity, its cell's ID. Its resolver is an edge to its
//! breaker, which keeps its status, and the store holds each pending
//! promise's cell only weakly, by that ID: the promised value alone keeps
//! its cell, so the store holds no promise alive. A promise's status is a
//! read of its breaker; within a transaction a read observes the promise's
//! place in the runtime's volume, which its resolution writes, so a
//! transaction that read it pending conflicts with the resolution, and one
//! that failed wakes to it.
//!
//! A resolution is an edit of a transaction (`promise-resolution-commits-with-its-cut`):
//! staged in its journal, with a hold on the resolver's breaker, it settles
//! the breaker's status and the store's entry with the commit, under the
//! store's lock, and the committer then publishes the value into the cell,
//! where it still lives, and wakes what waits on it.
//!
//! A breaker that dies queues its promise's ID on the store's channel of
//! lost resolvers, and the runtime then breaks the promise, if still
//! pending, with "resolver lost" (`opaque-resources-reclaimed-when-unreachable`).

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

pub(crate) use crate::core::PromiseId;
pub use crate::core::PromiseStatus;
use crate::core::{
    EvaluationFailure, ManagedPromiseWeakRoot, PromiseAssignment, RuntimeValueAccess, Value,
};
use crate::runtime::RuntimeValueRoot;

/// Why a transaction cannot wait on the value of a promise it resolves: only
/// its commit would settle it.
pub(crate) const SELF_AWAITED: &str =
    "the cut waits on the value of a promise it resolves, which only its commit settles";

/// The pending promises a store's machines and reflection allocated, by
/// their cells' IDs, each held weakly.
pub(crate) struct Promises {
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    pending: Mutex<BTreeMap<PromiseId, ManagedPromiseWeakRoot>>,
    /// Where each breaker reports its death.
    lost: Sender<PromiseId>,
}

/// How a promise resolves: with a value, or broken with a message or a
/// diagnostic.
#[derive(Clone)]
pub(crate) enum Resolution {
    Fulfill(RuntimeValueRoot),
    Break(RuntimeValueRoot),
}

impl Resolution {
    pub(super) fn fulfilled(&self) -> bool {
        matches!(self, Self::Fulfill(_))
    }

    /// The status it gives its promise.
    pub(super) fn status(&self) -> PromiseStatus {
        if self.fulfilled() {
            PromiseStatus::Fulfilled
        } else {
            PromiseStatus::Broken
        }
    }
}

/// A resolution committed and settled, its promised value not yet published.
pub(crate) struct Settled {
    cell: ManagedPromiseWeakRoot,
    resolution: Resolution,
}

impl Settled {
    pub(super) fn new(cell: ManagedPromiseWeakRoot, resolution: Resolution) -> Self {
        Self { cell, resolution }
    }

    fn assignment(&self, access: &RuntimeValueAccess<'_>) -> PromiseAssignment {
        match &self.resolution {
            Resolution::Fulfill(value) => Ok(value.clone_core_with(access)),
            Resolution::Break(failure) => Err(Arc::new(EvaluationFailure::emission_in(
                access,
                failure.clone_core_with(access),
            ))),
        }
    }
}

/// Publishes what a commit settled into the promised values, waking what
/// waits on them as evaluation does, once the committer has released the
/// store and holds no access region: fulfillment is logically asynchronous,
/// after the commit that claimed the promises
/// (`promise-resolution-commits-with-its-cut`).
pub(crate) fn publish_settled(values: &crate::core::CoreValueFactory, settled: Vec<Settled>) {
    if settled.is_empty() {
        return;
    }
    let publications = values.with_boundary_access(|access| {
        settled
            .into_iter()
            // A cell no one holds any longer has no one to read its value.
            .filter_map(|settled| {
                let cell = settled.cell.cell_in(&access)?;
                let assignment = settled.assignment(&access);
                cell.publish(&access, assignment).ok()
            })
            .collect::<Vec<_>>()
    });
    for publication in publications {
        publication.notify();
    }
}

impl Promises {
    pub(super) fn new(lost: Sender<PromiseId>) -> Self {
        Self {
            pending: Mutex::default(),
            lost,
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, BTreeMap<PromiseId, ManagedPromiseWeakRoot>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Allocates a promise: its identity, its promised value, which waits
    /// until it is resolved, and its resolver. The values gain an owner
    /// before `access` ends.
    pub(crate) fn allocate(&self, access: &RuntimeValueAccess<'_>) -> (PromiseId, Value, Value) {
        let (id, promised, resolver, cell) = access.construct_resolvable_promise(&self.lost);
        self.pending().insert(id, cell);
        (id, promised, resolver)
    }

    /// Whether `promise` is one of this store's, still pending: one that
    /// only a resolver, from outside evaluation, resolves.
    pub(crate) fn is_pending(&self, promise: PromiseId) -> bool {
        self.pending().contains_key(&promise)
    }

    /// How many promises are pending.
    #[cfg(test)]
    pub(crate) fn len_for_test(&self) -> usize {
        self.pending().len()
    }

    /// How many pending promises' cells still live.
    #[cfg(test)]
    pub(crate) fn live_cells_for_test(&self, values: &crate::core::CoreValueFactory) -> usize {
        values.with_boundary_access(|access| {
            self.pending()
                .values()
                .filter(|cell| cell.cell_in(&access).is_some())
                .count()
        })
    }

    /// Forgets `promise`, which nothing can have read, as an operation that
    /// allocated it and then failed does.
    pub(crate) fn forget(&self, promise: PromiseId) {
        self.pending().remove(&promise);
    }

    /// Settles `promise` as a commit resolving it does, under the store's
    /// lock, returning its cell, held weakly, for the committer to publish
    /// the resolution into once the lock is released, or `None` where it
    /// was not pending.
    pub(super) fn settle(&self, promise: PromiseId) -> Option<ManagedPromiseWeakRoot> {
        self.pending().remove(&promise)
    }
}
