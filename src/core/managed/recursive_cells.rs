//! Dormant managed representations for the three recursive identity cells.
//!
//! I5C prepares these layouts and their access roles without routing any
//! production constructor through them. I5D switches lazy, promise, and core
//! net identities together only after their exact traces and durable roots
//! are closed as one graph.

#![allow(
    dead_code,
    reason = "I5C prepares private representations for the atomic I5D cutover"
)]

use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use glam_gc::{Gc, Root, Trace, Visitor};

use crate::core::{
    EvaluationFailure, LazyId, LazyResult, LazySource, PromiseId, RuntimeValueAccess,
    RuntimeValueObserver, Value,
};
use crate::core_net::{CoreOperator, CoreWaitToken};
use crate::evaluation::{
    CompletionSubscriptionOutcome, CompletionSubscriptions, PromiseProducerObligation,
    WakeRegistration,
};
use crate::interaction_net::{NetSpecialization, RuntimeNet, RuntimeNetCell, RuntimeNetPayload};

use super::payload_edges::{
    visit_compatibility_managed_edges, visit_compatibility_payload_managed_edges,
    visit_halt_value_edges,
};

/// Terminal promise data stored inside the managed semantic graph.
///
/// Unlike the compatibility `PromiseAssignment`, success is not a registered
/// root: the containing promise cell itself is the traced owner.
type ManagedPromiseAssignment = Result<Value, Arc<EvaluationFailure>>;

/// Prepared synchronization-owning lazy identity.
///
/// Publication keeps the existing result-before-source-release protocol. The
/// separate fields retain the current lock-free terminal read opportunity.
pub(super) struct ManagedLazyCell {
    id: LazyId,
    label: Arc<str>,
    source: Mutex<Option<LazySource>>,
    result: OnceLock<LazyResult>,
}

/// Prepared synchronization-owning promise identity.
///
/// Completion registrations contain only scheduler IDs and weak coordinator
/// routing. The producer direction is deliberately weak; its external owner
/// will hold the promise's registered root after the I5D cutover.
pub(super) struct ManagedPromiseCell {
    id: PromiseId,
    values: RuntimeValueObserver,
    label: Arc<str>,
    assignment: OnceLock<ManagedPromiseAssignment>,
    completion: CompletionSubscriptions,
    producer: OnceLock<Weak<PromiseProducerObligation>>,
}

/// Prepared synchronization-owning core interaction-net identity.
///
/// The owner-neutral generic cell remains the sole topology/revision mutex.
/// I5C.3 supplies its exact payload trace; I5D replaces the current `Arc`
/// facade with a managed edge and registered-root holders atomically.
pub(super) struct ManagedCoreNetCell {
    runtime: RuntimeNetCell<PreparedCoreSpecialization>,
}

/// The dormant specialization makes cross-net ownership an exact managed
/// edge without exposing that representation to production before I5D.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PreparedCoreSpecialization;

#[derive(Clone, Copy)]
struct ManagedCoreNetEdge(Gc<ManagedCoreNetCell>);

impl fmt::Debug for ManagedCoreNetEdge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManagedCoreNetEdge(..)")
    }
}

impl PartialEq for ManagedCoreNetEdge {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(other.0)
    }
}

impl Eq for ManagedCoreNetEdge {}

impl NetSpecialization for PreparedCoreSpecialization {
    type Data = Value;
    type Operator = CoreOperator;
    type RuntimeSource = ManagedCoreNetEdge;
    type WaitToken = CoreWaitToken;
    type StuckReason = crate::core::EvaluationHalt;
}

/// Durable external owners retain registered roots, never bare managed edges.
#[derive(Clone)]
pub(super) struct ManagedLazyRoot {
    root: Root<ManagedLazyCell>,
    observer: RuntimeValueObserver,
}

#[derive(Clone)]
pub(super) struct ManagedPromiseRoot {
    root: Root<ManagedPromiseCell>,
    observer: RuntimeValueObserver,
}

#[derive(Clone)]
pub(super) struct ManagedCoreNetRoot {
    root: Root<ManagedCoreNetCell>,
    observer: RuntimeValueObserver,
}

/// A non-escaping lazy-cell observation authorized by one runtime value scope.
pub(super) struct ManagedLazyAccess<'access, 'scope> {
    cell: &'access ManagedLazyCell,
    authority: &'access RuntimeValueAccess<'scope>,
    _thread_bound: PhantomData<Rc<()>>,
}

/// A non-escaping promise-cell observation authorized by one runtime value scope.
pub(super) struct ManagedPromiseAccess<'access, 'scope> {
    cell: &'access ManagedPromiseCell,
    authority: &'access RuntimeValueAccess<'scope>,
    _thread_bound: PhantomData<Rc<()>>,
}

