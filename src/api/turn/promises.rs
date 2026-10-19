//! Promises that machines allocate and resolve (`machines-inherit-an-environment`).
//!
//! A resolver is an opaque glam value, which may hold no managed root, so it
//! carries only its promise's identity, and the runtime holds each
//! unresolved promise's root here until it is resolved, then how it
//! resolved, so its resolver reads its status
//! (`requests-travel-on-ports`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::api::runtime::RuntimeSharedResources;
use crate::api::{Error, Value, Values};
use crate::core::{
    EvaluationFailure, ManagedPromiseRoot, OpaquePayloadFamily, OpaquePayloadRecord, OpaqueValue,
    PromisedValue, Value as CoreValue,
};
use crate::runtime::EvaluationRuntimeId;

/// The promises a runtime's machines allocated, by their resolvers'
/// identities.
#[derive(Default)]
pub(super) struct Promises {
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    pending: Mutex<PendingPromises>,
}

#[derive(Default)]
struct PendingPromises {
    next: u64,
    entries: BTreeMap<u64, PromiseEntry>,
}

/// A promise, until it resolves, then how it resolved: a bounded record
/// that keeps no value, until collecting a resolver can drop it
/// (`api-opaque-finalization`).
enum PromiseEntry {
    Pending(ManagedPromiseRoot),
    Resolved { fulfilled: bool },
}

/// A promise's status, read without forcing its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromiseStatus {
    /// Not yet resolved.
    Pending,
    /// Resolved with a value.
    Fulfilled,
    /// Broken, with a message or diagnostic.
    Broken,
}

/// How a promise resolves: with a value, or broken with a message or a
/// diagnostic.
pub(super) enum Resolution {
    Fulfill(Value),
    Break(Value),
}

/// A resolver's payload: which promise of which runtime it resolves.
struct PromiseCapability {
    runtime: EvaluationRuntimeId,
    id: u64,
}

// SAFETY: the payload holds a runtime's identity and a number. It contains
// no Glam value, runtime root, managed pointer, or active runtime capability:
// the promise's root stays in the runtime's `Promises`.
unsafe impl OpaquePayloadFamily for PromiseCapability {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::edge_free("promise resolver identity", "src/api/turn/promises.rs");
}

impl Promises {
    fn pending(&self) -> std::sync::MutexGuard<'_, PendingPromises> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Allocates a promise: its promised value, which waits until it is
    /// resolved, and its resolver.
    pub(super) fn allocate(&self, values: &Values) -> (Value, Value) {
        values.with_access(|scoped| {
            let promise = scoped
                .runtime_access()
                .construct_rooted_managed_promise()
                .expect("managed promise representation must fit one collector run");
            let promised = scoped.wrap(CoreValue::Promised(PromisedValue::from_root(
                &promise,
                scoped.runtime_access(),
            )));
            let id = {
                let mut pending = self.pending();
                let id = pending.next;
                pending.next += 1;
                pending.entries.insert(id, PromiseEntry::Pending(promise));
                id
            };
            let capability = PromiseCapability {
                runtime: values.runtime_id(),
                id,
            };
            let resolver = scoped.wrap(CoreValue::Opaque(OpaqueValue::new(
                values.core(),
                Arc::new(capability),
            )));
            (promised, resolver)
        })
    }

    /// The identity of the promise that `resolver`, evaluated, resolves,
    /// where it is a resolver of this runtime's promise.
    fn identity(values: &Values, resolver: &Value) -> Result<Option<u64>, Error> {
        values.with_access(|scoped| {
            let capability = match scoped.clone_core(resolver)? {
                CoreValue::Opaque(opaque) => opaque.downcast::<PromiseCapability>(values.core()),
                _ => None,
            };
            Ok(capability
                .filter(|capability| capability.runtime == values.runtime_id())
                .map(|capability| capability.id))
        })
    }

    /// The status of the promise that `resolver`, evaluated, resolves, or
    /// `None` where it is no resolver of this runtime's promise.
    pub(super) fn status(
        &self,
        values: &Values,
        resolver: &Value,
    ) -> Result<Option<PromiseStatus>, Error> {
        let Some(id) = Self::identity(values, resolver)? else {
            return Ok(None);
        };
        Ok(self.pending().entries.get(&id).map(|entry| match entry {
            PromiseEntry::Pending(_) => PromiseStatus::Pending,
            PromiseEntry::Resolved { fulfilled: true } => PromiseStatus::Fulfilled,
            PromiseEntry::Resolved { fulfilled: false } => PromiseStatus::Broken,
        }))
    }

    /// Forgets the promise of `resolver`, which nothing can have read, as an
    /// operation that allocated it and then failed does.
    pub(super) fn forget(&self, values: &Values, resolver: &Value) {
        if let Ok(Some(id)) = Self::identity(values, resolver) {
            self.pending().entries.remove(&id);
        }
    }

    /// Resolves the promise of `resolver`, which is evaluated: once, so a
    /// second resolution fails. What waits on the promise wakes, and so does
    /// a cut waiting on a change.
    pub(super) fn resolve(
        &self,
        values: &Values,
        resources: &RuntimeSharedResources,
        resolver: &Value,
        resolution: Resolution,
    ) -> Result<(), Error> {
        let id = Self::identity(values, resolver)?
            .ok_or_else(|| Error::new("the value is not a resolver of this runtime's promise"))?;
        let fulfilled = matches!(resolution, Resolution::Fulfill(_));
        let promise = {
            let mut pending = self.pending();
            match pending.entries.get(&id) {
                Some(PromiseEntry::Pending(_)) => {}
                _ => return Err(Error::new("the promise was already resolved")),
            }
            match pending
                .entries
                .insert(id, PromiseEntry::Resolved { fulfilled })
            {
                Some(PromiseEntry::Pending(promise)) => promise,
                _ => unreachable!("the entry was pending"),
            }
        };
        let published = values.core().with_boundary_access(|access| {
            promise.publish(
                &access,
                match &resolution {
                    Resolution::Fulfill(value) => Ok(value.0.clone_core_with(&access)),
                    Resolution::Break(failure) => Err(Arc::new(EvaluationFailure::emission_in(
                        &access,
                        failure.0.clone_core_with(&access),
                    ))),
                },
            )
        });
        match published {
            Ok(publication) => {
                publication.notify();
                resources.publish_change();
                Ok(())
            }
            Err(_) => Err(Error::new("the promise was already resolved")),
        }
    }
}
