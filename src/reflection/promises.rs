//! Promises that machines and reflection allocate and resolve
//! (`machines-inherit-an-environment`, `requests-travel-on-ports`).
//!
//! A resolver is an opaque glam value, which may hold no managed root, so it
//! carries only its promise's identity, and the store holds each unresolved
//! promise's root here until it is resolved, then how it resolved, so its
//! resolver reads its status. A promise's status is a read of the store, too:
//! within a transaction a read observes the promise's place in the runtime's
//! volume, which its resolution writes, so a transaction that read it pending
//! conflicts with the resolution, and one that failed wakes to it.
//!
//! A resolution is an edit of a transaction (`promise-resolution-commits-with-its-cut`):
//! staged in its journal, it settles the record with the commit, under the
//! store's lock, and the committer then publishes the value and wakes what
//! waits on it.
//!
//! A resolver's payload reports its death to the store, and the runtime then
//! breaks its promise, if still pending, with "resolver lost", and drops its
//! record (`opaque-resources-reclaimed-when-unreachable`). A resolver keeps
//! its promise alive through the record, never the reverse: a pending
//! promise reaches nothing.

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

use crate::core::{
    EvaluationFailure, ManagedPromisePublication, ManagedPromiseRoot, OpaquePayloadFamily,
    OpaquePayloadRecord, OpaqueValue, PromiseAssignment, PromisedValue, RuntimeValueAccess, Value,
};
use crate::runtime::{EvaluationRuntimeId, RuntimeMutationAuthority, RuntimeValueRoot};

use super::store::DeathNotice;

/// A promise's identity among its store's promises.
pub(crate) type PromiseId = u64;

/// The promises a store's machines and reflection allocated, by their
/// resolvers' identities.
pub(crate) struct Promises {
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    pending: Mutex<PendingPromises>,
    /// Where each resolver reports its death.
    notices: Sender<DeathNotice>,
}

#[derive(Default)]
struct PendingPromises {
    next: PromiseId,
    entries: BTreeMap<PromiseId, PromiseEntry>,
}

/// A promise, until it resolves, then how it resolved: a bounded record
/// that keeps no value, until its resolver dies.
enum PromiseEntry {
    Pending(ManagedPromiseRoot),
    Resolved { fulfilled: bool },
}

/// A promise's status, read from its resolver, never its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromiseStatus {
    /// Not yet resolved.
    Pending,
    /// Resolved with a value.
    Fulfilled,
    /// Broken, with a message or diagnostic.
    Broken,
}

impl PromiseStatus {
    /// The status's name, as reflection's atom spells it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Fulfilled => "fulfilled",
            Self::Broken => "broken",
        }
    }
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
    root: ManagedPromiseRoot,
    resolution: Resolution,
}