/// A non-escaping core-net observation authorized by one runtime value scope.
pub(super) struct ManagedCoreNetAccess<'access, 'scope> {
    cell: &'access ManagedCoreNetCell,
    authority: &'access RuntimeValueAccess<'scope>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl ManagedCoreNetCell {
    fn new(runtime: RuntimeNet<PreparedCoreSpecialization>) -> Self {
        Self {
            runtime: RuntimeNetCell::new(runtime),
        }
    }
}

impl ManagedLazyCell {
    fn new(
        values: &crate::core::CoreValueFactory,
        label: impl Into<Arc<str>>,
        source: LazySource,
    ) -> Self {
        Self {
            id: LazyId(values.deferred_value_id()),
            label: label.into(),
            source: Mutex::new(Some(source)),
            result: OnceLock::new(),
        }
    }
}

impl ManagedPromiseCell {
    fn new(values: &crate::core::CoreValueFactory, label: impl Into<Arc<str>>) -> Self {
        let id = PromiseId(values.deferred_value_id());
        Self {
            id,
            values: values.runtime_value_observer(),
            label: label.into(),
            assignment: OnceLock::new(),
            completion: CompletionSubscriptions::for_promise(
                values.runtime_id(),
                id,
                values.work_coordinator_binding(),
            ),
            producer: OnceLock::new(),
        }
    }
}

impl<'access, 'scope> ManagedLazyAccess<'access, 'scope> {
    fn from_authorized_cell(
        cell: &'access ManagedLazyCell,
        observer: &RuntimeValueObserver,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<Self> {
        authority.admits(observer).then_some(Self {
            cell,
            authority,
            _thread_bound: PhantomData,
        })
    }

    fn id(&self) -> LazyId {
        self.cell.id
    }

    fn label(&self) -> &Arc<str> {
        &self.cell.label
    }

    fn source_snapshot(&self) -> Option<LazySource> {
        let _ = self.authority.runtime_id();
        if self.cell.result.get().is_some() {
            return None;
        }
        let source = self
            .cell
            .source
            .lock()
            .expect("managed lazy source cell was poisoned");
        if self.cell.result.get().is_some() {
            return None;
        }
        Some(
            source
                .as_ref()
                .expect("an unresolved managed lazy must retain its source")
                .clone(),
        )
    }

    fn cached(&self) -> Option<LazyResult> {
        self.cell.result.get().cloned()
    }

    fn cache(&self, result: LazyResult) -> LazyResult {
        let _ = self.cell.result.set(result);
        let result = self
            .cell
            .result
            .get()
            .expect("managed lazy cache must contain a value after set")
            .clone();
        let source = self
            .cell
            .source
            .lock()
            .expect("managed lazy source cell was poisoned")
            .take();
        drop(source);
        result
    }
}

impl<'access, 'scope> ManagedPromiseAccess<'access, 'scope> {
    fn from_authorized_cell(
        cell: &'access ManagedPromiseCell,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<Self> {
        authority.admits(&cell.values).then_some(Self {
            cell,
            authority,
            _thread_bound: PhantomData,
        })
    }

    fn id(&self) -> PromiseId {
        self.cell.id
    }

    fn label(&self) -> &Arc<str> {
        &self.cell.label
    }

    fn runtime_id(&self) -> crate::runtime::EvaluationRuntimeId {
        debug_assert!(self.authority.admits(&self.cell.values));
        self.cell.values.runtime_id()
    }

    fn assignment(&self) -> Option<ManagedPromiseAssignment> {
        self.cell.assignment.get().cloned()
    }

    fn install_producer(
        &self,
        producer: &Arc<PromiseProducerObligation>,
    ) -> Result<(), Weak<PromiseProducerObligation>> {
        self.cell.producer.set(Arc::downgrade(producer))
    }

    fn producer(&self) -> Option<Arc<PromiseProducerObligation>> {
        self.cell.producer.get().and_then(Weak::upgrade)
    }

    fn publish(
        &self,
        assignment: ManagedPromiseAssignment,
    ) -> Result<(), ManagedPromiseAssignment> {
        self.cell
            .completion
            .publish(|| self.cell.assignment.set(assignment))
    }

    fn subscribe_work(
        &self,
        runtime: crate::runtime::EvaluationRuntimeId,
        registration: WakeRegistration,
    ) -> CompletionSubscriptionOutcome {
        self.cell.completion.subscribe(runtime, registration, || {
            self.cell.assignment.get().is_some()
        })
    }

