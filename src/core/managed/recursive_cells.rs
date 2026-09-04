//! Managed representations for the three recursive identity cells.
//!
//! I5C prepared these layouts and their access roles. I5D routes lazy,
//! promise, and core-net production identities through them as one exact
//! traced graph.

use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use glam_gc::{Gc, Root, Trace, UnsupportedLayout, Visitor};

use crate::core::{
    CoreValueFactory, EvaluationFailure, LazyId, LazyResult, LazySource, PromiseId,
    RuntimeValueAccess, RuntimeValueObserver, Value,
};
use crate::core_net::CoreSpecialization;
use crate::evaluation::{
    CompletionSubscriptionOutcome, CompletionSubscriptions, CompletionWake,
    EvaluationWorkCoordinator, PromiseProducerObligation, WakeRegistration,
};
use crate::interaction_net::{RuntimeNet, RuntimeNetCell, RuntimeNetPayload};
use crate::runtime::RuntimeMutationAuthority;

use super::payload_edges::{
    CompatibilityNetEdges, visit_compatibility_managed_edges,
    visit_compatibility_payload_managed_edges, visit_halt_value_edges,
};
use super::{ManagedDropRecord, ManagedFamily};

/// Terminal promise data stored inside the managed semantic graph.
///
/// Unlike the compatibility `PromiseAssignment`, success is not a registered
/// root: the containing promise cell itself is the traced owner.
type ManagedPromiseAssignment = Result<Value, Arc<EvaluationFailure>>;

/// Synchronization-owning managed lazy identity.
///
/// Publication keeps the existing result-before-source-release protocol. The
/// separate fields retain the current lock-free terminal read opportunity.
pub(crate) struct ManagedLazyCell {
    id: LazyId,
    label: Arc<str>,
    source: Mutex<Option<LazySource>>,
    result: OnceLock<LazyResult>,
}

/// Synchronization-owning managed promise identity.
///
/// Completion registrations contain only scheduler IDs and weak coordinator
/// routing. The producer obligation remains strongly associated with the
/// promise so terminal observers retain its wait provenance; the obligation's
/// coordinator/local-owner route is weak, so this backlink cannot retain the
/// task registry or form an ownership cycle.
pub(crate) struct ManagedPromiseCell {
    id: PromiseId,
    values: RuntimeValueObserver,
    label: Arc<str>,
    assignment: OnceLock<ManagedPromiseAssignment>,
    completion: CompletionSubscriptions,
    producer: OnceLock<Arc<PromiseProducerObligation>>,
}

/// Synchronization-owning managed core interaction-net identity.
///
/// The owner-neutral generic cell remains the sole topology/revision mutex.
/// I5C.3 supplied its exact payload trace; I5D installed the managed edge and
/// registered-root holders atomically.
pub(crate) struct ManagedCoreNetCell {
    runtime: RuntimeNetCell<CoreSpecialization>,
}

#[derive(Clone, Copy)]
pub(crate) struct ManagedLazyEdge(Gc<ManagedLazyCell>);

impl fmt::Debug for ManagedLazyEdge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManagedLazyEdge(..)")
    }
}

impl PartialEq for ManagedLazyEdge {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(other.0)
    }
}

impl Eq for ManagedLazyEdge {}

#[derive(Clone, Copy)]
pub(crate) struct ManagedPromiseEdge(Gc<ManagedPromiseCell>);

impl fmt::Debug for ManagedPromiseEdge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManagedPromiseEdge(..)")
    }
}

impl PartialEq for ManagedPromiseEdge {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(other.0)
    }
}

impl Eq for ManagedPromiseEdge {}

#[derive(Clone, Copy)]
pub(crate) struct ManagedCoreNetEdge(Gc<ManagedCoreNetCell>);

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

/// Durable external owners retain registered roots, never bare managed edges.
#[derive(Clone, Debug)]
pub(crate) struct ManagedLazyRoot {
    id: LazyId,
    label: Arc<str>,
    edge: ManagedLazyEdge,
    #[allow(
        dead_code,
        reason = "the registered root is retained for ownership and released by Drop"
    )]
    root: Root<ManagedLazyCell>,
    observer: RuntimeValueObserver,
}

#[derive(Clone, Debug)]
pub(crate) struct ManagedPromiseRoot {
    id: PromiseId,
    label: Arc<str>,
    edge: ManagedPromiseEdge,
    root: Root<ManagedPromiseCell>,
    observer: RuntimeValueObserver,
}

