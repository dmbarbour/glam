//! Managed representations for the three recursive identity cells.
//!
//! I5C prepared these layouts and their access roles. I5D routes lazy,
//! promise, and core-net production identities through them as one exact
//! traced graph.

use std::cell::RefCell;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use glam_gc::{Gc, Root, Trace, UnsupportedLayout, Visitor};

use crate::core::{
    CoreValueFactory, EvaluationFailure, LazyId, LazyResult, LazySource, LazyValue, PromiseId,
    PromisedValue, RuntimeValueAccess, Value,
};
use crate::core_net::{CoreRuntimeNet, CoreSpecialization};
use crate::evaluation::{
    CompletionSubscriptionOutcome, CompletionSubscriptions, CompletionWake,
    EvaluationWorkCoordinator, PromiseProducerObligation, WakeRegistration,
};
use crate::interaction_net::{
    RuntimeNet, RuntimeNetCell, RuntimeNetEdgeTransition, RuntimeNetMutationGateway,
    RuntimeNetPayload,
};
use crate::runtime::RuntimeMutationAuthority;

use super::payload_edges::{
    trace_core_operator_managed_net_edges, trace_lazy_source_managed_net_edges,
    visit_compatibility_managed_edges, visit_compatibility_payload_managed_edges,
    visit_halt_value_edges,
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
/// promise so observers retain its producer provenance; the obligation's
/// concrete wait state and coordinator/local-owner routes are weak, so this
/// backlink cannot retain terminal roots, the task registry, or an ownership
/// cycle. Outstanding external wait handles retain late terminal observation.
pub(crate) struct ManagedPromiseCell {
    id: PromiseId,
    assignment: OnceLock<ManagedPromiseAssignment>,
    terminal: Arc<AtomicBool>,
    completion: Arc<CompletionSubscriptions>,
    producer: Arc<OnceLock<Arc<PromiseProducerObligation>>>,
}

/// Synchronization-owning managed core interaction-net identity.
///
/// The owner-neutral generic cell remains the sole topology/revision mutex.
/// I5C.3 supplied its exact payload trace; I5D installed the managed edge and
/// registered-root holders atomically.
pub(crate) struct ManagedCoreNetCell {
    runtime: RuntimeNetCell<CoreSpecialization>,
}

#[derive(Clone)]
pub(crate) struct ManagedLazyEdge(Gc<ManagedLazyCell>);

impl PartialEq for ManagedLazyEdge {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(other.0)
    }
}

impl Eq for ManagedLazyEdge {}

#[derive(Clone)]
pub(crate) struct ManagedPromiseEdge(Gc<ManagedPromiseCell>);

impl PartialEq for ManagedPromiseEdge {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(other.0)
    }
}

impl Eq for ManagedPromiseEdge {}

#[derive(Clone)]
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
    root: Root<ManagedLazyCell>,
}

#[derive(Clone)]
pub(crate) struct ManagedPromiseRoot {
    id: PromiseId,
    root: Root<ManagedPromiseCell>,
    terminal: Arc<AtomicBool>,
    completion: Arc<CompletionSubscriptions>,
    producer: Arc<OnceLock<Arc<PromiseProducerObligation>>>,
}

impl fmt::Debug for ManagedPromiseRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManagedPromiseRoot(..)")
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ManagedCoreNetRoot {
    root: Root<ManagedCoreNetCell>,
}

/// A non-escaping lazy-cell observation authorized by one runtime value scope.
pub(crate) struct ManagedLazyAccess<'access, 'scope> {
    owner: ManagedLazyEdge,
    cell: &'access ManagedLazyCell,
    authority: &'access RuntimeValueAccess<'scope>,
    _thread_bound: PhantomData<Rc<()>>,
}

/// A non-escaping promise-cell observation authorized by one runtime value scope.
pub(crate) struct ManagedPromiseAccess<'access, 'scope> {
    owner: ManagedPromiseEdge,
    cell: &'access ManagedPromiseCell,
    authority: &'access RuntimeValueAccess<'scope>,
    _thread_bound: PhantomData<Rc<()>>,
}

/// A non-escaping core-net observation authorized by one runtime value scope.
pub(crate) struct ManagedCoreNetAccess<'access, 'scope> {
    owner: ManagedCoreNetEdge,
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
    fn new(values: &crate::core::CoreValueFactory) -> Self {
        let id = PromiseId(values.deferred_value_id());
        Self {
            id,
            assignment: OnceLock::new(),
            terminal: Arc::new(AtomicBool::new(false)),
            completion: Arc::new(CompletionSubscriptions::for_promise(
                values.runtime_id(),
                id,
                values.work_coordinator_binding(),
            )),
            producer: Arc::new(OnceLock::new()),
        }
    }
}

impl RuntimeValueAccess<'_> {
    fn allocate_managed_lazy(
        &self,
        label: impl Into<Arc<str>>,
        source: LazySource,
    ) -> Result<ManagedLazyEdge, UnsupportedLayout> {
        let allocator = self.allocator::<ManagedLazyCell>()?;
        Ok(ManagedLazyEdge(allocator.alloc(ManagedLazyCell::new(
            self.values(),
            label,
            source,
        ))))
    }

    fn allocate_managed_promise(
        &self,
        _label: impl Into<Arc<str>>,
    ) -> Result<ManagedPromiseEdge, UnsupportedLayout> {
        let allocator = self.allocator::<ManagedPromiseCell>()?;
        Ok(ManagedPromiseEdge(
            allocator.alloc(ManagedPromiseCell::new(self.values())),
        ))
    }

    fn allocate_managed_core_net(
        &self,
        runtime: RuntimeNet<CoreSpecialization>,
    ) -> Result<ManagedCoreNetEdge, UnsupportedLayout> {
        let allocator = self.allocator::<ManagedCoreNetCell>()?;
        Ok(ManagedCoreNetEdge(
            allocator.alloc(ManagedCoreNetCell::new(runtime)),
        ))
    }

    /// Constructs one lazy facade inside this already-admitted region.
    ///
    /// The returned facade is an interior semantic edge, not an owner. Code
    /// which returns it from the region must first install it below an exact
    /// traced owner or use [`Self::construct_rooted_managed_lazy`].
    pub(in crate::core) fn construct_managed_lazy(
        &self,
        label: impl Into<Arc<str>>,
        source: LazySource,
    ) -> Result<LazyValue, UnsupportedLayout> {
        let edge = self.allocate_managed_lazy(label, source)?;
        Ok(LazyValue { edge })
    }

    /// Constructs an already-terminal failed lazy before publishing its
    /// facade from this region.
    pub(in crate::core) fn construct_failed_managed_lazy(
        &self,
        label: impl Into<Arc<str>>,
        failure: Arc<EvaluationFailure>,
    ) -> Result<LazyValue, UnsupportedLayout> {
        let lazy = self.construct_managed_lazy(label, LazySource::Error)?;
        let root = lazy.root_in(self);
        let result = root.cache(self, Err(failure));
        debug_assert!(result.is_err(), "new lazy errors must cache a failure");
        Ok(lazy)
    }

    /// Constructs one promise facade inside this already-admitted region.
    /// The facade must acquire a traced or registered owner before it escapes.
    pub(crate) fn construct_managed_promise(
        &self,
        label: impl Into<Arc<str>>,
    ) -> Result<PromisedValue, UnsupportedLayout> {
        let edge = self.allocate_managed_promise(label)?;
        Ok(PromisedValue { edge })
    }

    /// Constructs one core-net facade inside this already-admitted region.
    /// The facade must acquire a traced or registered owner before it escapes.
    pub(crate) fn construct_managed_core_net(
        &self,
        runtime: RuntimeNet<CoreSpecialization>,
    ) -> Result<CoreRuntimeNet, UnsupportedLayout> {
        let edge = self.allocate_managed_core_net(runtime)?;
        Ok(CoreRuntimeNet::from_managed_edge(edge))
    }

    pub(in crate::core) fn root_managed_lazy(&self, edge: &ManagedLazyEdge) -> ManagedLazyRoot {
        let value = edge.access(self);
        let id = value.id();
        let label = value.label().clone();
        ManagedLazyRoot {
            id,
            label,
            root: self.root(self.duplicate_edge(&edge.0)),
        }
    }

    pub(in crate::core) fn root_managed_promise(
        &self,
        edge: &ManagedPromiseEdge,
    ) -> ManagedPromiseRoot {
        let value = edge.access(self);
        let id = value.id();
        ManagedPromiseRoot {
            id,
            root: self.root(self.duplicate_edge(&edge.0)),
            terminal: Arc::clone(&value.cell.terminal),
            completion: Arc::clone(&value.cell.completion),
            producer: Arc::clone(&value.cell.producer),
        }
    }

    pub(crate) fn root_managed_core_net(&self, edge: &ManagedCoreNetEdge) -> ManagedCoreNetRoot {
        ManagedCoreNetRoot {
            root: self.root(self.duplicate_edge(&edge.0)),
        }
    }

    /// Constructs one lazy and publishes its explicit registered owner before
    /// this access region ends. A facade can be projected from the root while
    /// that owner remains live.
    #[cfg(test)]
    pub(in crate::core) fn construct_rooted_managed_lazy(
        &self,
        label: impl Into<Arc<str>>,
        source: LazySource,
    ) -> Result<ManagedLazyRoot, UnsupportedLayout> {
        let lazy = self.construct_managed_lazy(label, source)?;
        Ok(self.root_managed_lazy(&lazy.edge))
    }

    /// Constructs one promise and publishes its explicit registered owner
    /// before this access region ends. A facade can be projected from the root
    /// while that owner remains live.
    #[allow(
        dead_code,
        reason = "GCI5R-001C establishes rooted handoff before the D-F production cutovers"
    )]
    pub(crate) fn construct_rooted_managed_promise(
        &self,
        label: impl Into<Arc<str>>,
    ) -> Result<ManagedPromiseRoot, UnsupportedLayout> {
        let promise = self.construct_managed_promise(label)?;
        Ok(self.root_managed_promise(&promise.edge))
    }

    /// Constructs one core net and publishes its explicit registered owner
    /// before this access region ends.
    #[allow(
        dead_code,
        reason = "GCI5R-001C establishes rooted handoff before the D-F production cutovers"
    )]
    pub(crate) fn construct_rooted_managed_core_net(
        &self,
        runtime: RuntimeNet<CoreSpecialization>,
    ) -> Result<ManagedCoreNetRoot, UnsupportedLayout> {
        let edge = self.allocate_managed_core_net(runtime)?;
        Ok(self.root_managed_core_net(&edge))
    }
}

impl ManagedLazyEdge {
    pub(crate) fn trace(&self, visitor: &mut Visitor<'_>) {
        visitor.visit(&self.0);
    }

    #[inline(always)]
    fn duplicate_in(&self, authority: &RuntimeValueAccess<'_>) -> Self {
        Self(authority.duplicate_edge(&self.0))
    }

    #[inline(always)]
    #[allow(
        dead_code,
        reason = "P2B installs explicit lazy identity before parent raw-Value equality migrates"
    )]
    pub(crate) fn same_allocation_in(
        &self,
        other: &Self,
        authority: &RuntimeValueAccess<'_>,
    ) -> bool {
        authority.same_edge(&self.0, &other.0)
    }

    pub(crate) fn access<'access, 'scope>(
        &self,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> ManagedLazyAccess<'access, 'scope> {
        // SAFETY: this private edge can only be constructed by the matching
        // value domain, and it may escape that construction region only below
        // a registered root or another traced value in the same graph. The
        // caller reaches it from that live owner while holding this graph's
        // access region. Public values reject foreign-runtime composition,
        // and glam-gc debug builds recheck heap and representation provenance.
        let cell = unsafe { authority.scope.get_traced_edge(&self.0) };
        ManagedLazyAccess::from_authorized_cell(self.duplicate_in(authority), cell, authority)
    }
}

impl ManagedPromiseEdge {
    pub(crate) fn trace(&self, visitor: &mut Visitor<'_>) {
        visitor.visit(&self.0);
    }

    #[inline(always)]
    fn duplicate_in(&self, authority: &RuntimeValueAccess<'_>) -> Self {
        Self(authority.duplicate_edge(&self.0))
    }

    #[inline(always)]
    #[allow(
        dead_code,
        reason = "P2B installs explicit promise identity before EvaluationHalt equality migrates"
    )]
    pub(crate) fn same_allocation_in(
        &self,
        other: &Self,
        authority: &RuntimeValueAccess<'_>,
    ) -> bool {
        authority.same_edge(&self.0, &other.0)
    }

    pub(crate) fn access<'access, 'scope>(
        &self,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> ManagedPromiseAccess<'access, 'scope> {
        // SAFETY: the private constructor preserves exact heap and
        // representation provenance, and the edge may escape its construction
        // region only below a registered root or another traced value in the
        // same graph. The caller reaches it from that live owner while holding
        // this graph's access region. Public values reject foreign-runtime
        // composition, and glam-gc debug builds recheck heap and type metadata.
        let cell = unsafe { authority.scope.get_traced_edge(&self.0) };
        ManagedPromiseAccess::from_authorized_cell(self.duplicate_in(authority), cell, authority)
    }
}