    fn unsubscribe_work(&self, registration: WakeRegistration) -> bool {
        self.cell.completion.unsubscribe(registration)
    }
}

impl<'access, 'scope> ManagedCoreNetAccess<'access, 'scope> {
    fn from_authorized_cell(
        cell: &'access ManagedCoreNetCell,
        observer: &RuntimeValueObserver,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<Self> {
        authority.admits(observer).then_some(Self {
            cell,
            authority,
            _thread_bound: PhantomData,
        })
    }

    fn with<R>(&self, inspect: impl FnOnce(&RuntimeNet<PreparedCoreSpecialization>) -> R) -> R {
        let _ = self.authority.runtime_id();
        self.cell.runtime.with(inspect)
    }

    fn with_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<PreparedCoreSpecialization>) -> R,
    ) -> R {
        self.cell.runtime.with_mut(update)
    }
}

fn trace_lazy_result(result: &LazyResult, visitor: &mut Visitor<'_>) {
    match result {
        Ok(value) => visit_compatibility_payload_managed_edges(value, visitor),
        Err(failure) => visit_compatibility_payload_managed_edges(failure.as_ref(), visitor),
    }
}

fn trace_promise_assignment(assignment: &ManagedPromiseAssignment, visitor: &mut Visitor<'_>) {
    match assignment {
        Ok(value) => visit_compatibility_managed_edges(value, visitor),
        Err(failure) => visit_compatibility_payload_managed_edges(failure.as_ref(), visitor),
    }
}

// SAFETY: result publication precedes source removal. Tracing first prefers a
// terminal result, otherwise clones one source while mutation is excluded and
// reports all managed identities reached by the compile-exhaustive payload
// walk. The cloned snapshot is visited after releasing the source mutex.
unsafe impl Trace for ManagedLazyCell {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(super::managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        if let Some(result) = self.result.get().cloned() {
            trace_lazy_result(&result, visitor);
            return;
        }
        let source = {
            let source = self
                .source
                .try_lock()
                .expect("managed lazy must be quiescent during tracing");
            if let Some(result) = self.result.get().cloned() {
                drop(source);
                trace_lazy_result(&result, visitor);
                return;
            }
            source
                .as_ref()
                .expect("an unresolved managed lazy must retain its source")
                .clone()
        };
        visit_compatibility_payload_managed_edges(&source, visitor);
    }
}

// SAFETY: assignment is one-write state. Its success and failure payloads are
// the cell's only semantic edges; subscriptions and the weak producer backlink
// are edge-free coordination.
unsafe impl Trace for ManagedPromiseCell {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(super::managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        if let Some(assignment) = self.assignment.get() {
            trace_promise_assignment(assignment, visitor);
        }
    }
}

// SAFETY: the owner-neutral runtime cell exposes one stable logical payload
// snapshot while mutation is excluded. Every value/operator/stuck payload is
// traversed through its compile-exhaustive compatibility adapter, and every
// prepared cross-net source reports its exact managed edge.
unsafe impl Trace for ManagedCoreNetCell {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(super::managed_slot_extent::<Self>());

    fn trace(&self, visitor: &mut Visitor<'_>) {
        self.runtime
            .try_visit_logical_payloads(&mut |payload| match payload {
                RuntimeNetPayload::Data(value) => {
                    visit_compatibility_managed_edges(value, visitor);
                }
                RuntimeNetPayload::Operator(operator) => {
                    visit_compatibility_payload_managed_edges(operator, visitor);
                }
                RuntimeNetPayload::Source(source) => visitor.visit(source.0),
                RuntimeNetPayload::StuckReason(reason) => {
                    visit_halt_value_edges(reason, &mut |value| {
                        visit_compatibility_managed_edges(value, visitor);
                    });
                }
            });
    }
}

// These are representation records, not a value-size policy. A deliberate
// field change must update the I5C ledger and these target-specific latches.
#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
const _: () = {
    assert!(std::mem::size_of::<ManagedLazyCell>() == 160);
    assert!(std::mem::align_of::<ManagedLazyCell>() == 8);
    assert!(std::mem::size_of::<ManagedPromiseCell>() == 192);
    assert!(std::mem::align_of::<ManagedPromiseCell>() == 8);
    assert!(std::mem::size_of::<ManagedCoreNetCell>() == 248);
    assert!(std::mem::align_of::<ManagedCoreNetCell>() == 8);
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CoreValueFactory, EvaluatedValue, LazySource};
    use crate::interaction_net::NetBuilder;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn new_values() -> CoreValueFactory {
        CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
    }