impl Settled {
    pub(super) fn new(root: ManagedPromiseRoot, resolution: Resolution) -> Self {
        Self { root, resolution }
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

/// Publishes what a commit settled into the promised values, under the
/// runtime-mutation admission that the committer still holds, returning the
/// wakes to deliver once it has advanced the observation epoch and released
/// that admission: every wake on a resolved promise follows its publication
/// (`promise-resolution-commits-with-its-cut`). The committer has released
/// the store's lock, and holds no access region.
pub(crate) fn publish_settled(
    values: &crate::core::CoreValueFactory,
    mutation: &dyn RuntimeMutationAuthority,
    settled: Vec<Settled>,
) -> Vec<ManagedPromisePublication> {
    if settled.is_empty() {
        return Vec::new();
    }
    values.with_boundary_access(|access| {
        settled
            .into_iter()
            .filter_map(|settled| {
                let assignment = settled.assignment(&access);
                settled
                    .root
                    .publish_admitted(&access, mutation, assignment)
                    .ok()
            })
            .collect()
    })
}

/// As [`publish_settled`], for a store outside a runtime's admission, such
/// as a test host's, delivering the wakes at once.
#[cfg(test)]
pub(crate) fn publish_settled_unadmitted(
    values: &crate::core::CoreValueFactory,
    settled: Vec<Settled>,
) {
    let publications = values.with_boundary_access(|access| {
        settled
            .into_iter()
            .filter_map(|settled| {
                let assignment = settled.assignment(&access);
                settled.root.publish(&access, assignment).ok()
            })
            .collect::<Vec<_>>()
    });
    for publication in publications {
        publication.notify();
    }
}

/// A resolver's payload: which promise of which runtime it resolves, and
/// where to report its death.
struct PromiseCapability {
    runtime: EvaluationRuntimeId,
    id: PromiseId,
    notices: Sender<DeathNotice>,
}

impl Drop for PromiseCapability {
    fn drop(&mut self) {
        // A store already gone has no record left to settle.
        let _ = self.notices.send(DeathNotice::Resolver(self.id));
    }
}

// SAFETY: the payload holds a runtime's identity, a number, and a channel's
// sending end. It contains no Glam value, runtime root, managed pointer, or
// active runtime capability: the promise's root stays in the store's
// `Promises`, and its `Drop` only queues a notice for the store, which acts
// on it later, at a safe point.
unsafe impl OpaquePayloadFamily for PromiseCapability {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::external("promise resolver", "src/reflection/promises.rs");
}

/// The promise that `resolver`, evaluated, resolves, where it is a resolver
/// of this runtime's promise.
pub(crate) fn resolver_promise(
    access: &RuntimeValueAccess<'_>,
    resolver: &Value,
) -> Option<PromiseId> {
    let Value::Opaque(opaque) = resolver else {
        return None;
    };
    let values = access.values();
    opaque
        .downcast::<PromiseCapability>(values)
        .filter(|capability| capability.runtime == values.runtime_id())
        .map(|capability| capability.id)
}

impl Promises {
    pub(super) fn new(notices: Sender<DeathNotice>) -> Self {
        Self {
            pending: Mutex::default(),
            notices,
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, PendingPromises> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Allocates a promise: its identity, its promised value, which waits
    /// until it is resolved, and its resolver.
    pub(crate) fn allocate(&self, access: &RuntimeValueAccess<'_>) -> (PromiseId, Value, Value) {
        let promise = access
            .construct_rooted_managed_promise()
            .expect("managed promise representation must fit one collector run");
        let promised = Value::Promised(PromisedValue::from_root(&promise, access));
        let id = {
            let mut pending = self.pending();
            let id = pending.next;
            pending.next += 1;
            pending.entries.insert(id, PromiseEntry::Pending(promise));
            id
        };
        let values = access.values();
        let capability = PromiseCapability {
            runtime: values.runtime_id(),
            id,
            notices: self.notices.clone(),
        };
        let resolver = Value::Opaque(OpaqueValue::new(values, Arc::new(capability)));
        (id, promised, resolver)
    }

    /// The status of `promise`, or `None` where this store has no such
    /// promise.
    pub(crate) fn status(&self, promise: PromiseId) -> Option<PromiseStatus> {
        self.pending()
            .entries
            .get(&promise)
            .map(|entry| match entry {
                PromiseEntry::Pending(_) => PromiseStatus::Pending,
                PromiseEntry::Resolved { fulfilled: true } => PromiseStatus::Fulfilled,
                PromiseEntry::Resolved { fulfilled: false } => PromiseStatus::Broken,
            })
    }

    /// How many promises the record holds, pending or resolved.
    #[cfg(test)]
    pub(crate) fn len_for_test(&self) -> usize {
        self.pending().entries.len()
    }

    /// Forgets `promise`, which nothing can have read, as an operation that
    /// allocated it and then failed does.
    pub(crate) fn forget(&self, promise: PromiseId) {
        self.pending().entries.remove(&promise);
    }

    /// Settles `promise` as a commit resolving it does, under the store's
    /// lock, returning its root for the committer to publish the
    /// resolution into once the lock is released, or `None` where it was
    /// not pending.
    pub(super) fn settle(&self, promise: PromiseId, fulfilled: bool) -> Option<ManagedPromiseRoot> {
        let mut pending = self.pending();
        let Some(PromiseEntry::Pending(_)) = pending.entries.get(&promise) else {
            return None;
        };
        match pending
            .entries
            .insert(promise, PromiseEntry::Resolved { fulfilled })
        {
            Some(PromiseEntry::Pending(root)) => Some(root),
            _ => unreachable!("the entry was pending"),
        }
    }
}