#[derive(Clone, Debug)]
pub(crate) struct ManagedCoreNetRoot {
    #[allow(
        dead_code,
        reason = "the registered root is retained for ownership and released by Drop"
    )]
    root: Root<ManagedCoreNetCell>,
}

/// A non-escaping lazy-cell observation authorized by one runtime value scope.
pub(crate) struct ManagedLazyAccess<'access, 'scope> {
    cell: &'access ManagedLazyCell,
    authority: &'access RuntimeValueAccess<'scope>,
    _thread_bound: PhantomData<Rc<()>>,
}

/// A non-escaping promise-cell observation authorized by one runtime value scope.
pub(crate) struct ManagedPromiseAccess<'access, 'scope> {
    cell: &'access ManagedPromiseCell,
    _authority: &'access RuntimeValueAccess<'scope>,
    _thread_bound: PhantomData<Rc<()>>,
}

/// A non-escaping core-net observation authorized by one runtime value scope.
pub(crate) struct ManagedCoreNetAccess<'access, 'scope> {
    cell: &'access ManagedCoreNetCell,
    authority: &'access RuntimeValueAccess<'scope>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl ManagedCoreNetCell {
    fn new(runtime: RuntimeNet<CoreSpecialization>) -> Self {
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

impl RuntimeValueAccess<'_> {
    pub(crate) fn allocate_managed_lazy(
        &self,
        values: &CoreValueFactory,
        label: impl Into<Arc<str>>,
        source: LazySource,
    ) -> Result<ManagedLazyEdge, UnsupportedLayout> {
        assert!(
            self.belongs_to(values),
            "lazy construction requires its value domain"
        );
        let allocator = self.allocator::<ManagedLazyCell>()?;
        Ok(ManagedLazyEdge(
            allocator.alloc(ManagedLazyCell::new(values, label, source)),
        ))
    }

    pub(crate) fn allocate_managed_promise(
        &self,
        values: &CoreValueFactory,
        label: impl Into<Arc<str>>,
    ) -> Result<ManagedPromiseEdge, UnsupportedLayout> {
        assert!(
            self.belongs_to(values),
            "promise construction requires its value domain"
        );
        let allocator = self.allocator::<ManagedPromiseCell>()?;
        Ok(ManagedPromiseEdge(
            allocator.alloc(ManagedPromiseCell::new(values, label)),
        ))
    }

    pub(crate) fn allocate_managed_core_net(
        &self,
        values: &CoreValueFactory,
        runtime: RuntimeNet<CoreSpecialization>,
    ) -> Result<ManagedCoreNetEdge, UnsupportedLayout> {
        assert!(
            self.belongs_to(values),
            "core-net construction requires its value domain"
        );
        let allocator = self.allocator::<ManagedCoreNetCell>()?;
        Ok(ManagedCoreNetEdge(
            allocator.alloc(ManagedCoreNetCell::new(runtime)),
        ))
    }

    pub(crate) fn root_managed_lazy(
        &self,
        observer: RuntimeValueObserver,
        edge: ManagedLazyEdge,
    ) -> ManagedLazyRoot {
        assert!(
            self.admits(&observer),
            "lazy root requires its value domain"
        );
        let value = edge
            .access(&observer, self)
            .expect("lazy edge must belong to its root domain");
        let id = value.id();
        let label = value.label().clone();
        ManagedLazyRoot {
            id,
            label,
            edge,
            root: self.root(edge.0),
            observer,
        }
    }

    pub(crate) fn root_managed_promise(
        &self,
        observer: RuntimeValueObserver,
        edge: ManagedPromiseEdge,
    ) -> ManagedPromiseRoot {
        assert!(
            self.admits(&observer),
            "promise root requires its value domain"
        );
        let value = edge
            .access(&observer, self)
            .expect("promise edge must belong to its root domain");
        let id = value.id();
        let label = value.label().clone();
        ManagedPromiseRoot {
            id,
            label,
            edge,
            root: self.root(edge.0),
            observer,
        }
    }

    pub(crate) fn root_managed_core_net(
        &self,
        observer: RuntimeValueObserver,
        edge: ManagedCoreNetEdge,
    ) -> ManagedCoreNetRoot {
        assert!(
            self.admits(&observer),
            "core-net root requires its value domain"
        );
        ManagedCoreNetRoot {
            root: self.root(edge.0),
        }
    }

    #[cfg(test)]
    fn root_new_managed_lazy(
        &self,
        values: &CoreValueFactory,
        label: impl Into<Arc<str>>,
        source: LazySource,
    ) -> Result<ManagedLazyRoot, UnsupportedLayout> {
        let observer = values.runtime_value_observer();
        let edge = self.allocate_managed_lazy(values, label, source)?;
        Ok(self.root_managed_lazy(observer, edge))
    }

    #[cfg(test)]
    fn root_new_managed_promise(
        &self,
        values: &CoreValueFactory,
        label: impl Into<Arc<str>>,
    ) -> Result<ManagedPromiseRoot, UnsupportedLayout> {
        let observer = values.runtime_value_observer();
        let edge = self.allocate_managed_promise(values, label)?;
        Ok(self.root_managed_promise(observer, edge))
    }

    #[cfg(test)]
    fn root_new_managed_core_net(
        &self,
        values: &CoreValueFactory,
        runtime: RuntimeNet<CoreSpecialization>,
    ) -> Result<ManagedCoreNetRoot, UnsupportedLayout> {
        let observer = values.runtime_value_observer();
        let edge = self.allocate_managed_core_net(values, runtime)?;
        Ok(self.root_managed_core_net(observer, edge))
    }
}

impl ManagedLazyEdge {
    pub(crate) fn trace(self, visitor: &mut Visitor<'_>) {
        visitor.visit(self.0);
    }

    pub(crate) fn access<'access, 'scope>(
        self,
        observer: &RuntimeValueObserver,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<ManagedLazyAccess<'access, 'scope>> {
        if !authority.admits(observer) {
            return None;
        }
        // SAFETY: this private edge can only be constructed by the matching
        // value domain. Its caller supplies liveness through a rooted owner or
        // a traced semantic edge reached within this access region.
        let cell = unsafe { authority.scope.get_traced_edge(self.0) };
        ManagedLazyAccess::from_authorized_cell(cell, observer, authority)
    }
}

impl ManagedPromiseEdge {
    pub(crate) fn trace(self, visitor: &mut Visitor<'_>) {
        visitor.visit(self.0);
    }

    pub(crate) fn access<'access, 'scope>(
        self,
        observer: &RuntimeValueObserver,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<ManagedPromiseAccess<'access, 'scope>> {
        if !authority.admits(observer) {
            return None;
        }
        // SAFETY: the private constructor and observer preserve exact heap and
        // representation provenance; the caller supplies current liveness.
        let cell = unsafe { authority.scope.get_traced_edge(self.0) };
        ManagedPromiseAccess::from_authorized_cell(cell, authority)
    }
}

impl ManagedCoreNetEdge {
    pub(crate) fn trace(self, visitor: &mut Visitor<'_>) {
        visitor.visit(self.0);
    }

    pub(crate) fn access<'access, 'scope>(
        self,
        observer: &RuntimeValueObserver,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<ManagedCoreNetAccess<'access, 'scope>> {
        if !authority.admits(observer) {
            return None;
        }
        // SAFETY: the private constructor and observer preserve exact heap and
        // representation provenance; the caller supplies current liveness.
        let cell = unsafe { authority.scope.get_traced_edge(self.0) };
        ManagedCoreNetAccess::from_authorized_cell(cell, observer, authority)
    }
}

impl ManagedLazyRoot {
    pub(crate) fn id(&self) -> LazyId {
        self.id
    }

    pub(crate) fn label(&self) -> &Arc<str> {
        &self.label
    }

    pub(crate) fn edge(&self) -> ManagedLazyEdge {
        self.edge
    }

    pub(crate) fn observer(&self) -> &RuntimeValueObserver {
        &self.observer
    }

    #[cfg(test)]
    pub(crate) fn access<'access, 'scope>(
        &'access self,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<ManagedLazyAccess<'access, 'scope>> {
        if !authority.admits(&self.observer) || !authority.admits_root(&self.root) {
            return None;
        }
        ManagedLazyAccess::from_authorized_cell(
            authority.get(&self.root),
            &self.observer,
            authority,
        )
    }
}

impl ManagedPromiseRoot {
    pub(crate) fn id(&self) -> PromiseId {
        self.id
    }

    pub(crate) fn label(&self) -> &Arc<str> {
        &self.label
    }

    pub(crate) fn edge(&self) -> ManagedPromiseEdge {
        self.edge
    }

    pub(crate) fn observer(&self) -> &RuntimeValueObserver {
        &self.observer
    }

    pub(crate) fn access<'access, 'scope>(
        &'access self,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<ManagedPromiseAccess<'access, 'scope>> {
        if !authority.admits(&self.observer) || !authority.admits_root(&self.root) {
            return None;
        }
        ManagedPromiseAccess::from_authorized_cell(authority.get(&self.root), authority)
    }
}

impl ManagedCoreNetRoot {
    #[cfg(test)]
    pub(crate) fn access<'access, 'scope>(
        &'access self,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<ManagedCoreNetAccess<'access, 'scope>> {
        if !authority.admits_root(&self.root) {
            return None;
        }
        Some(ManagedCoreNetAccess {
            cell: authority.get(&self.root),
            authority,
            _thread_bound: PhantomData,
        })
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

    pub(crate) fn id(&self) -> LazyId {
        self.cell.id
    }

    pub(crate) fn label(&self) -> &Arc<str> {
        &self.cell.label
    }

    pub(crate) fn source_snapshot(&self) -> Option<LazySource> {
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

    pub(crate) fn cached(&self) -> Option<LazyResult> {
        self.cell.result.get().cloned()
    }

    pub(crate) fn cache(&self, result: LazyResult) -> LazyResult {
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
            _authority: authority,
            _thread_bound: PhantomData,
        })
    }

    pub(crate) fn id(&self) -> PromiseId {
        self.cell.id
    }

    pub(crate) fn label(&self) -> &Arc<str> {
        &self.cell.label
    }

    #[cfg(test)]
    pub(crate) fn runtime_id(&self) -> crate::runtime::EvaluationRuntimeId {
        debug_assert!(self._authority.admits(&self.cell.values));
        self.cell.values.runtime_id()
    }

    pub(crate) fn assignment(&self) -> Option<ManagedPromiseAssignment> {
        self.cell.assignment.get().cloned()
    }

    pub(crate) fn install_producer(
        &self,
        producer: &Arc<PromiseProducerObligation>,
    ) -> Result<(), Arc<PromiseProducerObligation>> {
        self.cell.producer.set(Arc::clone(producer))
    }

    pub(crate) fn producer(&self) -> Option<Arc<PromiseProducerObligation>> {
        self.cell.producer.get().cloned()
    }

    #[cfg(test)]
    pub(crate) fn publish(
        &self,
        assignment: ManagedPromiseAssignment,
    ) -> Result<(), ManagedPromiseAssignment> {
        self.publish_detached(assignment, |_| ())
    }

    pub(crate) fn publish_detached<T>(
        &self,
        assignment: ManagedPromiseAssignment,
        after_assignment: impl FnOnce(&ManagedPromiseAssignment) -> T,
    ) -> Result<T, ManagedPromiseAssignment> {
        self.cell.completion.publish(|| {
            self.cell.assignment.set(assignment)?;
            Ok(after_assignment(self.cell.assignment.get().expect(
                "managed promise publication must initialize its assignment",
            )))
        })
    }

    pub(crate) fn publish_guarded<T>(
        &self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        assignment: ManagedPromiseAssignment,
        after_assignment: impl FnOnce(&ManagedPromiseAssignment) -> T,
    ) -> Result<(T, CompletionWake), ManagedPromiseAssignment> {
        self.cell
            .completion
            .publish_guarded(coordinator, mutation, || {
                self.cell.assignment.set(assignment)?;
                Ok(after_assignment(self.cell.assignment.get().expect(
                    "managed promise publication must initialize its assignment",
                )))
            })
    }

    pub(crate) fn subscribe_work(
        &self,
        runtime: crate::runtime::EvaluationRuntimeId,
        registration: WakeRegistration,
    ) -> CompletionSubscriptionOutcome {
        self.cell.completion.subscribe(runtime, registration, || {
            self.cell.assignment.get().is_some()
        })
    }

    pub(crate) fn unsubscribe_work(&self, registration: WakeRegistration) -> bool {
        self.cell.completion.unsubscribe(registration)
    }

    #[cfg(test)]
    pub(crate) fn exact_subscription_count(&self) -> usize {
        self.cell.completion.len()
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

    pub(crate) fn with<R>(&self, inspect: impl FnOnce(&RuntimeNet<CoreSpecialization>) -> R) -> R {
        let _ = self.authority.runtime_id();
        self.cell.runtime.with(inspect)
    }

    #[cfg(test)]
    pub(crate) fn with_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<CoreSpecialization>) -> R,
    ) -> R {
        self.cell.runtime.with_mut(update)
    }

    pub(crate) fn cell(&self) -> &RuntimeNetCell<CoreSpecialization> {
        &self.cell.runtime
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
        source.visit_compatibility_net_edges(&mut |net| {
            net.trace_managed_edge(visitor);
        });
    }
}

// SAFETY: assignment is one-write state. Its success and failure payloads are
// the cell's only semantic edges. Subscriptions and the strong immutable
// producer record contain no managed edge or root; the record's coordinator
// and local-owner routes are weak.
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
                    operator.visit_compatibility_net_edges(&mut |net| {
                        net.trace_managed_edge(visitor);
                    });
                }
                RuntimeNetPayload::Source(source) => source.trace_managed_edge(visitor),
                RuntimeNetPayload::StuckReason(reason) => {
                    visit_halt_value_edges(reason, &mut |value| {
                        visit_compatibility_managed_edges(value, visitor);
                    });
                }
            });
    }
}