    macro_rules! assert_does_not_implement {
        ($module:ident, $type:ty, $trait:path) => {
            mod $module {
                use super::*;

                trait AmbiguousIfImplemented<Discriminator> {
                    fn verify() {}
                }

                struct Implemented;

                impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}
                impl<T: ?Sized + $trait> AmbiguousIfImplemented<Implemented> for T {}

                const _: fn() = || {
                    <$type as AmbiguousIfImplemented<_>>::verify();
                };
            }
        };
    }

    assert_does_not_implement!(
        lazy_access_is_not_send,
        ManagedLazyAccess<'static, 'static>,
        Send
    );
    assert_does_not_implement!(
        lazy_access_is_not_sync,
        ManagedLazyAccess<'static, 'static>,
        Sync
    );
    assert_does_not_implement!(
        promise_access_is_not_send,
        ManagedPromiseAccess<'static, 'static>,
        Send
    );
    assert_does_not_implement!(
        promise_access_is_not_sync,
        ManagedPromiseAccess<'static, 'static>,
        Sync
    );
    assert_does_not_implement!(
        core_net_access_is_not_send,
        ManagedCoreNetAccess<'static, 'static>,
        Send
    );
    assert_does_not_implement!(
        core_net_access_is_not_sync,
        ManagedCoreNetAccess<'static, 'static>,
        Sync
    );

    #[test]
    fn recursive_cell_layouts_are_recorded() {
        assert_eq!(std::mem::size_of::<ManagedLazyCell>(), 160);
        assert_eq!(std::mem::size_of::<ManagedPromiseCell>(), 192);
        assert_eq!(std::mem::size_of::<ManagedCoreNetCell>(), 248);
    }

    #[test]
    fn bounded_lazy_gateway_preserves_terminal_publication_protocol() {
        let values = new_values();
        let observer = values.runtime_value_observer();
        let cell = ManagedLazyCell::new(&values, "prepared lazy", LazySource::Error);
        let winner = EvaluatedValue::try_from(Value::Number(42.into())).unwrap();
        let loser = EvaluatedValue::try_from(Value::Number(73.into())).unwrap();

        values.with_runtime_value_access(|access| {
            let lazy = ManagedLazyAccess::from_authorized_cell(&cell, &observer, &access)
                .expect("the matching value domain should authorize its lazy cell");
            assert_eq!(lazy.id(), cell.id);
            assert_eq!(lazy.label().as_ref(), "prepared lazy");
            assert!(lazy.source_snapshot().is_some());
            assert_eq!(lazy.cache(Ok(winner.clone())), Ok(winner.clone()));
            assert_eq!(lazy.cache(Ok(loser)), Ok(winner.clone()));
            assert_eq!(lazy.cached(), Some(Ok(winner)));
            assert!(lazy.source_snapshot().is_none());
        });

        let unrelated = new_values();
        unrelated.with_runtime_value_access(|access| {
            assert!(ManagedLazyAccess::from_authorized_cell(&cell, &observer, &access).is_none());
        });
    }

    #[test]
    fn bounded_promise_gateway_preserves_one_terminal_winner() {
        let values = new_values();
        let cell = ManagedPromiseCell::new(&values, "prepared promise");
        let winner = Value::Number(11.into());
        let loser = Value::Number(12.into());

        values.with_runtime_value_access(|access| {
            let promise = ManagedPromiseAccess::from_authorized_cell(&cell, &access)
                .expect("the matching value domain should authorize its promise cell");
            assert_eq!(promise.id(), cell.id);
            assert_eq!(promise.runtime_id(), values.runtime_id());
            assert_eq!(promise.label().as_ref(), "prepared promise");
            assert!(promise.producer().is_none());
            assert_eq!(promise.publish(Ok(winner.clone())), Ok(()));
            assert_eq!(promise.publish(Ok(loser.clone())), Err(Ok(loser)));
            assert_eq!(promise.assignment(), Some(Ok(winner)));
        });

        let unrelated = new_values();
        unrelated.with_runtime_value_access(|access| {
            assert!(ManagedPromiseAccess::from_authorized_cell(&cell, &access).is_none());
        });
    }

    #[test]
    fn bounded_core_net_gateway_preserves_cell_mutation_publication() {
        let values = new_values();
        let observer = values.runtime_value_observer();
        let mut builder = NetBuilder::<PreparedCoreSpecialization>::new();
        let exposed = builder.data(Value::Number(5.into()));
        let cell = ManagedCoreNetCell::new(builder.finish(exposed).instantiate());

        values.with_runtime_value_access(|access| {
            let net = ManagedCoreNetAccess::from_authorized_cell(&cell, &observer, &access)
                .expect("the matching value domain should authorize its core net");
            assert_eq!(
                net.with(|runtime| runtime.interface_data(runtime.exposed()).cloned()),
                Some(Value::Number(5.into()))
            );
            let before = cell.runtime.with_revisions(|_| ()).1;
            net.with_mut(|_| ());
            let after = cell.runtime.with_revisions(|_| ()).1;
            assert_eq!(after.topology_revision(), before.topology_revision() + 1);
        });

        let unrelated = new_values();
        unrelated.with_runtime_value_access(|access| {
            assert!(
                ManagedCoreNetAccess::from_authorized_cell(&cell, &observer, &access).is_none()
            );
        });
    }
}
