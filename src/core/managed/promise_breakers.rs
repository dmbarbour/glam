//! Promise breakers, which resolvers name (`promise-cells-are-weakly-resolved`).
//!
//! A resolver is one edge to its promise's breaker, and its copies share it.
//! The breaker names its promise by the cell's ID and keeps the promise's
//! status, so the status answers whether or not the cell still lives. It
//! reaches no cell: the store holds a weak reference to each pending
//! promise's cell, keyed by that ID, which serves both a resolution and a
//! lost resolver. When the breaker dies, its destructor queues the ID on the
//! runtime's channel of lost resolvers, and the runtime breaks the promise
//! later, at a safe point (`passive-managed-destruction-with-drop-records`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::Sender;

use glam_gc::{Gc, Root, Trace, Visitor};

use super::{
    EdgeLiveness, ManagedDropRecord, ManagedFamily, ManagedPromiseWeakRoot, RuntimeValueAccess,
    managed_slot_extent,
};
use crate::core::{PromiseId, PromiseStatus, ResolverValue, Value};

/// A promise's breaker: its ID, its status, and where to report its death.
pub(crate) struct ManagedPromiseBreaker {
    id: PromiseId,
    status: Arc<AtomicU8>,
    lost: Sender<PromiseId>,
}

/// A resolver's edge to its breaker.
pub(crate) struct ManagedBreakerEdge(Gc<ManagedPromiseBreaker>);

/// A durable owner's hold on a breaker, which keeps the resolver's promise
/// from being lost: a transaction's, while it stages a resolution, and a
/// request's, while it is served. Its status is the breaker's, read without
/// an access region.
#[derive(Clone)]
pub(crate) struct ManagedResolverRoot {
    id: PromiseId,
    status: Arc<AtomicU8>,
    _breaker: Root<ManagedPromiseBreaker>,
}

const PENDING: u8 = 0;
const FULFILLED: u8 = 1;
const BROKEN: u8 = 2;

impl Drop for ManagedPromiseBreaker {
    fn drop(&mut self) {
        // A runtime already gone has no promise left to break.
        let _ = self.lost.send(self.id);
    }
}

// SAFETY: a breaker holds an ID, a status and a channel's sending end: no
// managed edge.
unsafe impl Trace for ManagedPromiseBreaker {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(managed_slot_extent::<Self>());

    fn trace(&self, _visitor: &mut Visitor<'_>) {}
}

// SAFETY: direct destruction only queues the promise's ID on an unbounded
// channel, which never blocks, takes no lock while no receiver waits (the
// runtime only polls it), and fails harmlessly once the runtime is gone. It
// touches no `Gc` and reaches no runtime: the runtime breaks the promise
// later, at a safe point.
unsafe impl ManagedFamily for ManagedPromiseBreaker {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "managed promise breaker",
        "src/core/managed/promise_breakers.rs",
        "direct Drop queues the promise's ID on the runtime's channel of lost resolvers \
         (`passive-managed-destruction-with-drop-records`)",
        "the status Arc and the channel's sending end release ordinary Rust resources",
    );
}

impl ManagedBreakerEdge {
    pub(crate) fn trace(&self, visitor: &mut Visitor<'_>) {
        visitor.visit(&self.0);
    }

    #[inline(always)]
    pub(crate) fn duplicate_in(&self, access: &RuntimeValueAccess<'_>) -> Self {
        Self(access.duplicate_edge(&self.0))
    }

    #[inline(always)]
    pub(crate) fn same_allocation_in(&self, other: &Self, access: &RuntimeValueAccess<'_>) -> bool {
        access.same_edge(&self.0, &other.0)
    }

    /// A root on the breaker. The caller reaches the edge from a live owner
    /// under `access`.
    pub(crate) fn root_in(&self, access: &RuntimeValueAccess<'_>) -> ManagedResolverRoot {
        let breaker = access.view_edge(&self.0, EdgeLiveness::HeldValue);
        ManagedResolverRoot {
            id: breaker.get().id,
            status: Arc::clone(&breaker.get().status),
            _breaker: access.root(breaker),
        }
    }
}

impl ManagedResolverRoot {
    /// The promise, by its cell's ID.
    pub(crate) fn id(&self) -> PromiseId {
        self.id
    }

    /// The promise's status, as the commits so far settled it.
    pub(crate) fn status(&self) -> PromiseStatus {
        match self.status.load(Ordering::Acquire) {
            PENDING => PromiseStatus::Pending,
            FULFILLED => PromiseStatus::Fulfilled,
            _ => PromiseStatus::Broken,
        }
    }

    /// Settles the status, as a commit resolving the promise does. Returns
    /// whether it was pending.
    pub(crate) fn settle(&self, fulfilled: bool) -> bool {
        let status = if fulfilled { FULFILLED } else { BROKEN };
        self.status
            .compare_exchange(PENDING, status, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

impl RuntimeValueAccess<'_> {
    /// Allocates a promise that a resolver resolves: its ID, its promised
    /// value, its resolver, and a rooted weak reference to its cell, for the
    /// store. The values are facades, which gain an owner before this region
    /// ends; until then the region keeps them.
    pub(crate) fn construct_resolvable_promise(
        &self,
        lost: &Sender<PromiseId>,
    ) -> (PromiseId, Value, Value, ManagedPromiseWeakRoot) {
        let promised = self
            .construct_managed_promise()
            .expect("a promise cell fits one collector slot");
        let id = promised.edge.access(self).id();
        let weak = promised.edge.root_weak(self);
        let breaker = self
            .allocator::<ManagedPromiseBreaker>()
            .expect("a promise breaker fits one collector slot")
            .alloc(ManagedPromiseBreaker {
                id,
                status: Arc::new(AtomicU8::new(PENDING)),
                lost: lost.clone(),
            });
        let resolver = ResolverValue::from_edge(ManagedBreakerEdge(breaker));
        (
            id,
            Value::Promised(promised),
            Value::Resolver(resolver),
            weak,
        )
    }

    /// A root on the breaker of `value`, a resolver, or `None` for another
    /// value. The caller holds `value` under this access.
    pub(crate) fn resolver_root(&self, value: &Value) -> Option<ManagedResolverRoot> {
        let Value::Resolver(resolver) = value else {
            return None;
        };
        Some(resolver.root_in(self))
    }
}