impl ManagedCoreNetEdge {
    pub(crate) fn trace(&self, visitor: &mut Visitor<'_>) {
        visitor.visit(&self.0);
    }

    #[inline(always)]
    fn duplicate_in(&self, authority: &RuntimeValueAccess<'_>) -> Self {
        Self(authority.duplicate_edge(&self.0))
    }

    #[inline(always)]
    pub(crate) fn same_allocation_in(
        &self,
        other: &Self,
        authority: &RuntimeValueAccess<'_>,
    ) -> bool {
        authority.same_edge(&self.0, &other.0)
    }

    pub(crate) fn access<'access, 'scope>(
        &self,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> ManagedCoreNetAccess<'access, 'scope> {
        // SAFETY: private construction and publication preserve exact heap and
        // representation provenance; the caller reaches this edge through a
        // live traced owner while supplying current matching-graph access.
        let cell = unsafe { authority.scope.get_traced_edge(&self.0) };
        ManagedCoreNetAccess::from_authorized_cell(self.duplicate_in(authority), cell, authority)
    }
}

impl ManagedLazyRoot {
    pub(crate) fn id(&self) -> LazyId {
        self.id
    }

    pub(crate) fn label(&self) -> &Arc<str> {
        &self.label
    }

    pub(crate) fn edge(&self, authority: &RuntimeValueAccess<'_>) -> ManagedLazyEdge {
        ManagedLazyEdge(authority.project_root(&self.root))
    }

    pub(crate) fn access<'access, 'scope>(
        &'access self,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<ManagedLazyAccess<'access, 'scope>> {
        if !authority.admits_root(&self.root) {
            return None;
        }
        let edge = self.edge(authority);
        Some(ManagedLazyAccess::from_authorized_cell(
            edge,
            authority.get(&self.root),
            authority,
        ))
    }

    /// Publishes one terminal result through this registered owner.
    ///
    /// The caller supplies a bounded matching-runtime access region. The root
    /// is the durable authority which keeps the cell live across publication;
    /// semantic lazy facades do not carry mutation authority.
    pub(crate) fn cache(
        &self,
        authority: &RuntimeValueAccess<'_>,
        result: LazyResult,
    ) -> LazyResult {
        self.access(authority)
            .expect("lazy root and publication access must share one value domain")
            .cache(result)
    }
}

impl ManagedPromiseRoot {
    pub(crate) fn id(&self) -> PromiseId {
        self.id
    }

    pub(crate) fn edge(&self, authority: &RuntimeValueAccess<'_>) -> ManagedPromiseEdge {
        ManagedPromiseEdge(authority.project_root(&self.root))
    }

    pub(crate) fn same_promise(&self, other: &Self) -> bool {
        self.root.ptr_eq(&other.root)
    }

    pub(crate) fn runtime_id(&self) -> crate::runtime::EvaluationRuntimeId {
        self.completion.runtime_id()
    }

    pub(crate) fn producer(&self) -> Option<Arc<PromiseProducerObligation>> {
        self.producer.get().cloned()
    }

    pub(crate) fn subscribe_work(
        &self,
        runtime: crate::runtime::EvaluationRuntimeId,
        registration: WakeRegistration,
    ) -> CompletionSubscriptionOutcome {
        self.completion.subscribe(runtime, registration, || {
            self.terminal.load(Ordering::Acquire)
        })
    }

    pub(crate) fn unsubscribe_work(&self, registration: WakeRegistration) -> bool {
        self.completion.unsubscribe(registration)
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::Acquire)
    }

    pub(crate) fn access<'access, 'scope>(
        &'access self,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<ManagedPromiseAccess<'access, 'scope>> {
        if !authority.admits_root(&self.root) {
            return None;
        }
        let edge = self.edge(authority);
        Some(ManagedPromiseAccess::from_authorized_cell(
            edge,
            authority.get(&self.root),
            authority,
        ))
    }

    /// Publishes one terminal assignment through this registered owner.
    ///
    /// Managed access is bounded to the assignment transition. Completion
    /// wakes and producer notifications run only after that region, and any
    /// coordinator mutation admission it used, have been released.
    pub(crate) fn publish(
        &self,
        values: &CoreValueFactory,
        assignment: ManagedPromiseAssignment,
    ) -> Result<(), ManagedPromiseAssignment> {
        assert_eq!(
            values.runtime_id(),
            self.runtime_id(),
            "promise root and publication factory must share one value domain"
        );
        let producer = self.producer();
        if let Some(producer) = producer
            && let Some(coordinator) = producer.coordinator()
        {
            let mutation = coordinator.mutation_guard();
            let published = values.with_runtime_value_access(|authority| {
                self.publish_guarded(
                    &authority,
                    &coordinator,
                    &mutation,
                    assignment,
                    |assignment| {
                        producer.publish_assignment_guarded(&coordinator, &mutation, assignment)
                    },
                )
            });
            let (producer, completion) = published?;
            drop(mutation);
            completion.notify();
            producer.notify();
            return Ok(());
        }

        let producer = values.with_runtime_value_access(|authority| {
            self.publish_detached(&authority, assignment, |assignment| {
                self.producer()
                    .map(|producer| producer.publish_assignment_detached(assignment))
            })
        })?;
        if let Some(producer) = producer {
            producer.notify();
        }
        Ok(())
    }

    pub(crate) fn install_producer(
        &self,
        authority: &RuntimeValueAccess<'_>,
        producer: &Arc<PromiseProducerObligation>,
    ) -> Result<(), Arc<PromiseProducerObligation>> {
        self.access(authority)
            .expect("promise root and producer access must share one value domain")
            .install_producer(producer)
    }

    pub(crate) fn publish_detached<T>(
        &self,
        authority: &RuntimeValueAccess<'_>,
        assignment: ManagedPromiseAssignment,
        after_assignment: impl FnOnce(&ManagedPromiseAssignment) -> T,
    ) -> Result<T, ManagedPromiseAssignment> {
        self.access(authority)
            .expect("promise root and publication access must share one value domain")
            .publish_detached(assignment, after_assignment)
    }

    pub(crate) fn publish_guarded<T>(
        &self,
        authority: &RuntimeValueAccess<'_>,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        assignment: ManagedPromiseAssignment,
        after_assignment: impl FnOnce(&ManagedPromiseAssignment) -> T,
    ) -> Result<(T, CompletionWake), ManagedPromiseAssignment> {
        self.access(authority)
            .expect("promise root and publication access must share one value domain")
            .publish_guarded(coordinator, mutation, assignment, after_assignment)
    }
}

impl ManagedCoreNetRoot {
    pub(crate) fn edge(&self, authority: &RuntimeValueAccess<'_>) -> ManagedCoreNetEdge {
        ManagedCoreNetEdge(authority.project_root(&self.root))
    }

    #[cfg(test)]
    pub(crate) fn same_root_in(&self, other: &Self, authority: &RuntimeValueAccess<'_>) -> bool {
        self.edge(authority)
            .same_allocation_in(&other.edge(authority), authority)
    }

    #[cfg(test)]
    pub(crate) fn access<'access, 'scope>(
        &'access self,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Option<ManagedCoreNetAccess<'access, 'scope>> {
        if !authority.admits_root(&self.root) {
            return None;
        }
        let edge = self.edge(authority);
        Some(ManagedCoreNetAccess {
            owner: edge,
            cell: authority.get(&self.root),
            authority,
            _thread_bound: PhantomData,
        })
    }
}

impl<'access, 'scope> ManagedLazyAccess<'access, 'scope> {
    fn from_authorized_cell(
        owner: ManagedLazyEdge,
        cell: &'access ManagedLazyCell,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Self {
        Self {
            owner,
            cell,
            authority,
            _thread_bound: PhantomData,
        }
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

    fn cache(&self, result: LazyResult) -> LazyResult {
        let proposed = RefCell::new(Some(result));
        // SAFETY: `self.owner` is the exact live lazy cell authorized by this
        // access. The leaving visitor snapshots its pre-transition source
        // under the source mutex, while the adding visitor reports the
        // proposed terminal result without consuming it. The closure publishes
        // exactly one terminal winner before removing the old source graph.
        unsafe {
            self.authority.with_managed_edge_transition(
                &self.owner.0,
                |visitor| trace_lazy_source_cell(self.cell, visitor),
                |visitor| {
                    trace_lazy_result(
                        proposed
                            .borrow()
                            .as_ref()
                            .expect("lazy transition must retain its proposed result"),
                        visitor,
                    );
                },
                || {
                    let proposed = proposed
                        .borrow_mut()
                        .take()
                        .expect("lazy transition must consume its proposed result once");
                    let _ = self.cell.result.set(proposed);
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
                },
            )
        }
    }
}

impl<'access, 'scope> ManagedPromiseAccess<'access, 'scope> {
    fn from_authorized_cell(
        owner: ManagedPromiseEdge,
        cell: &'access ManagedPromiseCell,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Self {
        Self {
            owner,
            cell,
            authority,
            _thread_bound: PhantomData,
        }
    }

    pub(crate) fn id(&self) -> PromiseId {
        self.cell.id
    }

    #[cfg(test)]
    pub(crate) fn runtime_id(&self) -> crate::runtime::EvaluationRuntimeId {
        self.authority.runtime_id()
    }

    pub(crate) fn assignment(&self) -> Option<ManagedPromiseAssignment> {
        self.cell.assignment.get().cloned()
    }

    fn install_producer(
        &self,
        producer: &Arc<PromiseProducerObligation>,
    ) -> Result<(), Arc<PromiseProducerObligation>> {
        self.cell.producer.set(Arc::clone(producer))
    }

    pub(crate) fn producer(&self) -> Option<Arc<PromiseProducerObligation>> {
        self.cell.producer.get().cloned()
    }

    #[cfg(test)]
    fn publish(
        &self,
        assignment: ManagedPromiseAssignment,
    ) -> Result<(), ManagedPromiseAssignment> {
        self.publish_detached(assignment, |_| ())
    }

    fn publish_detached<T>(
        &self,
        assignment: ManagedPromiseAssignment,
        after_assignment: impl FnOnce(&ManagedPromiseAssignment) -> T,
    ) -> Result<T, ManagedPromiseAssignment> {
        self.cell
            .completion
            .publish(|| self.publish_assignment(assignment, after_assignment))
    }

    fn publish_guarded<T>(
        &self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        assignment: ManagedPromiseAssignment,
        after_assignment: impl FnOnce(&ManagedPromiseAssignment) -> T,
    ) -> Result<(T, CompletionWake), ManagedPromiseAssignment> {
        self.cell
            .completion
            .publish_guarded(coordinator, mutation, || {
                self.publish_assignment(assignment, after_assignment)
            })
    }

    fn publish_assignment<T>(
        &self,
        assignment: ManagedPromiseAssignment,
        after_assignment: impl FnOnce(&ManagedPromiseAssignment) -> T,
    ) -> Result<T, ManagedPromiseAssignment> {
        let proposed = RefCell::new(Some(assignment));
        // SAFETY: `self.owner` is the exact live promise cell authorized by
        // this access. Promise assignment has no leaving edge, and the adding
        // visitor reports the complete proposed terminal assignment. The
        // closure performs the representation's one-write publication.
        unsafe {
            self.authority.with_managed_edge_transition(
                &self.owner.0,
                |_visitor| (),
                |visitor| {
                    trace_promise_assignment(
                        proposed
                            .borrow()
                            .as_ref()
                            .expect("promise transition must retain its proposed assignment"),
                        visitor,
                    );
                },
                || {
                    let assignment = proposed
                        .borrow_mut()
                        .take()
                        .expect("promise transition must consume its proposed assignment once");
                    self.cell.assignment.set(assignment)?;
                    self.cell.terminal.store(true, Ordering::Release);
                    Ok(after_assignment(self.cell.assignment.get().expect(
                        "managed promise publication must initialize its assignment",
                    )))
                },
            )
        }
    }

    #[cfg(test)]
    pub(crate) fn exact_subscription_count(&self) -> usize {
        self.cell.completion.len()
    }
}

impl<'access, 'scope> ManagedCoreNetAccess<'access, 'scope> {
    fn from_authorized_cell(
        owner: ManagedCoreNetEdge,
        cell: &'access ManagedCoreNetCell,
        authority: &'access RuntimeValueAccess<'scope>,
    ) -> Self {
        Self {
            owner,
            cell,
            authority,
            _thread_bound: PhantomData,
        }
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
        self.cell.runtime.with_mut_via(self, update)
    }

