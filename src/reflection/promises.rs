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

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::core::{
    EvaluationFailure, ManagedPromisePublication, ManagedPromiseRoot, OpaquePayloadFamily,
    OpaquePayloadRecord, OpaqueValue, PromisedValue, RuntimeValueAccess, Value,
};
use crate::runtime::{EvaluationRuntimeId, RuntimeValueRoot};

/// A promise's identity among its store's promises.
pub(crate) type PromiseId = u64;

/// The promises a store's machines and reflection allocated, by their
/// resolvers' identities.
#[derive(Default)]
pub(crate) struct Promises {
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    pending: Mutex<PendingPromises>,
}

#[derive(Default)]
struct PendingPromises {
    next: PromiseId,
    entries: BTreeMap<PromiseId, PromiseEntry>,
}

/// A promise, until it resolves, then how it resolved: a bounded record
/// that keeps no value, until collecting a resolver can drop it
/// (`api-opaque-finalization`).
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

/// A resolver's payload: which promise of which runtime it resolves.
struct PromiseCapability {
    runtime: EvaluationRuntimeId,
    id: PromiseId,
}

// SAFETY: the payload holds a runtime's identity and a number. It contains
// no Glam value, runtime root, managed pointer, or active runtime capability:
// the promise's root stays in the store's `Promises`.
unsafe impl OpaquePayloadFamily for PromiseCapability {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::edge_free("promise resolver identity", "src/reflection/promises.rs");
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

    /// Forgets `promise`, which nothing can have read, as an operation that
    /// allocated it and then failed does.
    pub(crate) fn forget(&self, promise: PromiseId) {
        self.pending().entries.remove(&promise);
    }

    /// Resolves `promise`: once, so a second resolution fails. The caller
    /// notifies what waits on the promised value once it has left `access`,
    /// then writes the promise's place in the store, so transactions that
    /// read it see the change.
    pub(crate) fn resolve(
        &self,
        access: &RuntimeValueAccess<'_>,
        promise: PromiseId,
        resolution: Resolution,
    ) -> Result<ManagedPromisePublication, Arc<str>> {
        let fulfilled = matches!(resolution, Resolution::Fulfill(_));
        let root = {
            let mut pending = self.pending();
            match pending.entries.get(&promise) {
                Some(PromiseEntry::Pending(_)) => {}
                Some(PromiseEntry::Resolved { .. }) => {
                    return Err("the promise was already resolved".into());
                }
                None => return Err("the store has no such promise".into()),
            }
            match pending
                .entries
                .insert(promise, PromiseEntry::Resolved { fulfilled })
            {
                Some(PromiseEntry::Pending(root)) => root,
                _ => unreachable!("the entry was pending"),
            }
        };
        root.publish(
            access,
            match resolution {
                Resolution::Fulfill(value) => Ok(value.clone_core_with(access)),
                Resolution::Break(failure) => Err(Arc::new(EvaluationFailure::emission_in(
                    access,
                    failure.clone_core_with(access),
                ))),
            },
        )
        .map_err(|_| "the promise was already resolved".into())
    }
}