// SAFETY: the lazy cell's direct synchronization fields have no active Drop
// behavior. Its source and result contain only compatibility values whose
// transitive destruction passed I4F.2b's passive-closure gate; managed
// identities reached after I5D are inert Gc edges.
unsafe impl ManagedFamily for ManagedLazyCell {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "managed lazy identity cell",
        "src/core/managed/recursive_cells.rs",
        "no direct Drop implementation",
        "source, result, mutex, and one-write state destroy passively",
    );
}

// SAFETY: assignment payloads passed the same passive compatibility closure.
// Completion registrations, weak coordinator routing inside the producer
// obligation, and the producer backlink contain no managed semantic edge and
// invoke no service on Drop.
unsafe impl ManagedFamily for ManagedPromiseCell {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "managed promise identity cell",
        "src/core/managed/recursive_cells.rs",
        "no direct Drop implementation",
        "assignment, subscriptions, weak routes, and one-write state destroy passively",
    );
}

// SAFETY: RuntimeNetCell's Drop only closes its edge-free disturbance signal.
// Net topology and payloads destroy passively; no runtime, evaluator,
// scheduler, host callback, or registered root is reachable from this cell.
unsafe impl ManagedFamily for ManagedCoreNetCell {
    const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
        "managed core interaction-net cell",
        "src/core/managed/recursive_cells.rs",
        "direct Drop closes only the edge-free disturbance companion",
        "runtime topology, payloads, mutexes, and revisions destroy passively",
    );
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
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::core::{CoreValueFactory, EvaluatedValue, LazySource};
    use crate::interaction_net::{NetBuilder, PreparedCopySource};
    use crate::runtime::{RuntimeIds, RuntimeMutationAdmission, allocate_evaluation_runtime_id};

    fn new_values() -> CoreValueFactory {
        CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
    }

    fn prepared_runtime(value: i64) -> RuntimeNet<CoreSpecialization> {
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let exposed = builder.data(Value::Number(value.into()));
        builder.finish(exposed).instantiate()
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
    fn promise_publication_callbacks_observe_assignment_before_wake_detachment() {
        let values = new_values();
        let admission = RuntimeMutationAdmission::new();
        let coordinator =
            EvaluationWorkCoordinator::new_for_test(values.clone(), admission.clone());

        values.with_runtime_value_access(|access| {
            let root = access
                .root_new_managed_promise(&values, "publication ordering")
                .expect("the managed promise cell should fit a run");
            let promise = root.access(&access).unwrap();
            let detached = promise
                .publish_detached(Ok(Value::Number(31.into())), |assignment| {
                    assignment.clone()
                })
                .expect("the first detached publication should win");
            assert_eq!(detached, Ok(Value::Number(31.into())));

            let guarded_root = access
                .root_new_managed_promise(&values, "guarded publication ordering")
                .expect("the managed promise cell should fit a run");
            let guarded = guarded_root.access(&access).unwrap();
            let mutation = admission.mutation_guard();
            let (observed, wake) = guarded
                .publish_guarded(
                    &coordinator,
                    &mutation,
                    Ok(Value::Number(47.into())),
                    |assignment| assignment.clone(),
                )
                .expect("the first guarded publication should win");
            assert_eq!(observed, Ok(Value::Number(47.into())));
            drop(mutation);
            wake.notify();
        });
    }

    #[test]
    fn bounded_core_net_gateway_preserves_cell_mutation_publication() {
        let values = new_values();
        let observer = values.runtime_value_observer();
        let cell = ManagedCoreNetCell::new(prepared_runtime(5));

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

    #[test]
    fn recursive_cell_family_contracts_and_registered_root_lifecycles() {
        assert_eq!(
            <ManagedLazyCell as ManagedFamily>::DROP_RECORD.fields(),
            (
                "managed lazy identity cell",
                "src/core/managed/recursive_cells.rs",
                "no direct Drop implementation",
                "source, result, mutex, and one-write state destroy passively",
            )
        );
        assert_eq!(
            <ManagedPromiseCell as ManagedFamily>::DROP_RECORD.fields(),
            (
                "managed promise identity cell",
                "src/core/managed/recursive_cells.rs",
                "no direct Drop implementation",
                "assignment, subscriptions, weak routes, and one-write state destroy passively",
            )
        );
        assert_eq!(
            <ManagedCoreNetCell as ManagedFamily>::DROP_RECORD.fields(),
            (
                "managed core interaction-net cell",
                "src/core/managed/recursive_cells.rs",
                "direct Drop closes only the edge-free disturbance companion",
                "runtime topology, payloads, mutexes, and revisions destroy passively",
            )
        );

        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the recursive-cell heap should start collectible");
        let (lazy, promise, net) = values.with_runtime_value_access(|access| {
            (
                access
                    .root_new_managed_lazy(&values, "rooted lazy", LazySource::Error)
                    .expect("the managed lazy cell should fit a run"),
                access
                    .root_new_managed_promise(&values, "rooted promise")
                    .expect("the managed promise cell should fit a run"),
                access
                    .root_new_managed_core_net(&values, prepared_runtime(17))
                    .expect("the managed core-net cell should fit a run"),
            )
        });

        values.with_runtime_value_access(|access| {
            assert_eq!(
                lazy.access(&access).unwrap().label().as_ref(),
                "rooted lazy"
            );
            assert_eq!(
                promise.access(&access).unwrap().label().as_ref(),
                "rooted promise"
            );
            assert_eq!(
                net.access(&access)
                    .unwrap()
                    .with(|runtime| runtime.interface_data(runtime.exposed()).cloned()),
                Some(Value::Number(17.into()))
            );
        });

        let unrelated = new_values();
        unrelated.with_runtime_value_access(|access| {
            assert!(lazy.access(&access).is_none());
            assert!(promise.access(&access).is_none());
            assert!(net.access(&access).is_none());
        });

        let live = values
            .collect_managed_for_test()
            .expect("all three registered recursive-cell roots should survive");
        assert_eq!(live.root_entries(), baseline.root_entries() + 3);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 3);

        drop((lazy, promise, net));
        let dead = values
            .collect_managed_for_test()
            .expect("unrooted recursive cells should be reclaimed");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 3);
    }

    #[test]
    fn semantic_edges_and_durable_roots_share_one_managed_identity() {
        let values = new_values();
        let observer = values.runtime_value_observer();
        let (lazy, promise, net, lazy_edge, promise_edge, net_edge) = values
            .with_runtime_value_access(|access| {
                let lazy_edge = access
                    .allocate_managed_lazy(&values, "split lazy", LazySource::Error)
                    .expect("the managed lazy cell should fit a run");
                let promise_edge = access
                    .allocate_managed_promise(&values, "split promise")
                    .expect("the managed promise cell should fit a run");
                let net_edge = access
                    .allocate_managed_core_net(&values, prepared_runtime(61))
                    .expect("the managed core-net cell should fit a run");

                assert_eq!(
                    lazy_edge
                        .access(&observer, &access)
                        .expect("a fresh lazy edge should be accessible")
                        .label()
                        .as_ref(),
                    "split lazy"
                );
                assert_eq!(
                    promise_edge
                        .access(&observer, &access)
                        .expect("a fresh promise edge should be accessible")
                        .label()
                        .as_ref(),
                    "split promise"
                );
                assert_eq!(
                    net_edge
                        .access(&observer, &access)
                        .expect("a fresh net edge should be accessible")
                        .with(|runtime| runtime.interface_data(runtime.exposed()).cloned()),
                    Some(Value::Number(61.into()))
                );

                (
                    access.root_managed_lazy(observer.clone(), lazy_edge),
                    access.root_managed_promise(observer.clone(), promise_edge),
                    access.root_managed_core_net(observer.clone(), net_edge),
                    lazy_edge,
                    promise_edge,
                    net_edge,
                )
            });

        values.with_runtime_value_access(|access| {
            assert_eq!(
                lazy.access(&access).unwrap().id(),
                lazy_edge.access(&observer, &access).unwrap().id()
            );
            assert_eq!(
                promise.access(&access).unwrap().id(),
                promise_edge.access(&observer, &access).unwrap().id()
            );
            assert_eq!(
                net.access(&access)
                    .unwrap()
                    .with(|runtime| runtime.exposed()),
                net_edge
                    .access(&observer, &access)
                    .unwrap()
                    .with(|runtime| runtime.exposed())
            );
        });

        let unrelated = new_values();
        unrelated.with_runtime_value_access(|access| {
            assert!(lazy_edge.access(&observer, &access).is_none());
            assert!(promise_edge.access(&observer, &access).is_none());
            assert!(net_edge.access(&observer, &access).is_none());
        });
    }

    #[test]
    fn managed_core_net_source_self_cycle_is_traced_and_reclaimed() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the managed-net cycle fixture should start collectible");
        let observer = values.runtime_value_observer();
        let root = values.with_runtime_value_access(|access| {
            let allocator = access
                .allocator::<ManagedCoreNetCell>()
                .expect("the managed core-net cell should fit a run");
            let edge = allocator.alloc(ManagedCoreNetCell::new(prepared_runtime(23)));
            let managed_edge = ManagedCoreNetEdge(edge);

            // SAFETY: `edge` is live in this access region's exact heap and
            // representation. The replacement adds precisely the self edge
            // reported to the collector gateway.
            unsafe {
                let cell = access.scope.get_traced_edge(edge);
                let remote = cell.runtime.with(RuntimeNet::exposed);
                access
                    .scope
                    .mutator
                    .with_edge_replacement(edge, None, Some(edge), || {
                        cell.runtime.with_mut(|runtime| {
                            runtime.begin_copy(PreparedCopySource::new(
                                crate::core_net::CoreRuntimeNet::from_managed_parts(
                                    managed_edge,
                                    observer.clone(),
                                ),
                                remote,
                            ));
                        });
                    });
            }

            access.root_managed_core_net(observer, managed_edge)
        });

        let live = values
            .collect_managed_for_test()
            .expect("a rooted managed-net self-cycle should survive");
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 1);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("an unrooted managed-net self-cycle should be reclaimed");
        assert_eq!(dead.finalized_slots(), 1);
    }

    #[test]
    fn recursive_cell_gateways_are_private_and_complete() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let owner_path = manifest.join("src/core/managed/recursive_cells.rs");
        let inventory_path = manifest.join("src/core/managed/recursive_identity_inventory.rs");
        let owner = fs::read_to_string(&owner_path).expect("the recursive-cell source should read");
        let count = |parts: &[&str]| owner.matches(&parts.concat()).count();

        assert_eq!(count(&["fn root_new_", "managed_"]), 3);
        assert_eq!(count(&["fn allocate_", "managed_"]), 3);
        assert_eq!(count(&["fn root_", "managed_"]), 3);
        assert_eq!(count(&["fn access<'", "access"]), 6);
        assert_eq!(count(&["fn from_authorized_", "cell"]), 3);
        assert_eq!(count(&["unsafe impl Trace for Managed", "LazyCell"]), 1);
        assert_eq!(count(&["unsafe impl Trace for Managed", "PromiseCell"]), 1);
        assert_eq!(count(&["unsafe impl Trace for Managed", "CoreNetCell"]), 1);
        assert_eq!(
            count(&["unsafe impl ManagedFamily for Managed", "LazyCell"]),
            1
        );
        assert_eq!(
            count(&["unsafe impl ManagedFamily for Managed", "PromiseCell"]),
            1
        );
        assert_eq!(
            count(&["unsafe impl ManagedFamily for Managed", "CoreNetCell"]),
            1
        );

        let mut stack = vec![manifest.join("src")];
        let mut escaped = Vec::new();
        while let Some(directory) = stack.pop() {
            for entry in fs::read_dir(directory).expect("the source tree should be readable") {
                let path = entry.expect("a source entry should be readable").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs")
                    || path == owner_path
                    || path == inventory_path
                {
                    continue;
                }
                let source = fs::read_to_string(&path).expect("Rust source should be readable");
                let names = [
                    "ManagedLazyCell",
                    "ManagedPromiseCell",
                    "ManagedCoreNetCell",
                ];
                let legacy_arc_cells = [
                    ["Arc<", "LazyCell"].concat(),
                    ["Arc<", "PromiseCell"].concat(),
                    ["Arc<", "RuntimeNetCell<CoreSpecialization"].concat(),
                ];
                if names.iter().any(|name| source.contains(name))
                    || legacy_arc_cells.iter().any(|name| source.contains(name))
                {
                    escaped.push(
                        path.strip_prefix(manifest)
                            .expect("source should belong to the package")
                            .to_path_buf(),
                    );
                }
            }
        }
        assert!(
            escaped.is_empty(),
            "managed recursive-cell representations escaped their private module: {escaped:?}"
        );
    }

    #[test]
    fn managed_cells_and_coordination_companions_have_closed_roles() {
        let source = include_str!("recursive_cells.rs");
        let declaration = |name: &str| {
            let start = source
                .find(name)
                .unwrap_or_else(|| panic!("missing declaration {name}"));
            let tail = &source[start..];
            let end = tail
                .find("\n}")
                .unwrap_or_else(|| panic!("unterminated declaration {name}"));
            &tail[..end]
        };

        for cell in [
            "struct ManagedLazyCell",
            "struct ManagedPromiseCell",
            "struct ManagedCoreNetCell",
        ] {
            let body = declaration(cell);
            assert!(!body.contains("Root<"), "{cell} must not retain a root");
            assert!(
                !body.contains("RuntimeValueRoot"),
                "{cell} must not retain a compatibility root"
            );
        }
        assert!(
            declaration("struct ManagedPromiseCell")
                .contains("producer: OnceLock<Arc<PromiseProducerObligation>>")
        );

        let completion = include_str!("../../evaluation/coordinator/completion.rs");
        let companion = {
            let start = completion
                .find("pub(crate) struct CompletionSubscriptions")
                .expect("completion companion declaration should remain present");
            let tail = &completion[start..];
            &tail[..tail
                .find("\n}")
                .expect("completion companion declaration should terminate")]
        };
        for forbidden in ["Gc<", "Root<", "Value", "PromiseProducerObligation"] {
            assert!(
                !companion.contains(forbidden),
                "completion coordination acquired a semantic edge: {forbidden}"
            );
        }
    }

    #[test]
    fn atomic_cutover_destinations_are_complete_for_each_identity_family() {
        let source = include_str!("recursive_cells.rs");
        let require = |family: &str, parts: &[&str]| {
            let fragment = parts.concat();
            assert!(
                source.contains(&fragment),
                "{family} has no managed destination for {fragment}"
            );
        };

        for (family, cell, edge, root, access, constructor, publication) in [
            (
                "lazy",
                "ManagedLazyCell",
                "ManagedLazyEdge",
                "ManagedLazyRoot",
                "ManagedLazyAccess",
                "root_new_managed_lazy",
                "fn cache",
            ),
            (
                "promise",
                "ManagedPromiseCell",
                "ManagedPromiseEdge",
                "ManagedPromiseRoot",
                "ManagedPromiseAccess",
                "root_new_managed_promise",
                "fn publish",
            ),
            (
                "core net",
                "ManagedCoreNetCell",
                "ManagedCoreNetEdge",
                "ManagedCoreNetRoot",
                "ManagedCoreNetAccess",
                "root_new_managed_core_net",
                "fn with_mut",
            ),
        ] {
            require(family, &["struct ", cell]);
            require(family, &["struct ", edge, "(", "Gc<", cell, ">)"]);
            require(family, &["struct ", root]);
            require(family, &["struct ", access]);
            require(family, &["fn allocate_", "managed_"]);
            require(family, &["fn root_", "managed_"]);
            require(family, &["fn ", constructor]);
            require(family, &[publication]);
            require(family, &["unsafe impl Trace for ", cell]);
            require(family, &["unsafe impl ManagedFamily for ", cell]);
        }

        require("promise", &["OnceLock<Arc<", "PromiseProducerObligation>>"]);
        require("promise", &["fn publish_", "detached"]);
        require("promise", &["fn publish_", "guarded"]);
        require("promise", &["Completion", "Subscriptions"]);
        require("core net", &["RuntimeNet", "Cell<CoreSpecialization>"]);
    }
}