    pub(crate) fn cell(&self) -> &RuntimeNetCell<CoreSpecialization> {
        &self.cell.runtime
    }
}

impl RuntimeNetMutationGateway<CoreSpecialization> for ManagedCoreNetAccess<'_, '_> {
    #[inline(always)]
    fn transition_edges<Result>(
        &self,
        runtime: &mut RuntimeNet<CoreSpecialization>,
        edges: RuntimeNetEdgeTransition,
        update: impl FnOnce(&mut RuntimeNet<CoreSpecialization>) -> Result,
    ) -> Result {
        // SAFETY: this access proves the managed owner is live in the exact
        // value region. Each visitor resolves only the exact representation
        // owners selected for its side of this update. The caller already
        // holds the sole runtime-net mutation mutex, and these visitors
        // operate on the borrowed state rather than reacquiring it.
        unsafe {
            self.authority.with_managed_edge_state_transition(
                &self.owner.0,
                runtime,
                |runtime, visitor| {
                    edges.visit_leaving(runtime, &mut |payload| {
                        trace_core_runtime_payload(payload, visitor);
                    });
                },
                |runtime, visitor| {
                    edges.visit_adding(runtime, &mut |payload| {
                        trace_core_runtime_payload(payload, visitor);
                    });
                },
                update,
            )
        }
    }
}

fn trace_lazy_result(result: &LazyResult, visitor: &mut Visitor<'_>) {
    match result {
        Ok(value) => visit_compatibility_payload_managed_edges(value, visitor),
        Err(failure) => visit_compatibility_payload_managed_edges(failure.as_ref(), visitor),
    }
}

fn trace_lazy_source(source: &LazySource, visitor: &mut Visitor<'_>) {
    visit_compatibility_payload_managed_edges(source, visitor);
    trace_lazy_source_managed_net_edges(source, visitor);
}

fn trace_lazy_source_cell(cell: &ManagedLazyCell, visitor: &mut Visitor<'_>) {
    let source = cell
        .source
        .lock()
        .expect("managed lazy source cell was poisoned");
    if let Some(source) = source.as_ref() {
        trace_lazy_source(source, visitor);
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
        trace_lazy_source(&source, visitor);
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

fn trace_core_runtime_payload(
    payload: RuntimeNetPayload<'_, CoreSpecialization>,
    visitor: &mut Visitor<'_>,
) {
    match payload {
        RuntimeNetPayload::Data(value) => {
            visit_compatibility_managed_edges(value, visitor);
        }
        RuntimeNetPayload::Operator(operator) => {
            visit_compatibility_payload_managed_edges(operator, visitor);
            trace_core_operator_managed_net_edges(operator, visitor);
        }
        RuntimeNetPayload::Source(source) => source.trace_managed_edge(visitor),
        RuntimeNetPayload::StuckReason(reason) => {
            visit_halt_value_edges(reason, &mut |value| {
                visit_compatibility_managed_edges(value, visitor);
            });
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
        self.runtime.try_visit_logical_payloads(&mut |payload| {
            trace_core_runtime_payload(payload, visitor);
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
    assert!(std::mem::size_of::<ManagedLazyCell>() == 144);
    assert!(std::mem::align_of::<ManagedLazyCell>() == 8);
    assert!(std::mem::size_of::<ManagedPromiseCell>() == 104);
    assert!(std::mem::align_of::<ManagedPromiseCell>() == 8);
    assert!(std::mem::size_of::<ManagedCoreNetCell>() == 248);
    assert!(std::mem::align_of::<ManagedCoreNetCell>() == 8);
    assert!(std::mem::size_of::<ManagedLazyRoot>() == 32);
    assert!(std::mem::align_of::<ManagedLazyRoot>() == 8);
    assert!(std::mem::size_of::<ManagedPromiseRoot>() == 40);
    assert!(std::mem::align_of::<ManagedPromiseRoot>() == 8);
    assert!(std::mem::size_of::<ManagedCoreNetRoot>() == 8);
    assert!(std::mem::align_of::<ManagedCoreNetRoot>() == 8);
};

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::core::{
        Builtin, BuiltinCall, CoreValueFactory, Dict, EvaluatedValue, EvaluationHalt, Key,
        LazyApplication, LazySource, LazyValue, List, ListThunk, NetValue, PromisedValue,
    };

    use crate::interaction_net::{NetBuilder, PreparedCopySource};
    use crate::runtime::{RuntimeIds, RuntimeMutationAdmission, allocate_evaluation_runtime_id};
    use syn::visit::{self, Visit};

    use glam_gc::EdgeTransitionObservation;

    fn new_values() -> CoreValueFactory {
        CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
    }

    fn runtime_with_data(value: Value) -> RuntimeNet<CoreSpecialization> {
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let exposed = builder.data(value);
        builder.finish(exposed).instantiate()
    }

    fn prepared_runtime(value: i64) -> RuntimeNet<CoreSpecialization> {
        runtime_with_data(Value::Number(value.into()))
    }

    fn replace_lazy_source_for_cycle<Edge: Trace>(
        access: &RuntimeValueAccess<'_>,
        owner: &ManagedLazyEdge,
        target: &Gc<Edge>,
        source: LazySource,
    ) {
        // SAFETY: both edges were allocated in this access region's exact
        // heap. The fixture replaces the edge-free placeholder with precisely
        // the one managed target reported to the production collector gateway.
        unsafe {
            let cell = access.scope.get_traced_edge(&owner.0);
            let mut stored = cell
                .source
                .lock()
                .expect("managed lazy source cell should not be poisoned");
            assert!(matches!(stored.as_ref(), Some(LazySource::Error)));
            access
                .scope
                .mutator
                .with_edge_replacement(&owner.0, None, Some(target), || {
                    *stored = Some(source);
                });
        }
    }

    fn assert_single_promise_cycle_through(label: &str, wrap: impl FnOnce(Value) -> Value) {
        let values = new_values();
        let baseline = values.collect_managed_for_test().unwrap_or_else(|failure| {
            panic!("the {label} fixture should start collectible: {failure}")
        });
        let root = values.with_runtime_value_access(|access| {
            let root = access
                .construct_rooted_managed_promise(label)
                .expect("the managed promise cell should fit a run");
            let promise = PromisedValue::from_root(&root, &access);
            root.access(&access)
                .expect("the rooted promise should be accessible")
                .publish(Ok(wrap(Value::Promised(promise))))
                .expect("the fresh promise should accept its compatibility cycle");
            root
        });

        let live = values.collect_managed_for_test().unwrap_or_else(|failure| {
            panic!("one root should retain the {label} cycle: {failure}")
        });
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 1);

        drop(root);
        let dead = values.collect_managed_for_test().unwrap_or_else(|failure| {
            panic!("the unrooted {label} cycle should reclaim: {failure}")
        });
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 1);
    }

    fn assert_single_promise_failure_cycle_through(
        label: &str,
        wrap: impl FnOnce(Value) -> EvaluationFailure,
    ) {
        let values = new_values();
        let baseline = values.collect_managed_for_test().unwrap_or_else(|failure| {
            panic!("the {label} fixture should start collectible: {failure}")
        });
        let root = values.with_runtime_value_access(|access| {
            let root = access
                .construct_rooted_managed_promise(label)
                .expect("the managed promise cell should fit a run");
            let promise = PromisedValue::from_root(&root, &access);
            root.access(&access)
                .expect("the rooted promise should be accessible")
                .publish(Err(Arc::new(wrap(Value::Promised(promise)))))
                .expect("the fresh promise should accept its failure cycle");
            root
        });

        let live = values.collect_managed_for_test().unwrap_or_else(|failure| {
            panic!("one root should retain the {label} cycle: {failure}")
        });
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 1);

        drop(root);
        let dead = values.collect_managed_for_test().unwrap_or_else(|failure| {
            panic!("the unrooted {label} cycle should reclaim: {failure}")
        });
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 1);
    }

    fn assert_lazy_promise_cycle_through_source(
        label: &str,
        source_for: impl FnOnce(Value) -> LazySource,
    ) {
        let values = new_values();
        let baseline = values.collect_managed_for_test().unwrap_or_else(|failure| {
            panic!("the {label} fixture should start collectible: {failure}")
        });
        let root = values.with_runtime_value_access(|access| {
            let lazy_edge = access
                .allocate_managed_lazy(label, LazySource::Error)
                .expect("the managed lazy cell should fit a run");
            let promise_edge = access
                .allocate_managed_promise(label)
                .expect("the managed promise cell should fit a run");
            let lazy_root = access.root_managed_lazy(&lazy_edge);
            let promise_root = access.root_managed_promise(&promise_edge);
            let lazy = LazyValue::from_root(&lazy_root, &access);
            let promise = PromisedValue::from_root(&promise_root, &access);

            replace_lazy_source_for_cycle(
                &access,
                &lazy_edge,
                &promise_edge.0,
                source_for(Value::Promised(promise)),
            );
            promise_root
                .access(&access)
                .expect("the rooted promise should be accessible")
                .publish(Ok(Value::Lazy(lazy)))
                .expect("the fresh promise should accept its lazy assignment");
            drop(promise_root);
            lazy_root
        });

        let live = values.collect_managed_for_test().unwrap_or_else(|failure| {
            panic!("one root should retain the {label} cycle: {failure}")
        });
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 2);

        drop(root);
        let dead = values.collect_managed_for_test().unwrap_or_else(|failure| {
            panic!("the unrooted {label} cycle should reclaim: {failure}")
        });
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 2);
    }

    fn source_declaration<'source>(source: &'source str, name: &str) -> &'source str {
        let start = source
            .find(name)
            .unwrap_or_else(|| panic!("missing declaration {name}"));
        let tail = &source[start..];
        let end = tail
            .find("\n}")
            .unwrap_or_else(|| panic!("unterminated declaration {name}"));
        &tail[..end]
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    struct ConstructorCallInventory {
        access_entries: BTreeSet<String>,
        owner_neutral: BTreeSet<String>,
        rooted: BTreeSet<String>,
        nursery: BTreeSet<String>,
        publications: BTreeSet<String>,
    }

    impl<'ast> Visit<'ast> for ConstructorCallInventory {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = call.func.as_ref()
                && let Some(segment) = path.path.segments.last()
            {
                self.record_call(&segment.ident.to_string());
            }
            visit::visit_expr_call(self, call);
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            self.record_call(&call.method.to_string());
            visit::visit_expr_method_call(self, call);
        }
    }

    impl ConstructorCallInventory {
        fn record_call(&mut self, name: &str) {
            if is_access_entry(name) {
                self.access_entries.insert(name.to_owned());
            }
            if is_owner_neutral_constructor(name) {
                self.owner_neutral.insert(name.to_owned());
            }
            if is_rooted_constructor(name) {
                self.rooted.insert(name.to_owned());
            }
            if is_evaluator_nursery(name) {
                self.nursery.insert(name.to_owned());
            }
            if is_publication_operation(name) {
                self.publications.insert(name.to_owned());
            }
        }

        fn has_constructor(&self) -> bool {
            !self.owner_neutral.is_empty() || !self.rooted.is_empty()
        }

        fn summary(&self, declaration: &str) -> String {
            let joined =
                |items: &BTreeSet<String>| items.iter().cloned().collect::<Vec<_>>().join(",");
            format!(
                "{declaration}|access={}|owner_neutral={}|rooted={}|nursery={}|publication={}",
                joined(&self.access_entries),
                joined(&self.owner_neutral),
                joined(&self.rooted),
                joined(&self.nursery),
                joined(&self.publications),
            )
        }
    }

    fn is_access_entry(name: &str) -> bool {
        matches!(
            name,
            "with_runtime_value_access"
                | "with_access"
                | "with_value_access"
                | "construct_runtime_value_root"
                | "try_construct_runtime_value_root"
        )
    }

    fn is_owner_neutral_constructor(name: &str) -> bool {
        matches!(
            name,
            "construct_managed_lazy"
                | "construct_failed_managed_lazy"
                | "construct_managed_promise"
                | "construct_managed_core_net"
                | "computed_fixpoint_in"
                | "semantic_thunk_in"
                | "semantic_computation_in"
                | "external_host_call_in"
                | "error_in"
                | "failure_in"
                | "from_access_in"
                | "from_application_in"
                | "from_builtin_in"
                | "from_net_construction_in"
                | "from_function_call_in"
                | "from_net_computation_in"
                | "from_reflection_gate_in"
                | "reflection_task_result_in"
                | "builtin_call_in"
        )
    }

    fn is_rooted_constructor(name: &str) -> bool {
        matches!(
            name,
            "construct_rooted_managed_lazy"
                | "construct_rooted_managed_promise"
                | "construct_rooted_managed_core_net"
        )
    }

    fn is_evaluator_nursery(name: &str) -> bool {
        matches!(
            name,
            "construct_lazy" | "construct_lazy_value" | "construct_promise" | "construct_core_net"
        )
    }

    fn is_publication_operation(name: &str) -> bool {
        matches!(
            name,
            "construct_runtime_value_root"
                | "try_construct_runtime_value_root"
                | "root_runtime_value"
                | "root_in"
                | "root_managed_lazy"
                | "root_managed_promise"
                | "root_managed_core_net"
                | "root_value"
                | "wrap"
        )
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum RegionalConstructionDisposition {
        InRegionForwarder,
        ContainingRoot,
        EvaluatorNursery,
        ExplicitFamilyRoot,
    }

    struct ReviewedConstructorSite {
        declaration: &'static str,
        disposition: RegionalConstructionDisposition,
        reason: &'static str,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct RegionalConstructorSite {
        declaration: String,
        calls: ConstructorCallInventory,
    }

    struct RegionalConstructorVisitor<'path> {
        path: &'path Path,
        modules: Vec<String>,
        owner: Option<String>,
        sites: Vec<RegionalConstructorSite>,
    }

    fn is_test_only(attributes: &[syn::Attribute]) -> bool {
        attributes.iter().any(|attribute| {
            attribute.path().is_ident("test")
                || (attribute.path().is_ident("cfg")
                    && matches!(
                        &attribute.meta,
                        syn::Meta::List(list) if list.tokens.to_string() == "test"
                    ))
        })
    }

    fn simple_type_name(ty: &syn::Type) -> String {
        match ty {
            syn::Type::Path(path) => path
                .path
                .segments
                .last()
                .map_or_else(|| "<impl>".to_owned(), |segment| segment.ident.to_string()),
            _ => "<impl>".to_owned(),
        }
    }

    impl RegionalConstructorVisitor<'_> {
        fn declaration(&self, function: &syn::Ident) -> String {
            let mut parts = vec![self.path.display().to_string()];
            parts.extend(self.modules.iter().cloned());
            if let Some(owner) = &self.owner {
                parts.push(owner.clone());
            }
            parts.push(function.to_string());
            parts.join("::")
        }

        fn record(&mut self, function: &syn::Ident, block: &syn::Block) {
            let mut calls = ConstructorCallInventory::default();
            calls.visit_block(block);
            if calls.has_constructor() {
                let declaration = self.declaration(function);
                self.sites
                    .push(RegionalConstructorSite { declaration, calls });
            }
        }
    }

    impl<'ast> Visit<'ast> for RegionalConstructorVisitor<'_> {
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            if is_test_only(&item.attrs) {
                return;
            }
            self.modules.push(item.ident.to_string());
            visit::visit_item_mod(self, item);
            self.modules.pop();
        }

        fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
            if is_test_only(&item.attrs) {
                return;
            }
            let prior = self.owner.replace(simple_type_name(&item.self_ty));
            visit::visit_item_impl(self, item);
            self.owner = prior;
        }

        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            if is_test_only(&item.attrs) {
                return;
            }
            self.record(&item.sig.ident, &item.block);
            visit::visit_impl_item_fn(self, item);
        }

        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            if is_test_only(&item.attrs) {
                return;
            }
            self.record(&item.sig.ident, &item.block);
            visit::visit_item_fn(self, item);
        }
    }

    fn regional_constructor_sites(path: &Path, source: &str) -> Vec<RegionalConstructorSite> {
        let syntax = syn::parse_file(source)
            .unwrap_or_else(|failure| panic!("failed to parse {}: {failure}", path.display()));
        let mut visitor = RegionalConstructorVisitor {
            path,
            modules: Vec::new(),
            owner: None,
            sites: Vec::new(),
        };
        visitor.visit_file(&syntax);
        visitor
            .sites
            .sort_by(|left, right| left.declaration.cmp(&right.declaration));
        visitor.sites
    }

    fn reviewed_constructor_sites() -> Vec<ReviewedConstructorSite> {
        let groups: &[(
            RegionalConstructionDisposition,
            &'static str,
            &[&'static str],
        )] = &[
            (
                RegionalConstructionDisposition::InRegionForwarder,
                "borrows the caller's active RuntimeValueAccess; its caller establishes the containing owner",
                &[
                    "src/compiler.rs::CompileContext::import_binary_in",
                    "src/compiler.rs::CompileContext::import_module_in",
                    "src/compiler.rs::invalid_import_request_in",
                    "src/core.rs::LazyValue::error_in",
                    "src/core.rs::LazyValue::failure_in",
                    "src/core.rs::LazyValue::with_source_in",
                    "src/core.rs::Value::builtin_call_in",
                    "src/eval/operator.rs::constant_effect_in",
                    "src/g_syntax/net_lowering.rs::ResolvedNetLowerer::compile_lazy_into",
                    "src/g_syntax/net_lowering.rs::ResolvedNetLowerer::lower_code_in",
                    "src/g_syntax/net_lowering.rs::lower_resolved_expr_in",
                    "src/reflection/machine.rs::request_function_in",
                    "src/reflection/store.rs::apply_value_at_path_in",
                    "src/reflection/store.rs::lazy_core_value_path",
                ],
            ),
            (
                RegionalConstructionDisposition::ContainingRoot,
                "publishes the completed same-region graph through its containing runtime value root",
                &[
                    "src/api/assembly.rs::Assembler::net",
                    "src/api/value.rs::ScopedValues::access",
                    "src/api/value.rs::ScopedValues::anno",
                    "src/api/value.rs::ScopedValues::apply",
                    "src/api/value.rs::Values::after_reflection",
                    "src/api/value.rs::Values::dict_singleton",
                    "src/api/value.rs::Values::dict_union",
                    "src/api/value.rs::Values::dict_update",
                    "src/api/value.rs::Values::empty_object",
                    "src/api/value.rs::Values::list_slice",
                    "src/compiler.rs::CompileContext::new",
                    "src/evaluation/session.rs::EvalContext::compose_builtin",
                    "src/g_syntax/compiler_values.rs::run_pure_match_resolved",
                    "src/reflection/machine.rs::EffectTask::interpret_prepared_drive",
                    "src/reflection/machine.rs::lazy_value_path_root",
                    "src/reflection/store.rs::apply_edit",
                ],
            ),
            (
                RegionalConstructionDisposition::EvaluatorNursery,
                "the evaluator step acquires an exact family root before the small access region ends",
                &[
                    "src/eval/application.rs::apply_function_values_in",
                    "src/eval/builtins/annotation/implementation.rs::annotation_error_value",
                    "src/eval/builtins/annotation/implementation.rs::defer_metadata_reflection",
                    "src/eval/builtins/annotation/implementation.rs::defer_reflection_annotation",
                    "src/eval/builtins/annotation/implementation.rs::eval_metadata_pure_annotation",
                    "src/eval/builtins/annotation/implementation.rs::eval_metadata_reflection_annotation",
                    "src/eval/builtins/annotation/implementation.rs::metadata_update_outputs",
                    "src/eval/builtins/dict/merge.rs::builtin_apply3_value",
                    "src/eval/builtins/dict/merge.rs::merge_duplicate_dict_value",
                    "src/eval/builtins/dict/merge.rs::update_nested_dict_path",
                    "src/eval/builtins/effect/implementation.rs::eval_fixpoint_builtin",
                    "src/eval/builtins/list_effect/implementation.rs::deferred_list",
                    "src/eval/builtins/net.rs::apply_net_arity",
                    "src/eval/builtins/net.rs::apply",
                    "src/eval/builtins/object/implementation.rs::eval_object_instance_builtin",
                    "src/eval/operator.rs::apply_builtin_values_lazily",
                    "src/eval/operator.rs::apply_core_operator",
                    "src/evaluation/access.rs::EvaluatorStepContext::construct_core_net",
                    "src/evaluation/access.rs::EvaluatorStepContext::construct_promise",
                    "src/reflection/machine.rs::EffectTask::deliver_step",
                    "src/reflection/machine.rs::replace_reset_frames",
                    "src/reflection/machine.rs::set_state_path_in",
                ],
            ),
            (
                RegionalConstructionDisposition::ExplicitFamilyRoot,
                "carries an already-intended promise root across its public or coordinator handoff",
                &[
                    "src/api/assembly.rs::Assembler::promise",
                    "src/api/assembly.rs::ReflectionEnvironmentBuilder::promise",
                    "src/core.rs::PromisedValue::fixpoint",
                ],
            ),
        ];

        groups
            .iter()
            .flat_map(|(disposition, reason, declarations)| {
                declarations
                    .iter()
                    .map(|declaration| ReviewedConstructorSite {
                        declaration,
                        disposition: *disposition,
                        reason,
                    })
            })
            .collect()
    }

    fn disposition_matches(
        site: &RegionalConstructorSite,
        disposition: RegionalConstructionDisposition,
    ) -> bool {
        match disposition {
            RegionalConstructionDisposition::InRegionForwarder => {
                site.calls.access_entries.is_empty()
                    && site.calls.rooted.is_empty()
                    && site.calls.nursery.is_empty()
                    && site.calls.publications.is_empty()
            }
            RegionalConstructionDisposition::ContainingRoot => {
                !site.calls.publications.is_empty() && site.calls.nursery.is_empty()
            }
            RegionalConstructionDisposition::EvaluatorNursery => {
                !site.calls.nursery.is_empty()
                    || (site.declaration.starts_with("src/evaluation/access.rs::")
                        && !site.calls.rooted.is_empty())
            }
            RegionalConstructionDisposition::ExplicitFamilyRoot => {
                !site.calls.rooted.is_empty() && site.calls.nursery.is_empty()
            }
        }
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
        assert_eq!(std::mem::size_of::<ManagedLazyCell>(), 144);
        assert_eq!(std::mem::align_of::<ManagedLazyCell>(), 8);
        assert_eq!(
            <ManagedLazyCell as Trace>::REQUESTED_SLOT_SIZE,
            Some(crate::core::managed::managed_slot_extent::<ManagedLazyCell>())
        );
        assert_eq!(std::mem::size_of::<ManagedPromiseCell>(), 104);
        assert_eq!(std::mem::align_of::<ManagedPromiseCell>(), 8);
        assert_eq!(
            <ManagedPromiseCell as Trace>::REQUESTED_SLOT_SIZE,
            Some(crate::core::managed::managed_slot_extent::<
                ManagedPromiseCell,
            >())
        );
        assert_eq!(std::mem::size_of::<ManagedCoreNetCell>(), 248);
        assert_eq!(std::mem::align_of::<ManagedCoreNetCell>(), 8);
        assert_eq!(
            <ManagedCoreNetCell as Trace>::REQUESTED_SLOT_SIZE,
            Some(crate::core::managed::managed_slot_extent::<
                ManagedCoreNetCell,
            >())
        );
        assert_eq!(std::mem::size_of::<ManagedLazyRoot>(), 32);
        assert_eq!(std::mem::size_of::<ManagedPromiseRoot>(), 40);
        assert_eq!(std::mem::size_of::<ManagedCoreNetRoot>(), 8);
    }

    #[test]
    fn fresh_managed_facades_survive_until_first_publication() {
        let values = new_values();
        values
            .collect_managed_for_test()
            .expect("the publication fixture should start collectible");

        let root = values.construct_runtime_value_root(|access| {
            let lazy = LazyValue::error_in(access, "published lazy");
            let promise = access
                .construct_managed_promise("published promise")
                .expect("managed promise representation must fit one collector run");
            let mut builder = NetBuilder::<CoreSpecialization>::new();
            let exposed = builder.data(Value::Number(67.into()));
            let net = access
                .construct_managed_core_net(builder.finish(exposed).instantiate())
                .expect("managed core-net representation must fit one collector run");
            assert!(matches!(
                values.collect_managed_for_test(),
                Err(glam_gc::CollectionError::ActiveMutator)
            ));
            Value::List(List::from_values(vec![
                Value::Lazy(lazy),
                Value::Promised(promise),
                Value::Net(NetValue::new(net)),
            ]))
        });

        let intervening = values
            .collect_managed_for_test()
            .expect("the containing root should preserve every fresh family");
        assert_eq!(
            intervening.finalized_slots(),
            0,
            "fresh managed allocations must survive their first permanent publication"
        );

        drop(root);
        let retired = values
            .collect_managed_for_test()
            .expect("dropping the containing root should retire the published graph");
        assert!(retired.finalized_slots() >= 3);
    }

    #[test]
    fn bounded_lazy_gateway_preserves_terminal_publication_protocol() {
        let values = new_values();
        let root = values.with_runtime_value_access(|access| {
            access
                .construct_rooted_managed_lazy("prepared lazy", LazySource::Error)
                .expect("the managed lazy cell should fit a run")
        });
        let winner = EvaluatedValue::try_from(Value::Number(42.into())).unwrap();
        let loser = EvaluatedValue::try_from(Value::Number(73.into())).unwrap();

        values.with_runtime_value_access(|access| {
            let lazy = root
                .access(&access)
                .expect("the matching value domain should authorize its lazy cell");
            assert_eq!(lazy.id(), root.id());
            assert_eq!(lazy.label().as_ref(), "prepared lazy");
            assert!(lazy.source_snapshot().is_some());
            assert_eq!(root.cache(&access, Ok(winner.clone())), Ok(winner.clone()));
            assert_eq!(root.cache(&access, Ok(loser)), Ok(winner.clone()));
            assert_eq!(lazy.cached(), Some(Ok(winner)));
            assert!(lazy.source_snapshot().is_none());
        });

        let unrelated = new_values();
        unrelated.with_runtime_value_access(|access| {
            assert!(root.access(&access).is_none());
        });
    }

    #[test]
    fn bounded_promise_gateway_preserves_one_terminal_winner() {
        let values = new_values();
        let root = values.with_runtime_value_access(|access| {
            access
                .construct_rooted_managed_promise("prepared promise")
                .expect("the managed promise cell should fit a run")
        });
        let winner = Value::Number(11.into());
        let loser = Value::Number(12.into());

        assert!(!root.is_terminal());
        values.with_runtime_value_access(|access| {
            let promise = root
                .access(&access)
                .expect("the matching value domain should authorize its promise cell");
            assert_eq!(promise.id(), root.id());
            assert_eq!(promise.runtime_id(), values.runtime_id());
            assert!(promise.producer().is_none());
            assert_eq!(
                root.publish_detached(&access, Ok(winner.clone()), |_| ()),
                Ok(())
            );
            assert_eq!(
                root.publish_detached(&access, Ok(loser.clone()), |_| ()),
                Err(Ok(loser))
            );
            assert_eq!(promise.assignment(), Some(Ok(winner)));
        });
        assert!(root.is_terminal());

        let unrelated = new_values();
        unrelated.with_runtime_value_access(|access| {
            assert!(root.access(&access).is_none());
        });
    }

    #[test]
    fn lazy_success_transition_reports_source_removal_and_terminal_addition_without_forcing() {
        let values = new_values();
        let forced = Arc::new(AtomicBool::new(false));
        let forced_by_thunk = Arc::clone(&forced);
        let sentinel = LazyValue::semantic_thunk(&values, "transition sentinel", move |_| {
            forced_by_thunk.store(true, Ordering::Release);
            panic!("edge transition visitation must not evaluate a lazy value")
        });
        let probe = values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);

        values.with_runtime_value_access(|access| {
            let root = access
                .construct_rooted_managed_lazy(
                    "transitioning lazy",
                    LazySource::NetConstruction(Value::Lazy(sentinel.clone()).into()),
                )
                .expect("the managed lazy cell should fit a run");
            let result =
                EvaluatedValue::try_from(Value::List(List::from_values(vec![Value::Lazy(
                    sentinel.clone(),
                )])))
                .expect("a list is already in weak-head normal form");
            assert_eq!(root.cache(&access, Ok(result.clone())), Ok(result));
            assert!(root.access(&access).unwrap().source_snapshot().is_none());
        });

        assert!(!forced.load(Ordering::Acquire));
        let records = probe.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].leaving_edges(), 1);
        assert_eq!(records[0].adding_edges(), 1);
    }

    #[test]
    fn lazy_failure_transition_reports_failure_edges_and_releases_source() {
        let values = new_values();
        let probe = values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);

        values.with_runtime_value_access(|access| {
            let source = access
                .construct_managed_promise("lazy failure source")
                .expect("the source promise should fit a run");
            let emitted = access
                .construct_managed_promise("lazy failure emission")
                .expect("the emission promise should fit a run");
            let root = access
                .construct_rooted_managed_lazy(
                    "failing lazy",
                    LazySource::NetConstruction(Value::Promised(source).into()),
                )
                .expect("the managed lazy cell should fit a run");
            let failure = Arc::new(EvaluationFailure::emission(Value::List(List::from_values(
                vec![Value::Promised(emitted)],
            ))));
            assert_eq!(root.cache(&access, Err(Arc::clone(&failure))), Err(failure));
            assert!(root.access(&access).unwrap().source_snapshot().is_none());
        });

        let records = probe.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].leaving_edges(), 1);
        assert_eq!(records[0].adding_edges(), 1);
    }

    #[test]
    fn promise_publication_callbacks_observe_assignment_before_wake_detachment() {
        let values = new_values();
        let probe = values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);
        let admission = RuntimeMutationAdmission::new();
        let coordinator =
            EvaluationWorkCoordinator::new_for_test(values.clone(), admission.clone());

        values.with_runtime_value_access(|access| {
            let target_root = access
                .construct_rooted_managed_promise("publication target")
                .expect("the managed promise cell should fit a run");
            let target = Value::List(List::from_values(vec![Value::Promised(
                PromisedValue::from_root(&target_root, &access),
            )]));
            let root = access
                .construct_rooted_managed_promise("publication ordering")
                .expect("the managed promise cell should fit a run");
            let promise = root.access(&access).unwrap();
            let detached = promise
                .publish_detached(Ok(target.clone()), |assignment| assignment.clone())
                .expect("the first detached publication should win");
            assert_eq!(detached, Ok(target.clone()));

            let guarded_root = access
                .construct_rooted_managed_promise("guarded publication ordering")
                .expect("the managed promise cell should fit a run");
            let guarded = guarded_root.access(&access).unwrap();
            let mutation = admission.mutation_guard();
            let (observed, wake) = guarded
                .publish_guarded(
                    &coordinator,
                    &mutation,
                    Err(Arc::new(EvaluationFailure::emission(target.clone()))),
                    |assignment| assignment.clone(),
                )
                .expect("the first guarded publication should win");
            assert_eq!(
                observed,
                Err(Arc::new(EvaluationFailure::emission(target.clone())))
            );
            drop(mutation);
            wake.notify();
        });

        let records = probe.records();
        assert_eq!(records.len(), 2);
        for record in records {
            assert_eq!(record.leaving_edges(), 0);
            assert_eq!(record.adding_edges(), 1);
        }
    }

    #[test]
    fn losing_promise_publisher_reports_its_proposed_addition_without_changing_the_winner() {
        let values = new_values();
        let probe =
            values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Adding);
        let (
            promise_root,
            _first_target_root,
            _second_target_root,
            first_assignment,
            second_assignment,
        ) = values.with_runtime_value_access(|access| {
            let promise_root = access
                .construct_rooted_managed_promise("publication race")
                .expect("the managed promise cell should fit a run");
            let first_target_root = access
                .construct_rooted_managed_promise("first proposed target")
                .expect("the managed promise cell should fit a run");
            let second_target_root = access
                .construct_rooted_managed_promise("second proposed target")
                .expect("the managed promise cell should fit a run");
            let first_assignment = Value::List(List::from_values(vec![Value::Promised(
                PromisedValue::from_root(&first_target_root, &access),
            )]));
            let second_assignment = Value::List(List::from_values(vec![Value::Promised(
                PromisedValue::from_root(&second_target_root, &access),
            )]));
            (
                promise_root,
                first_target_root,
                second_target_root,
                first_assignment,
                second_assignment,
            )
        });
        let (winner_published, winner_observed) = std::sync::mpsc::channel();

        let winner_values = values.clone();
        let winner_root = promise_root.clone();
        let winner_assignment_for_thread = first_assignment.clone();
        let winner = std::thread::spawn(move || {
            let published = winner_values.with_runtime_value_access(|access| {
                winner_root
                    .access(&access)
                    .expect("the winner promise should remain live")
                    .publish(Ok(winner_assignment_for_thread))
            });
            winner_published.send(()).unwrap();
            published
        });

        let loser_values = values.clone();
        let loser_root = promise_root.clone();
        let loser = std::thread::spawn(move || {
            winner_observed.recv().unwrap();
            loser_values.with_runtime_value_access(|access| {
                loser_root
                    .access(&access)
                    .expect("the loser promise should remain live")
                    .publish(Ok(second_assignment))
            })
        });

        assert_eq!(winner.join().expect("winner thread panicked"), Ok(()));
        assert!(loser.join().expect("loser thread panicked").is_err());
        values.with_runtime_value_access(|access| {
            assert_eq!(
                promise_root.access(&access).unwrap().assignment(),
                Some(Ok(first_assignment))
            );
        });
        let records = probe.records();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .all(|record| record.leaving_edges() == 0 && record.adding_edges() == 1)
        );
    }

    #[test]
    fn bounded_core_net_gateway_preserves_cell_mutation_publication() {
        let values = new_values();
        let root = values.with_runtime_value_access(|access| {
            access
                .construct_rooted_managed_core_net(prepared_runtime(5))
                .expect("the managed core net should fit one collector slot")
        });

        values.with_runtime_value_access(|access| {
            let net = root
                .access(&access)
                .expect("the matching value domain should authorize its core net");
            assert_eq!(
                net.with(|runtime| runtime.interface_data(runtime.exposed()).cloned()),
                Some(Value::Number(5.into()))
            );
            let before = net.cell().with_revisions(|_| ()).1;
            net.with_mut(|_| ());
            let after = net.cell().with_revisions(|_| ()).1;
            assert_eq!(after.topology_revision(), before.topology_revision() + 1);
        });

        let unrelated = new_values();
        unrelated.with_runtime_value_access(|access| {
            assert!(root.access(&access).is_none());
        });
    }

    #[test]
    fn core_net_reduction_enters_the_same_managed_transition_gateway() {
        let values = new_values();
        let (lazy_root, net_root) = values.with_runtime_value_access(|access| {
            let lazy_root = access
                .construct_rooted_managed_lazy("erased net payload", LazySource::Error)
                .expect("the managed lazy should fit one collector slot");
            let mut builder = NetBuilder::<CoreSpecialization>::new();
            let erase = builder.copy(0).input;
            let data = builder.data(Value::Lazy(LazyValue::from_root(&lazy_root, &access)));
            builder.wire(erase, data);
            let exposed = builder.data(Value::Number(97.into()));
            let net_root = access
                .construct_rooted_managed_core_net(builder.finish(exposed).instantiate())
                .expect("the managed core net should fit one collector slot");
            (lazy_root, net_root)
        });
        let probe = values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);

        values.with_runtime_value_access(|access| {
            let net = net_root
                .access(&access)
                .expect("the rooted core net should remain accessible");
            let pair = net
                .with(|runtime| runtime.active_pairs().next())
                .expect("the erase-data pair should be active");
            assert!(matches!(
                net.cell().step_active_pair_with_gateway(
                    pair,
                    None,
                    &net,
                    |_source, _anchor| unreachable!("erase does not inspect a source"),
                ),
                crate::interaction_net::ActivePairStep::Reduction(
                    crate::interaction_net::Reduction {
                        kind: crate::interaction_net::ReductionKind::Erase,
                        ..
                    }
                )
            ));
        });

        let records = probe.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].leaving_edges(), 1);
        assert_eq!(records[0].adding_edges(), 0);
        drop((lazy_root, net_root));
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
                    .construct_rooted_managed_lazy("rooted lazy", LazySource::Error)
                    .expect("the managed lazy cell should fit a run"),
                access
                    .construct_rooted_managed_promise("rooted promise")
                    .expect("the managed promise cell should fit a run"),
                access
                    .construct_rooted_managed_core_net(prepared_runtime(17))
                    .expect("the managed core-net cell should fit a run"),
            )
        });

        values.with_runtime_value_access(|access| {
            assert_eq!(
                lazy.access(&access).unwrap().label().as_ref(),
                "rooted lazy"
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
        let (lazy, promise, net) = values.with_runtime_value_access(|access| {
            let lazy_edge = access
                .allocate_managed_lazy("split lazy", LazySource::Error)
                .expect("the managed lazy cell should fit a run");
            let promise_edge = access
                .allocate_managed_promise("split promise")
                .expect("the managed promise cell should fit a run");
            let net_edge = access
                .allocate_managed_core_net(prepared_runtime(61))
                .expect("the managed core-net cell should fit a run");

            assert_eq!(lazy_edge.access(&access).label().as_ref(), "split lazy");
            assert_eq!(
                net_edge
                    .access(&access)
                    .with(|runtime| runtime.interface_data(runtime.exposed()).cloned()),
                Some(Value::Number(61.into()))
            );

            (
                access.root_managed_lazy(&lazy_edge),
                access.root_managed_promise(&promise_edge),
                access.root_managed_core_net(&net_edge),
            )
        });

        values.with_runtime_value_access(|access| {
            let lazy_alias = lazy.edge(&access);
            let promise_alias = promise.edge(&access);
            let net_alias = net.edge(&access);
            assert!(lazy_alias.same_allocation_in(&lazy.edge(&access), &access));
            assert!(promise_alias.same_allocation_in(&promise.edge(&access), &access));
            assert!(net_alias.same_allocation_in(&net.edge(&access), &access));
            assert_eq!(
                lazy.access(&access).unwrap().id(),
                lazy_alias.access(&access).id()
            );
            assert_eq!(
                promise.access(&access).unwrap().id(),
                promise_alias.access(&access).id()
            );
            assert_eq!(
                net.access(&access)
                    .unwrap()
                    .with(|runtime| runtime.exposed()),
                net_alias.access(&access).with(|runtime| runtime.exposed())
            );
        });

        let unrelated = new_values();
        unrelated.with_runtime_value_access(|access| {
            assert!(lazy.access(&access).is_none());
            assert!(promise.access(&access).is_none());
            assert!(net.access(&access).is_none());
        });
    }

    #[test]
    fn regional_value_publication_retains_only_the_returned_managed_graph() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the regional-construction fixture should start collectible");

        let root = values.construct_runtime_value_root(|access| {
            let _unreturned = access
                .construct_managed_lazy("unreturned regional lazy", LazySource::Error)
                .expect("the unreturned managed lazy should fit a run");

            let lazy = access
                .construct_managed_lazy("returned regional lazy", LazySource::Error)
                .expect("the returned managed lazy should fit a run");
            let promise = access
                .construct_managed_promise("returned regional promise")
                .expect("the returned managed promise should fit a run");
            let net = access
                .construct_managed_core_net(prepared_runtime(83))
                .expect("the returned managed core net should fit a run");
            Value::List(List::from_values(vec![
                Value::Lazy(lazy),
                Value::Promised(promise),
                Value::Net(NetValue::new(net)),
            ]))
        });

        let live = values
            .collect_managed_for_test()
            .expect("the containing value root should retain all returned identities");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 4);
        assert_eq!(
            live.finalized_slots(),
            1,
            "the allocation omitted from the returned graph should be reclaimed"
        );

        assert!(matches!(
            root.clone_core_in_own_domain(),
            Some(Value::List(list)) if list.len() == 3
        ));

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("dropping the containing root should release its graph");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 4);
    }

    #[test]
    fn failed_lazy_gateway_is_terminal_before_traced_handoff() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the failed-lazy fixture should start collectible");
        let failure = Arc::new(EvaluationFailure::message("prepared failure"));
        let root = values.construct_runtime_value_root(|access| {
            let lazy = access
                .construct_failed_managed_lazy("prepared failure", failure.clone())
                .expect("the managed failed lazy should fit a run");
            assert_eq!(lazy.access(access).cached(), Some(Err(failure.clone())));
            assert!(lazy.access(access).source_snapshot().is_none());
            Value::Lazy(lazy)
        });

        let live = values
            .collect_managed_for_test()
            .expect("the containing value root should retain the failed lazy");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 2);
        drop(root);
    }

    #[test]
    fn early_regional_return_leaves_partial_managed_graph_collectible() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the early-return fixture should start collectible");

        let outcome = values.try_construct_runtime_value_root(|access| {
            let _promise = access
                .construct_managed_promise("abandoned partial graph")
                .expect("the partial managed promise should fit a run");
            let _lazy = access
                .construct_managed_lazy("abandoned partial lazy", LazySource::Error)
                .expect("the partial managed lazy should fit a run");
            let _net = access
                .construct_managed_core_net(prepared_runtime(89))
                .expect("the partial managed core net should fit a run");
            Err::<Value, &'static str>("construction stopped")
        });
        assert_eq!(outcome, Err("construction stopped"));

        let collected = values
            .collect_managed_for_test()
            .expect("an unpublished partial graph should remain collectible");
        assert_eq!(collected.root_entries(), baseline.root_entries());
        assert_eq!(collected.marked_slots(), baseline.marked_slots());
        assert_eq!(collected.finalized_slots(), 3);
    }

    #[test]
    fn managed_lazy_source_self_cycle_is_traced_and_reclaimed() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the managed-lazy cycle fixture should start collectible");
        let root = values.with_runtime_value_access(|access| {
            let edge = access
                .allocate_managed_lazy("lazy self cycle", LazySource::Error)
                .expect("the managed lazy cell should fit a run");
            let root = access.root_managed_lazy(&edge);
            let lazy = LazyValue::from_root(&root, &access);
            let source = LazySource::ComputedFixpoint(Arc::new(
                crate::core::FixpointComputation::Function(Value::Lazy(lazy)),
            ));

            // SAFETY: `edge` is live in this access region's exact heap and
            // representation. The closure replaces the edge-free placeholder
            // with precisely the self edge reported to the collector gateway.
            unsafe {
                let cell = access.scope.get_traced_edge(&edge.0);
                let mut stored = cell
                    .source
                    .lock()
                    .expect("managed lazy source cell should not be poisoned");
                assert!(matches!(stored.as_ref(), Some(LazySource::Error)));
                access
                    .scope
                    .mutator
                    .with_edge_replacement(&edge.0, None, Some(&edge.0), || {
                        *stored = Some(source);
                    });
            }

            root
        });

        let live = values
            .collect_managed_for_test()
            .expect("a rooted managed-lazy self-cycle should survive");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 1);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("an unrooted managed-lazy self-cycle should be reclaimed");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 1);
    }

    #[test]
    fn managed_promise_assignment_self_cycle_is_traced_and_reclaimed() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the managed-promise cycle fixture should start collectible");
        let root = values.with_runtime_value_access(|access| {
            let root = access
                .construct_rooted_managed_promise("promise self cycle")
                .expect("the managed promise cell should fit a run");
            let promise = PromisedValue::from_root(&root, &access);
            root.access(&access)
                .expect("the rooted promise should be accessible")
                .publish(Ok(Value::Promised(promise)))
                .expect("the fresh promise should accept its self assignment");
            root
        });

        let live = values
            .collect_managed_for_test()
            .expect("a rooted managed-promise self-cycle should survive");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 1);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("an unrooted managed-promise self-cycle should be reclaimed");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 1);
    }

    #[test]
    fn managed_core_net_source_self_cycle_is_traced_and_reclaimed() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the managed-net cycle fixture should start collectible");
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
                let cell = access.scope.get_traced_edge(&edge);
                let remote = cell.runtime.with(RuntimeNet::exposed);
                access
                    .scope
                    .mutator
                    .with_edge_replacement(&edge, None, Some(&edge), || {
                        cell.runtime.with_mut(|runtime| {
                            runtime.begin_copy(PreparedCopySource::new(
                                crate::core_net::CoreRuntimeNet::from_managed_edge(
                                    managed_edge.duplicate_in(&access),
                                ),
                                remote,
                            ));
                        });
                    });
            }

            access.root_managed_core_net(&managed_edge)
        });

        let live = values
            .collect_managed_for_test()
            .expect("a rooted managed-net self-cycle should survive");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 1);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("an unrooted managed-net self-cycle should be reclaimed");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 1);
    }

    #[test]
    fn managed_lazy_promise_pair_cycle_is_traced_and_reclaimed() {
        assert_lazy_promise_cycle_through_source("function fixpoint compatibility", |backedge| {
            LazySource::ComputedFixpoint(Arc::new(crate::core::FixpointComputation::Function(
                backedge,
            )))
        });
    }

    #[test]
    fn managed_cycle_through_object_fixpoint_is_traced_and_reclaimed() {
        assert_lazy_promise_cycle_through_source("object fixpoint compatibility", |backedge| {
            LazySource::ComputedFixpoint(Arc::new(
                crate::core::FixpointComputation::ObjectInstance(backedge),
            ))
        });
    }

    #[test]
    fn managed_cycle_through_lazy_application_is_traced_and_reclaimed() {
        assert_lazy_promise_cycle_through_source("lazy application compatibility", |backedge| {
            LazySource::Application(Arc::new(LazyApplication {
                function: backedge,
                arguments: Arc::from([]),
            }))
        });
    }

    #[test]
    fn managed_cycle_through_net_construction_is_traced_and_reclaimed() {
        assert_lazy_promise_cycle_through_source("net construction compatibility", |backedge| {
            LazySource::NetConstruction(Arc::new(backedge))
        });
    }

    #[test]
    fn managed_lazy_core_net_pair_cycle_is_traced_and_reclaimed() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the lazy-net cycle fixture should start collectible");
        let root = values.with_runtime_value_access(|access| {
            let lazy_edge = access
                .allocate_managed_lazy("lazy to net", LazySource::Error)
                .expect("the managed lazy cell should fit a run");
            let lazy_root = access.root_managed_lazy(&lazy_edge);
            let lazy = LazyValue::from_root(&lazy_root, &access);
            let net_edge = access
                .allocate_managed_core_net(runtime_with_data(Value::Lazy(lazy)))
                .expect("the managed core-net cell should fit a run");
            let net_root = access.root_managed_core_net(&net_edge);
            let net =
                crate::core_net::CoreRuntimeNet::from_managed_edge(net_edge.duplicate_in(&access));

            replace_lazy_source_for_cycle(
                &access,
                &lazy_edge,
                &net_edge.0,
                LazySource::NetComputation(NetValue::new(net)),
            );
            drop(net_root);
            lazy_root
        });

        let live = values
            .collect_managed_for_test()
            .expect("one root should retain the lazy-net cycle");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 2);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("the unrooted lazy-net cycle should be reclaimed");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 2);
    }

    #[test]
    fn managed_promise_core_net_pair_cycle_is_traced_and_reclaimed() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the promise-net cycle fixture should start collectible");
        let root = values.with_runtime_value_access(|access| {
            let promise_edge = access
                .allocate_managed_promise("promise to net")
                .expect("the managed promise cell should fit a run");
            let promise_root = access.root_managed_promise(&promise_edge);
            let promise = PromisedValue::from_root(&promise_root, &access);
            let net_edge = access
                .allocate_managed_core_net(runtime_with_data(Value::Promised(promise)))
                .expect("the managed core-net cell should fit a run");
            let net_root = access.root_managed_core_net(&net_edge);
            let net =
                crate::core_net::CoreRuntimeNet::from_managed_edge(net_edge.duplicate_in(&access));

            promise_root
                .access(&access)
                .expect("the rooted promise should be accessible")
                .publish(Ok(Value::Net(NetValue::new(net))))
                .expect("the fresh promise should accept its net assignment");
            drop(net_root);
            promise_root
        });

        let live = values
            .collect_managed_for_test()
            .expect("one root should retain the promise-net cycle");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 2);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("the unrooted promise-net cycle should be reclaimed");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 2);
    }

    #[test]
    fn managed_lazy_net_promise_ring_is_traced_and_reclaimed() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the three-family cycle fixture should start collectible");
        let root = values.with_runtime_value_access(|access| {
            let lazy_edge = access
                .allocate_managed_lazy("lazy to net ring", LazySource::Error)
                .expect("the managed lazy cell should fit a run");
            let promise_edge = access
                .allocate_managed_promise("promise to lazy ring")
                .expect("the managed promise cell should fit a run");
            let lazy_root = access.root_managed_lazy(&lazy_edge);
            let promise_root = access.root_managed_promise(&promise_edge);
            let lazy = LazyValue::from_root(&lazy_root, &access);
            let promise = PromisedValue::from_root(&promise_root, &access);
            let net_edge = access
                .allocate_managed_core_net(runtime_with_data(Value::Promised(promise)))
                .expect("the managed core-net cell should fit a run");
            let net_root = access.root_managed_core_net(&net_edge);
            let net =
                crate::core_net::CoreRuntimeNet::from_managed_edge(net_edge.duplicate_in(&access));

            replace_lazy_source_for_cycle(
                &access,
                &lazy_edge,
                &net_edge.0,
                LazySource::NetComputation(NetValue::new(net)),
            );
            promise_root
                .access(&access)
                .expect("the rooted promise should be accessible")
                .publish(Ok(Value::Lazy(lazy)))
                .expect("the fresh promise should accept its lazy assignment");
            drop((promise_root, net_root));
            lazy_root
        });

        let live = values
            .collect_managed_for_test()
            .expect("one root should retain the three-family cycle");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 3);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("the unrooted three-family cycle should be reclaimed");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 3);
    }

    #[test]
    fn managed_promise_cycle_through_list_is_traced_and_reclaimed() {
        assert_single_promise_cycle_through("list compatibility", |backedge| {
            Value::List(List::from_values(vec![backedge]))
        });
    }

    #[test]
    fn managed_promise_cycle_through_list_thunk_is_traced_and_reclaimed() {
        assert_single_promise_cycle_through("list thunk compatibility", |backedge| {
            let Value::Promised(promise) = backedge else {
                unreachable!("the cycle helper always supplies its promise backedge")
            };
            Value::List(List::from_thunk(ListThunk::Promised(promise)))
        });
    }

    #[test]
    fn managed_promise_cycle_through_dict_is_traced_and_reclaimed() {
        assert_single_promise_cycle_through("dict compatibility", |backedge| {
            Value::Dict(Dict::new_sync().insert(Key::binary_from_text("backedge"), backedge))
        });
    }

    #[test]
    fn managed_promise_cycle_through_partial_builtin_is_traced_and_reclaimed() {
        assert_single_promise_cycle_through("partial builtin compatibility", |backedge| {
            Value::PartialBuiltin(BuiltinCall {
                builtin: Builtin::Append,
                arguments: Arc::from([backedge]),
            })
        });
    }

    #[test]
    fn managed_promise_cycle_through_metadata_is_traced_and_reclaimed() {
        assert_single_promise_cycle_through("metadata compatibility", Value::metadata_carrier);
    }

    #[test]
    fn managed_promise_cycle_through_shared_list_spine_is_traced_and_reclaimed() {
        assert_single_promise_cycle_through("shared list compatibility", |backedge| {
            let shared = List::from_values(vec![backedge]);
            Value::List(List::concat(shared.clone(), shared))
        });
    }

    #[test]
    fn managed_promise_cycle_through_shared_dict_version_is_traced_and_reclaimed() {
        assert_single_promise_cycle_through("shared dict compatibility", |backedge| {
            let base = Dict::new_sync().insert(Key::binary_from_text("backedge"), backedge);
            Value::Dict(base.insert(Key::binary_from_text("version"), Value::Number(1.into())))
        });
    }

    #[test]
    fn managed_promise_cycle_through_function_stage_is_traced_and_reclaimed() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the function-stage cycle fixture should start collectible");
        let root = values.with_runtime_value_access(|access| {
            let promise_edge = access
                .allocate_managed_promise("function stage compatibility")
                .expect("the managed promise cell should fit a run");
            let promise_root = access.root_managed_promise(&promise_edge);
            let promise = PromisedValue::from_root(&promise_root, &access);
            let net_edge = access
                .allocate_managed_core_net(runtime_with_data(Value::Promised(promise)))
                .expect("the managed core-net cell should fit a run");
            let net_root = access.root_managed_core_net(&net_edge);
            let stage = NetValue::new(crate::core_net::CoreRuntimeNet::from_managed_edge(net_edge));

            promise_root
                .access(&access)
                .expect("the rooted promise should be accessible")
                .publish(Ok(Value::Function(crate::core::FunctionValue::new(
                    stage, 1,
                ))))
                .expect("the fresh promise should accept its function-stage cycle");
            drop(net_root);
            promise_root
        });

        let live = values
            .collect_managed_for_test()
            .expect("one root should retain the function-stage cycle");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 2);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("the unrooted function-stage cycle should be reclaimed");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 2);
    }

    #[test]
    fn managed_promise_cycle_through_failure_emission_is_traced_and_reclaimed() {
        assert_single_promise_failure_cycle_through(
            "failure emission compatibility",
            EvaluationFailure::emission,
        );
    }

    #[test]
    fn managed_promise_cycle_through_failure_context_is_traced_and_reclaimed() {
        assert_single_promise_failure_cycle_through("failure context compatibility", |backedge| {
            EvaluationFailure::message("compatibility failure").with_context(backedge)
        });
    }

    #[test]
    fn managed_core_net_stuck_reason_self_cycle_is_traced_and_reclaimed() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the stuck-reason cycle fixture should start collectible");
        let root = values.with_runtime_value_access(|access| {
            let mut builder = NetBuilder::<CoreSpecialization>::new();
            let [function, argument, result] = builder.bind();
            let callable = builder.data(Value::Number(0.into()));
            let supplied = builder.data(Value::Number(1.into()));
            builder.wire(function, callable);
            builder.wire(argument, supplied);
            let mut runtime = builder.finish(result).instantiate();
            let reduction = runtime
                .reduce_next()
                .expect("the Bind/Data pair should become a claimed call");
            let crate::interaction_net::ReductionKind::Call { bind, data } = reduction.kind else {
                panic!("the stuck-reason fixture should claim a call")
            };
            let call = crate::interaction_net::Call {
                pair: reduction.pair,
                bind,
                data,
            };

            let edge = access
                .allocate_managed_core_net(runtime)
                .expect("the managed core-net cell should fit a run");
            let root = access.root_managed_core_net(&edge);
            let net = crate::core_net::CoreRuntimeNet::from_managed_edge(edge);
            net.access(&access).fail_claimed_call(
                call,
                EvaluationHalt::from_value(Value::Net(NetValue::new(net.clone()))),
            );
            root
        });

        let live = values
            .collect_managed_for_test()
            .expect("a root should retain the net through its own stuck reason");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 1);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("the unrooted stuck-reason self-cycle should be reclaimed");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 1);
    }

    #[test]
    fn managed_operator_payload_cycle_survives_ready_and_claimed_work_then_reclaims() {
        fn assert_state(claim: bool) {
            let state = if claim { "claimed" } else { "ready" };
            let values = new_values();
            let baseline = values.collect_managed_for_test().unwrap_or_else(|failure| {
                panic!("the {state} operator-work fixture should start collectible: {failure}")
            });
            let root = values.with_runtime_value_access(|access| {
                let promise_root = access
                    .construct_rooted_managed_promise(format!("{state} operator work"))
                    .expect("the managed promise cell should fit a run");
                let promise = PromisedValue::from_root(&promise_root, &access);

                let mut builder = NetBuilder::<CoreSpecialization>::new();
                let [input, result] = builder.operator(crate::core_net::CoreOperator::Applicable(
                    Value::Promised(promise),
                ));
                let argument = builder.data(Value::Number(2.into()));
                builder.wire(input, argument);
                let runtime = builder.finish(result).instantiate();
                let pair = runtime
                    .active_pairs()
                    .next()
                    .expect("the Operator/Data pair should begin ready");
                let net_edge = access
                    .allocate_managed_core_net(runtime)
                    .expect("the managed core-net cell should fit a run");
                let net_root = access.root_managed_core_net(&net_edge);
                let net = crate::core_net::CoreRuntimeNet::from_managed_edge(net_edge);

                promise_root
                    .access(&access)
                    .expect("the rooted promise should be accessible")
                    .publish(Ok(Value::Net(NetValue::new(net.clone()))))
                    .expect("the fresh promise should accept its operator-work cycle");
                if claim {
                    assert!(matches!(
                        net.access(&access).step_active_pair(pair),
                        crate::core_net::CoreActivePairStep::Reduction(
                            crate::interaction_net::Reduction {
                                kind: crate::interaction_net::ReductionKind::OperatorCall { .. },
                                ..
                            }
                        )
                    ));
                }
                drop(net_root);
                promise_root
            });

            let live = values.collect_managed_for_test().unwrap_or_else(|failure| {
                panic!("one root should retain the {state} operator-work cycle: {failure}")
            });
            assert_eq!(live.root_entries(), baseline.root_entries() + 1);
            assert_eq!(live.marked_slots(), baseline.marked_slots() + 2);

            drop(root);
            let dead = values.collect_managed_for_test().unwrap_or_else(|failure| {
                panic!("the unrooted {state} operator-work cycle should reclaim: {failure}")
            });
            assert_eq!(dead.root_entries(), baseline.root_entries());
            assert_eq!(dead.finalized_slots(), 2);
        }

        assert_state(false);
        assert_state(true);
    }

    #[test]
    fn managed_promise_cycle_through_remote_cursor_source_is_traced_and_reclaimed() {
        let values = new_values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the remote-cursor cycle fixture should start collectible");
        let root = values.with_runtime_value_access(|access| {
            let promise_edge = access
                .allocate_managed_promise("remote cursor compatibility")
                .expect("the managed promise cell should fit a run");
            let promise_root = access.root_managed_promise(&promise_edge);
            let promise = PromisedValue::from_root(&promise_root, &access);

            let source_runtime = runtime_with_data(Value::Promised(promise));
            let remote = source_runtime.exposed();
            let source_edge = access
                .allocate_managed_core_net(source_runtime)
                .expect("the source managed core-net cell should fit a run");
            let source_root = access.root_managed_core_net(&source_edge);
            let source = crate::core_net::CoreRuntimeNet::from_managed_edge(source_edge);

            let mut target_runtime = prepared_runtime(0);
            let cursor = target_runtime.begin_copy(PreparedCopySource::new(source, remote));
            assert_ne!(
                cursor,
                target_runtime.exposed().node(),
                "the copy source must be retained by a distinct remote cursor"
            );
            let target_edge = access
                .allocate_managed_core_net(target_runtime)
                .expect("the target managed core-net cell should fit a run");
            let target_root = access.root_managed_core_net(&target_edge);
            let target = crate::core_net::CoreRuntimeNet::from_managed_edge(target_edge);

            promise_root
                .access(&access)
                .expect("the rooted promise should be accessible")
                .publish(Ok(Value::Net(NetValue::new(target))))
                .expect("the fresh promise should accept its cursor cycle");
            drop((source_root, target_root));
            promise_root
        });

        let live = values
            .collect_managed_for_test()
            .expect("one root should retain the remote-cursor cycle");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 3);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("the unrooted remote-cursor cycle should be reclaimed");
        assert_eq!(dead.root_entries(), baseline.root_entries());
        assert_eq!(dead.finalized_slots(), 3);
    }

    #[test]
    fn recursive_cell_gateways_are_private_and_complete() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let owner_path = manifest.join("src/core/managed/recursive_cells.rs");
        let inventory_path = manifest.join("src/core/managed/recursive_identity_inventory.rs");
        let gate_inventory_path = manifest.join("src/core/managed/gate_g2_inventory.rs");
        // The I9 active-owner and I11A gate audits name the managed cells only
        // as test-only inventory data; neither may construct or expose them.
        let active_inventory_path = manifest.join("src/core/managed/active_owner_inventory.rs");
        let owner = fs::read_to_string(&owner_path).expect("the recursive-cell source should read");
        let count = |parts: &[&str]| owner.matches(&parts.concat()).count();

        assert_eq!(count(&["fn construct_rooted_", "managed_"]), 3);
        assert_eq!(count(&["fn construct_", "managed_"]), 3);
        assert_eq!(count(&["fn construct_failed_", "managed_lazy"]), 1);
        assert_eq!(count(&["fn allocate_", "managed_"]), 3);
        assert_eq!(count(&["pub(crate) fn allocate_", "managed_"]), 0);
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
                    || path == gate_inventory_path
                    || path == active_inventory_path
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
                    || [
                        "allocate_managed_lazy",
                        "allocate_managed_promise",
                        "allocate_managed_core_net",
                    ]
                    .iter()
                    .any(|name| source.contains(name))
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
    fn regional_constructor_inventory_detects_unreviewed_escape_shapes() {
        // Keep this synthetic access spelling out of the older raw-text access
        // inventory: that inventory counts production call syntax and cannot
        // distinguish Rust embedded in a test string. The parser below still
        // receives the exact spelling which this fixture is meant to detect.
        let access_entry = ["with_runtime_value", "_access"].concat();
        let source = format!(
            r#"
            fn bare(factory: &CoreValueFactory) -> LazyValue {{
                factory.{access_entry}(|access| {{
                    LazyValue::error_in(&access, "bare")
                }})
            }}

            fn containing(factory: &CoreValueFactory) -> Holder {{
                factory.{access_entry}(|access| Holder {{
                    value: LazyValue::error_in(&access, "contained"),
                }})
            }}
        "#
        );
        let sites = regional_constructor_sites(Path::new("synthetic_escape.rs"), &source);
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0].declaration, "synthetic_escape.rs::bare");
        assert_eq!(
            sites[0].calls.access_entries,
            BTreeSet::from(["with_runtime_value_access".to_owned()])
        );
        assert_eq!(
            sites[0].calls.owner_neutral,
            BTreeSet::from(["error_in".to_owned()])
        );
        assert_eq!(sites[1].declaration, "synthetic_escape.rs::containing");
        assert_eq!(
            sites[1].calls.access_entries,
            BTreeSet::from(["with_runtime_value_access".to_owned()])
        );
        assert_eq!(
            sites[1].calls.owner_neutral,
            BTreeSet::from(["error_in".to_owned()])
        );
    }

    #[test]
    fn regional_constructor_callers_are_exact_and_classified() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let owner_path = manifest.join("src/core/managed/recursive_cells.rs");
        let mut stack = vec![manifest.join("src")];
        let mut actual = Vec::new();
        while let Some(directory) = stack.pop() {
            for entry in fs::read_dir(directory).expect("the source tree should be readable") {
                let path = entry.expect("a source entry should be readable").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") || path == owner_path
                {
                    continue;
                }
                let source = fs::read_to_string(&path).expect("Rust source should be readable");
                let relative = path
                    .strip_prefix(manifest)
                    .expect("source should belong to the package");
                actual.extend(regional_constructor_sites(relative, &source));
            }
        }
        actual.sort_by(|left, right| left.declaration.cmp(&right.declaration));

        let mut reviewed = reviewed_constructor_sites();
        reviewed.sort_by_key(|site| site.declaration);
        for site in &reviewed {
            assert!(!site.reason.is_empty());
        }
        let expected = reviewed
            .iter()
            .map(|site| site.declaration)
            .collect::<Vec<_>>();
        let actual_declarations = actual
            .iter()
            .map(|site| site.declaration.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            actual_declarations, expected,
            "the production regional-constructor surface changed without an ownership classification"
        );
        for (actual, reviewed) in actual.iter().zip(&reviewed) {
            assert!(
                disposition_matches(actual, reviewed.disposition),
                "{} no longer matches its reviewed {:?} disposition: {}",
                actual.declaration,
                reviewed.disposition,
                actual.calls.summary(&actual.declaration),
            );
        }
    }

    #[test]
    fn recursive_identity_coordination_is_edge_free() {
        let source = include_str!("recursive_cells.rs");

        for cell in [
            "struct ManagedLazyCell",
            "struct ManagedPromiseCell",
            "struct ManagedCoreNetCell",
        ] {
            let body = source_declaration(source, cell);
            assert!(!body.contains("Root<"), "{cell} must not retain a root");
            assert!(
                !body.contains("RuntimeValueRoot"),
                "{cell} must not retain a compatibility root"
            );
        }
        assert!(
            source_declaration(source, "struct ManagedPromiseCell")
                .contains("producer: Arc<OnceLock<Arc<PromiseProducerObligation>>>")
        );
        assert!(
            !source_declaration(source, "struct ManagedPromiseCell")
                .contains("RuntimeValueObserver"),
            "the managed promise cell must not retain runtime re-entry authority"
        );

        let completion = include_str!("../../evaluation/coordinator/completion.rs");
        let runtime_net = include_str!("../../interaction_net/runtime.rs");
        for (source, companion) in [
            (completion, "pub(crate) struct CompletionSubscriptions"),
            (runtime_net, "pub(crate) struct RuntimeNetDisturbance"),
            (runtime_net, "struct RuntimeNetDisturbanceInner"),
            (runtime_net, "struct NormalizationBatchState"),
            (runtime_net, "struct ActiveNormalizationBatch"),
        ] {
            let body = source_declaration(source, companion);
            for forbidden in [
                "Gc<",
                "Root<",
                "RuntimeValueRoot",
                "EvaluationWaitToken",
                "ManagedLazy",
                "ManagedPromise",
                "ManagedCoreNet",
                "PromiseProducerObligation",
            ] {
                assert!(
                    !body.contains(forbidden),
                    "{companion} acquired a semantic edge: {forbidden}"
                );
            }
        }
    }

    #[test]
    fn semantic_recursive_facades_are_managed_edges_only() {
        let core = include_str!("../../core.rs");
        for (facade, edge) in [
            ("LazyValue", "ManagedLazyEdge"),
            ("PromisedValue", "ManagedPromiseEdge"),
        ] {
            let declaration = source_declaration(core, &format!("struct {facade}"));
            assert!(declaration.contains(edge));
            for forbidden in [
                "RuntimeValueObserver",
                "Root<",
                "Arc<",
                "Weak<",
                "id:",
                "label:",
            ] {
                assert!(
                    !declaration.contains(forbidden),
                    "{facade} regained access or copied metadata through {forbidden}"
                );
            }
        }

        let lazy = source_declaration(core, "impl LazyValue {");
        let promise = source_declaration(core, "impl PromisedValue {");
        for facade in [lazy, promise] {
            assert!(!facade.contains("fn with_runtime_access"));
            assert!(!facade.contains("fn with_access"));
        }

        let core_net = include_str!("../../core_net.rs");
        let facade = source_declaration(core_net, "struct CoreRuntimeNet");
        assert!(facade.contains("ManagedCoreNetEdge"));
        for forbidden in ["RuntimeValueObserver", "Root<", "Arc<", "Weak<", "values:"] {
            assert!(
                !facade.contains(forbidden),
                "CoreRuntimeNet regained access or ownership through {forbidden}"
            );
        }
    }

    #[test]
    fn durable_recursive_owner_copies_have_proven_roles() {
        let recursive = include_str!("recursive_cells.rs");
        let lazy = source_declaration(recursive, "struct ManagedLazyRoot");
        assert!(lazy.contains("id: LazyId"));
        assert!(lazy.contains("label: Arc<str>"));
        assert!(lazy.contains("Root<ManagedLazyCell>"));
        assert!(!lazy.contains("RuntimeValueObserver"));
        assert!(!lazy.contains("ManagedLazyEdge"));

        let promise = source_declaration(recursive, "struct ManagedPromiseRoot");
        assert!(promise.contains("id: PromiseId"));
        assert!(promise.contains("Root<ManagedPromiseCell>"));
        assert!(promise.contains("Arc<CompletionSubscriptions>"));
        assert!(promise.contains("Arc<OnceLock<Arc<PromiseProducerObligation>>>"));
        for forbidden in ["label:", "RuntimeValueObserver", "ManagedPromiseEdge"] {
            assert!(
                !promise.contains(forbidden),
                "promise owner retained unneeded state through {forbidden}"
            );
        }

        let cell = source_declaration(recursive, "struct ManagedPromiseCell");
        assert!(!cell.contains("label:"));

        let net = source_declaration(recursive, "struct ManagedCoreNetRoot");
        assert!(net.contains("Root<ManagedCoreNetCell>"));
        for forbidden in [
            "RuntimeValueObserver",
            "ManagedCoreNetEdge",
            "CoreRuntimeNet",
        ] {
            assert!(
                !net.contains(forbidden),
                "core-net root retained duplicate authority through {forbidden}"
            );
        }

        let access = source_declaration(recursive, "impl ManagedCoreNetEdge {");
        assert!(!access.contains("RuntimeValueObserver"));

        let resolver_source = include_str!("../../api/value.rs");
        let resolver = source_declaration(resolver_source, "pub struct PromiseResolver");
        for required in [
            "observer: RuntimeValueObserver",
            "label: Arc<str>",
            "promise: Option<ManagedPromiseRoot>",
        ] {
            assert!(resolver.contains(required));
        }
        assert!(!resolver.contains("EvaluationRuntimeId"));
    }

    #[test]
    fn external_promise_owner_has_no_managed_backedge() {
        let recursive = include_str!("recursive_cells.rs");
        let coordinator = include_str!("../../evaluation/coordinator.rs");
        let task = include_str!("../../evaluation/coordinator/task.rs");

        let cell = source_declaration(recursive, "struct ManagedPromiseCell");
        assert!(cell.contains("Arc<PromiseProducerObligation>"));
        assert!(!cell.contains("TaskOwnedPromiseObligation"));
        assert!(!cell.contains("LocalPromiseObligation"));

        for routing in [
            source_declaration(task, "pub(crate) struct PromiseProducerObligation"),
            source_declaration(task, "enum PromiseProducerSource"),
        ] {
            for forbidden in [
                "Gc<",
                "Root<",
                "RuntimeValueRoot",
                "EvaluationWaitToken",
                "ManagedLazy",
                "ManagedPromise",
                "ManagedCoreNet",
                "PromisedValue",
            ] {
                assert!(
                    !routing.contains(forbidden),
                    "promise routing acquired a managed backedge: {forbidden}"
                );
            }
        }
        let routes = source_declaration(task, "enum PromiseProducerSource");
        let obligation = source_declaration(task, "pub(crate) struct PromiseProducerObligation");
        assert!(obligation.contains("Weak<EvaluationWaitState>"));
        assert!(!obligation.contains("EvaluationWaitToken"));
        assert!(routes.contains("Weak<EvaluationWorkCoordinator>"));
        assert!(routes.contains("Weak<LocalPromiseOwner>"));
        assert!(!task.contains("impl Drop for PromiseProducerObligation"));

        assert!(
            source_declaration(coordinator, "struct TaskOwnedPromiseObligation")
                .contains("ManagedPromiseRoot")
        );
        assert!(
            source_declaration(task, "struct LocalPromiseObligation")
                .contains("ManagedPromiseRoot")
        );
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
                "construct_rooted_managed_lazy",
                "fn cache",
            ),
            (
                "promise",
                "ManagedPromiseCell",
                "ManagedPromiseEdge",
                "ManagedPromiseRoot",
                "ManagedPromiseAccess",
                "construct_rooted_managed_promise",
                "fn publish",
            ),
            (
                "core net",
                "ManagedCoreNetCell",
                "ManagedCoreNetEdge",
                "ManagedCoreNetRoot",
                "ManagedCoreNetAccess",
                "construct_rooted_managed_core_net",
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

    #[test]
    fn recursive_identity_mutation_is_owned_by_registered_roots() {
        let recursive = include_str!("recursive_cells.rs");
        let core = include_str!("../../core.rs");
        let resolver = include_str!("../../api/value.rs");

        let lazy_root = source_declaration(recursive, "impl ManagedLazyRoot {");
        let promise_root = source_declaration(recursive, "impl ManagedPromiseRoot {");
        assert!(lazy_root.contains("pub(crate) fn cache("));
        for operation in [
            "pub(crate) fn publish(",
            "pub(crate) fn publish_detached",
            "pub(crate) fn publish_guarded",
        ] {
            assert!(
                promise_root.contains(operation),
                "promise root lost its {operation} mutation gateway"
            );
        }

        let lazy_facade = source_declaration(core, "impl LazyValue {");
        let promise_facade = source_declaration(core, "impl PromisedValue {");
        assert!(!lazy_facade.contains("fn cache("));
        for forbidden in ["fn set(", "fn set_root(", "fn fail(", "fn publish("] {
            assert!(
                !promise_facade.contains(forbidden),
                "semantic promise facade regained mutation operation {forbidden}"
            );
        }

        let resolver = source_declaration(resolver, "impl PromiseResolver {");
        assert!(resolver.contains(".publish(&values"));
        assert!(resolver.contains("take_for_completion"));
    }

    #[test]
    fn registered_family_roots_store_one_canonical_allocation_identity() {
        let recursive = include_str!("recursive_cells.rs");
        for (root, cell) in [
            ("ManagedLazyRoot", "ManagedLazyCell"),
            ("ManagedPromiseRoot", "ManagedPromiseCell"),
            ("ManagedCoreNetRoot", "ManagedCoreNetCell"),
        ] {
            let declaration = source_declaration(recursive, &format!("struct {root}"));
            assert!(
                declaration.contains(&format!("Root<{cell}>")),
                "{root} lost its canonical registered root"
            );
            for forbidden in [
                "Gc<",
                "ManagedLazyEdge",
                "ManagedPromiseEdge",
                "ManagedCoreNetEdge",
                "LazyValue",
                "PromisedValue",
                "CoreRuntimeNet",
            ] {
                assert!(
                    !declaration.contains(forbidden),
                    "{root} caches a second allocation identity through {forbidden}"
                );
            }
        }
    }

    #[test]
    fn durable_recursive_wrappers_do_not_hide_a_second_semantic_identity() {
        let halt = include_str!("../../core/evaluation_halt.rs");
        let core_net = include_str!("../../core_net.rs");
        let eval_net = include_str!("../../eval/net.rs");

        for (declaration, required, forbidden) in [
            (
                source_declaration(halt, "enum EvaluationHaltKind"),
                "ManagedPromiseRoot",
                "PromisedValue",
            ),
            (
                source_declaration(eval_net, "struct NormalizationRequest"),
                "ManagedCoreNetRoot",
                "CoreRuntimeNet",
            ),
            (
                source_declaration(core_net, "struct CorePreparedCopySource"),
                "ManagedCoreNetRoot",
                "PreparedCopySource<",
            ),
            (
                source_declaration(core_net, "struct CoreFrontierObservation"),
                "ManagedCoreNetRoot",
                "FrontierObservation<",
            ),
        ] {
            assert!(declaration.contains(required));
            assert!(
                !declaration.contains(forbidden),
                "durable {required} wrapper also caches {forbidden}"
            );
        }
    }
}
