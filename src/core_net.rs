//! Core operators and specialization for generic interaction nets.
//!
//! Front-end semantic lowering lives in `g_syntax`; this module deliberately
//! contains no expression language.

use std::sync::Arc;

use crate::core::{
    BuiltinCall, CoreValueFactory, EvaluationHalt, FunctionCode, Key, ManagedCoreNetAccess,
    ManagedCoreNetEdge, ManagedCoreNetRoot, RuntimeValueAccess, Value,
};
use crate::evaluation::EvaluationWaitToken;
#[cfg(test)]
use crate::interaction_net::RuntimeNetRevisions;
use crate::interaction_net::{
    ActivePairKey, ActivePairStep, BlockedCall, BlockedOperatorCall, CursorDependency,
    CursorDependencyDisposition, CursorDependencyResolution, CursorProgress, CursorStep,
    DemandEndpoint, FrontierObservation, InteractionNet, InterfaceDemand, NetContention, NodeId,
    OperatorYield, Port, PreparedCopySource, Reduction, RuntimeNet, RuntimeNetMutation,
    SourceFrontier,
};
use crate::runtime::RuntimeValueRoot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreDataKey {
    Key(Key),
    Index,
    PathIndex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreOperator {
    ApplyArity {
        arity: usize,
        supplied: Arc<[Value]>,
    },
    FunctionCaptures {
        code: Arc<FunctionCode>,
        supplied: Arc<[Value]>,
    },
    ComputationCaptures {
        code: Arc<FunctionCode>,
        supplied: Arc<[Value]>,
    },
    Dict {
        keys: Arc<[Key]>,
        supplied: Arc<[Value]>,
    },
    Builtin(BuiltinCall),
    Applicable(Value),
    List {
        arity: usize,
        supplied: Arc<[Value]>,
    },
    Access {
        path: Arc<[CoreDataKey]>,
        supplied: Arc<[Value]>,
    },
    /// Reifies an opaque-tagged external effect request without performing it
    /// during interaction-net evaluation.
    Request {
        tag: Key,
        arity: usize,
        supplied: Arc<[Value]>,
        wrap_effect: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreSpecialization;

/// Opaque identity for evaluator work that suspends a core net call. The weak
/// session provenance remains hidden from the generic runtime, which only
/// clones and compares tokens for exact wakeups.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoreWaitToken(pub(crate) EvaluationWaitToken);

impl CoreWaitToken {
    pub(crate) fn wait_id(&self) -> u64 {
        self.0.get()
    }
}

pub type CoreInteractionNet = InteractionNet<CoreSpecialization>;

/// Runtime-local identity of one shared core interaction net.
///
/// The generic shared owner remains private. This facade is exactly one
/// non-rooting managed edge; every inspection, mutation, or root projection
/// requires explicit matching `RuntimeValueAccess`. Runtime provenance is
/// established at public construction boundaries and rechecked by registered
/// roots or collector debug validation rather than cached on every net edge.
#[derive(Clone)]
pub struct CoreRuntimeNet {
    edge: ManagedCoreNetEdge,
}

// This target-specific latch records the final one-edge facade cost. It is an
// implementation diagnostic, not an ABI promise.
#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
const _: () = assert!(std::mem::size_of::<CoreRuntimeNet>() == 8);

/// One bounded, thread-local authority to inspect or mutate a core net.
///
/// The view borrows both the durable net and the matching managed-value access
/// carrier. It cannot enter a work descriptor, survive the mutator region, or
/// cross a thread. The generic shared owner remains hidden behind this view.
pub(crate) struct CoreRuntimeNetAccess<'access, 'scope> {
    owner: &'access CoreRuntimeNet,
    runtime: ManagedCoreNetAccess<'access, 'scope>,
    values: &'access RuntimeValueAccess<'scope>,
}

#[cfg(test)]
std::thread_local! {
    static CORE_NORMALIZATION_SCOPE_DEPTH: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
struct CoreNormalizationScopeForTest;

#[cfg(test)]
impl CoreNormalizationScopeForTest {
    fn enter() -> Self {
        CORE_NORMALIZATION_SCOPE_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self
    }
}

#[cfg(test)]
impl Drop for CoreNormalizationScopeForTest {
    fn drop(&mut self) {
        CORE_NORMALIZATION_SCOPE_DEPTH.with(|depth| {
            depth.set(
                depth
                    .get()
                    .checked_sub(1)
                    .expect("normalization scope depth must remain balanced"),
            );
        });
    }
}

#[cfg(test)]
pub(crate) fn thread_has_active_core_normalization_scope() -> bool {
    CORE_NORMALIZATION_SCOPE_DEPTH.with(|depth| depth.get() != 0)
}

impl CoreValueFactory {
    /// Instantiates a core net in this factory's exact value domain.
    #[cfg(test)]
    pub(crate) fn instantiate_core_net(&self, template: &CoreInteractionNet) -> CoreRuntimeNet {
        self.with_runtime_value_access(|access| {
            access
                .construct_managed_core_net(template.instantiate())
                .expect("managed core-net representation must fit one collector run")
        })
    }

    #[cfg(test)]
    fn construct_core_runtime_net_for_test(
        &self,
        runtime: RuntimeNet<CoreSpecialization>,
    ) -> CoreRuntimeNet {
        self.with_runtime_value_access(|access| {
            access
                .construct_managed_core_net(runtime)
                .expect("managed core-net test representation must fit one collector run")
        })
    }
}

impl CoreRuntimeNet {
    pub(crate) fn from_root(root: &ManagedCoreNetRoot, access: &RuntimeValueAccess<'_>) -> Self {
        Self::from_managed_edge(root.edge(access))
    }

    pub(crate) fn from_managed_edge(edge: ManagedCoreNetEdge) -> Self {
        Self { edge }
    }

    /// Duplicates this semantic net edge under matching value-domain access.
    ///
    /// The returned facade remains non-rooting and must be installed beneath
    /// a traced owner before the surrounding access quantum ends.
    #[inline(always)]
    pub(crate) fn duplicate_in(&self, access: &RuntimeValueAccess<'_>) -> Self {
        Self::from_managed_edge(self.edge.duplicate_in(access))
    }

    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        self.edge == other.edge
    }

    /// Compares exact managed-net identity under matching value access.
    pub(crate) fn same_net_in(&self, other: &Self, access: &RuntimeValueAccess<'_>) -> bool {
        self.edge.same_allocation_in(&other.edge, access)
    }

    /// Derives bounded net access from matching value-domain authority.
    pub(crate) fn access<'access, 'scope>(
        &'access self,
        values: &'access RuntimeValueAccess<'scope>,
    ) -> CoreRuntimeNetAccess<'access, 'scope> {
        let runtime = self.edge.access(values);
        CoreRuntimeNetAccess {
            owner: self,
            runtime,
            values,
        }
    }

    pub(crate) fn root_in(&self, access: &RuntimeValueAccess<'_>) -> ManagedCoreNetRoot {
        access.root_managed_core_net(&self.edge)
    }

    pub(crate) fn trace_managed_edge(&self, visitor: &mut glam_gc::Visitor<'_>) {
        self.edge.trace(visitor);
    }

    #[cfg(test)]
    pub(crate) fn with_test_access<R>(
        &self,
        values: &CoreValueFactory,
        operation: impl FnOnce(CoreRuntimeNetAccess<'_, '_>) -> R,
    ) -> R {
        values.with_runtime_value_access(|access| operation(self.access(&access)))
    }

    /// Test-only spelling for an intentional second non-rooting edge.
    #[cfg(test)]
    pub(crate) fn duplicate_for_test(&self, values: &CoreValueFactory) -> Self {
        values.with_runtime_value_access(|access| self.duplicate_in(&access))
    }

    #[cfg(test)]
    pub(crate) fn test_with<R>(
        &self,
        values: &CoreValueFactory,
        inspect: impl FnOnce(&RuntimeNet<CoreSpecialization>) -> R,
    ) -> R {
        self.with_test_access(values, |access| access.with(inspect))
    }

    #[cfg(test)]
    pub(crate) fn test_with_revisions<R>(
        &self,
        values: &CoreValueFactory,
        inspect: impl FnOnce(&RuntimeNet<CoreSpecialization>) -> R,
    ) -> (R, RuntimeNetRevisions) {
        self.with_test_access(values, |access| access.with_revisions(inspect))
    }

    #[cfg(test)]
    pub(crate) fn test_with_mut<R>(
        &self,
        values: &CoreValueFactory,
        update: impl FnOnce(&mut RuntimeNet<CoreSpecialization>) -> R,
    ) -> R {
        self.with_test_access(values, |access| access.with_mut(update))
    }

    #[cfg(test)]
    pub(crate) fn test_with_optional_mut<R>(
        &self,
        values: &CoreValueFactory,
        update: impl FnOnce(&mut RuntimeNet<CoreSpecialization>) -> Option<R>,
    ) -> Option<R> {
        self.with_test_access(values, |access| access.with_optional_mut(update))
    }

    #[cfg(test)]
    pub(crate) fn test_poll_interface_demand(
        &self,
        values: &CoreValueFactory,
        interface: Port,
    ) -> InterfaceDemand {
        self.with_test_access(values, |access| access.poll_interface_demand(interface))
    }

    #[cfg(test)]
    pub(crate) fn test_step_cursor(
        &self,
        values: &CoreValueFactory,
        cursor: NodeId,
    ) -> CoreCursorStep {
        self.with_test_access(values, |access| access.step_cursor(cursor))
    }

    #[cfg(test)]
    pub(crate) fn test_advance_claimed_cursor(
        &self,
        values: &CoreValueFactory,
        cursor: NodeId,
    ) -> Option<CursorProgress> {
        self.with_test_access(values, |access| access.test_advance_claimed_cursor(cursor))
    }

    #[cfg(test)]
    pub(crate) fn test_prepare_copy_source(
        &self,
        values: &CoreValueFactory,
    ) -> CorePreparedCopySource {
        self.with_test_access(values, |access| access.prepare_copy_source())
    }

    #[cfg(test)]
    pub(crate) fn active_normalization_batch(
        &self,
        values: &CoreValueFactory,
    ) -> Option<(u64, bool)> {
        self.with_test_access(values, |access| {
            access.runtime.cell().active_normalization_batch()
        })
    }

    #[cfg(test)]
    pub(crate) fn test_stable_auxiliary(values: &CoreValueFactory) -> (Self, Port) {
        let (runtime, interface) = RuntimeNet::test_stable_auxiliary();
        (
            values.construct_core_runtime_net_for_test(runtime),
            interface,
        )
    }

    #[cfg(test)]
    pub(crate) fn test_copy_layer(values: &CoreValueFactory, source: Self) -> (Self, Port) {
        let (prepared, _source_root) = source
            .test_prepare_copy_source(values)
            .into_inner_for_factory(values);
        let (runtime, interface) = RuntimeNet::test_copy_layer_from(prepared);
        (
            values.construct_core_runtime_net_for_test(runtime),
            interface,
        )
    }

    #[cfg(test)]
    pub(crate) fn test_pair_owned_copy_layer(
        values: &CoreValueFactory,
        source: Self,
    ) -> (Self, Port, NodeId) {
        let (prepared, _source_root) = source
            .test_prepare_copy_source(values)
            .into_inner_for_factory(values);
        let (runtime, interface, cursor) = RuntimeNet::test_pair_owned_copy_layer_from(prepared);
        (
            values.construct_core_runtime_net_for_test(runtime),
            interface,
            cursor,
        )
    }

    #[cfg(test)]
    pub(crate) fn test_productive_pair_owned_copy_layer(
        values: &CoreValueFactory,
        source: Self,
    ) -> (Self, Port) {
        let (prepared, _source_root) = source
            .test_prepare_copy_source(values)
            .into_inner_for_factory(values);
        let (runtime, interface) = RuntimeNet::test_productive_pair_owned_copy_layer_from(prepared);
        (
            values.construct_core_runtime_net_for_test(runtime),
            interface,
        )
    }

    #[cfg(test)]
    pub(crate) fn test_stable_root_with_claimed_cursor(
        values: &CoreValueFactory,
        source: Self,
    ) -> (Self, Port, NodeId) {
        let (prepared, _source_root) = source
            .test_prepare_copy_source(values)
            .into_inner_for_factory(values);
        let (runtime, interface, cursor) =
            RuntimeNet::test_stable_root_with_claimed_cursor_from(prepared);
        (
            values.construct_core_runtime_net_for_test(runtime),
            interface,
            cursor,
        )
    }

    #[cfg(test)]
    pub(crate) fn test_claim_pairless_cursor_obligation(
        &self,
        values: &CoreValueFactory,
        cursor: NodeId,
    ) -> bool {
        self.with_test_access(values, |access| {
            access.with_mut(|runtime| runtime.claim_pairless_cursor_obligation(cursor))
        })
    }
}

impl CoreRuntimeNetAccess<'_, '_> {
    #[cfg(test)]
    pub(crate) fn active_normalization_batch(&self) -> Option<(u64, bool)> {
        self.runtime.cell().active_normalization_batch()
    }

    pub(crate) fn values(&self) -> &RuntimeValueAccess<'_> {
        self.values
    }

    /// Runs one same-net normalization batch inside this managed-access
    /// region. The generic lease remains private to this call, closes before
    /// the callback result is returned, and falls back to `Drop` on unwind.
    pub(crate) fn with_normalization_batch<R>(
        &self,
        operation: impl FnOnce(&Self) -> R,
    ) -> Result<R, CoreNetContention> {
        let lease = self
            .runtime
            .cell()
            .try_begin_normalization_batch()
            .map_err(CoreNetContention::new)?;
        #[cfg(test)]
        let scope = CoreNormalizationScopeForTest::enter();
        let result = operation(self);
        lease.close();
        #[cfg(test)]
        drop(scope);
        Ok(result)
    }

    pub(crate) fn with<R>(&self, inspect: impl FnOnce(&RuntimeNet<CoreSpecialization>) -> R) -> R {
        self.runtime.with(inspect)
    }

    #[cfg(test)]
    pub(crate) fn with_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<CoreSpecialization>) -> R,
    ) -> R {
        self.runtime.with_mut(update)
    }

    #[cfg(test)]
    pub(crate) fn with_revisions<R>(
        &self,
        inspect: impl FnOnce(&RuntimeNet<CoreSpecialization>) -> R,
    ) -> (R, RuntimeNetRevisions) {
        self.runtime.cell().with_revisions(inspect)
    }

    #[cfg(test)]
    pub(crate) fn with_optional_mut<R>(
        &self,
        update: impl FnOnce(&mut RuntimeNet<CoreSpecialization>) -> Option<R>,
    ) -> Option<R> {
        self.runtime
            .cell()
            .with_optional_mut_via(&self.runtime, update)
    }

    pub(crate) fn poll_interface_demand(&self, interface: Port) -> InterfaceDemand {
        self.runtime
            .cell()
            .with_conditional_mut_via(&self.runtime, |runtime| {
                runtime.poll_interface_demand(interface)
            })
    }

    pub(crate) fn resolve_cursor_dependency(
        &self,
        cursor: NodeId,
        expected: &CoreCursorDependency,
        disposition: CursorDependencyDisposition,
    ) -> CursorDependencyResolution {
        self.runtime.cell().with_conditional_edge_mut_via(
            &self.runtime,
            |runtime| {
                runtime.resolve_cursor_dependency_edge_transition(
                    cursor,
                    &expected.to_generic(self.values),
                )
            },
            |runtime| {
                let resolution = runtime.resolve_cursor_dependency(
                    cursor,
                    &expected.to_generic(self.values),
                    disposition,
                );
                if resolution == CursorDependencyResolution::Resolved {
                    RuntimeNetMutation::Changed(resolution)
                } else {
                    RuntimeNetMutation::Unchanged(resolution)
                }
            },
        )
    }

    pub(crate) fn step_cursor(&self, cursor: NodeId) -> CoreCursorStep {
        self.step_cursor_if_current(cursor, None)
    }

    pub(crate) fn step_active_pair(&self, pair: ActivePairKey) -> CoreActivePairStep {
        self.step_active_pair_if_current(pair, None)
    }

    pub(crate) fn prepare_copy_source(&self) -> CorePreparedCopySource {
        CorePreparedCopySource {
            root: self.owner.root_in(self.values),
            remote: self.runtime.with(RuntimeNet::exposed),
        }
    }

    fn inspect_source_frontier(
        &self,
        source: &CoreRuntimeNet,
        anchor: Port,
    ) -> SourceFrontier<CoreSpecialization> {
        let source = source.access(self.values);
        source
            .runtime
            .cell()
            .inspect_source_frontier(source.owner.duplicate_in(self.values), anchor)
    }

    fn step_cursor_if_current(
        &self,
        cursor: NodeId,
        expected_topology_revision: Option<u64>,
    ) -> CoreCursorStep {
        let step = self.runtime.cell().step_cursor_with_gateway(
            cursor,
            expected_topology_revision,
            &self.runtime,
            |source, anchor| self.inspect_source_frontier(source, anchor),
        );
        CoreCursorStep::from_generic(step, self.values)
    }

    fn step_active_pair_if_current(
        &self,
        pair: ActivePairKey,
        expected_topology_revision: Option<u64>,
    ) -> CoreActivePairStep {
        let step = self.runtime.cell().step_active_pair_with_gateway(
            pair,
            expected_topology_revision,
            &self.runtime,
            |source, anchor| self.inspect_source_frontier(source, anchor),
        );
        CoreActivePairStep::from_generic(step)
    }

    #[cfg(test)]
    fn test_advance_claimed_cursor(&self, cursor: NodeId) -> Option<CursorProgress> {
        self.runtime
            .cell()
            .test_advance_claimed_cursor_with_gateway(cursor, &self.runtime, |source, anchor| {
                self.inspect_source_frontier(source, anchor)
            })
    }

    pub(crate) fn resume_claimed_call_with_copy(
        &self,
        call: crate::interaction_net::Call,
        source: CorePreparedCopySource,
    ) {
        let (source, _source_root) = source.into_inner_for(self.values);
        self.runtime.cell().with_edge_mut_via(
            &self.runtime,
            |runtime| runtime.resume_call_with_copy_edge_transition(call),
            |runtime| runtime.resume_claimed_call_with_copy(call, source),
        );
    }

    pub(crate) fn claim_call(&self, call: crate::interaction_net::Call) -> Option<Value> {
        self.runtime.cell().with(|runtime| runtime.claim_call(call))
    }

    pub(crate) fn claim_call_rooted(
        &self,
        call: crate::interaction_net::Call,
    ) -> Option<RuntimeValueRoot> {
        self.claim_call(call)
            .map(|value| self.values.root_runtime_value(value))
    }

    pub(crate) fn reclaim_blocked_call(
        &self,
        blocked: &BlockedCall<CoreWaitToken>,
    ) -> Option<(crate::interaction_net::Call, RuntimeValueRoot)> {
        self.runtime
            .cell()
            .with_conditional_mut_via(&self.runtime, |runtime| {
                let Some(call) = runtime.call(blocked.pair) else {
                    return RuntimeNetMutation::Unchanged(None);
                };
                if !runtime.retry_blocked_call(call, &blocked.wait) {
                    return RuntimeNetMutation::Unchanged(None);
                }
                let callable = runtime
                    .claim_call(call)
                    .expect("reclaimed call must expose its callable data");
                RuntimeNetMutation::Changed(Some((call, self.values.root_runtime_value(callable))))
            })
    }

    pub(crate) fn resume_claimed_call_with_operator(
        &self,
        call: crate::interaction_net::Call,
        operator: CoreOperator,
    ) {
        self.runtime.cell().with_edge_mut_via(
            &self.runtime,
            |runtime| runtime.resume_call_with_operator_edge_transition(call),
            |runtime| runtime.resume_claimed_call_with_operator(call, operator),
        );
    }

    pub(crate) fn block_claimed_call(
        &self,
        call: crate::interaction_net::Call,
        wait: CoreWaitToken,
    ) {
        self.runtime.cell().with_mut_via(&self.runtime, |runtime| {
            runtime.block_claimed_call(call, wait)
        });
    }

    pub(crate) fn fail_claimed_call(
        &self,
        call: crate::interaction_net::Call,
        error: EvaluationHalt,
    ) {
        self.runtime.cell().with_edge_mut_via(
            &self.runtime,
            |runtime| runtime.fail_call_edge_transition(call),
            |runtime| runtime.fail_claimed_call(call, error),
        );
    }

    pub(crate) fn release_claimed_call(&self, call: crate::interaction_net::Call) -> bool {
        self.runtime
            .cell()
            .with_conditional_mut_via(&self.runtime, |runtime| {
                if runtime.release_claimed_call(call) {
                    RuntimeNetMutation::Changed(true)
                } else {
                    RuntimeNetMutation::Unchanged(false)
                }
            })
    }

    pub(crate) fn restore_blocked_call(
        &self,
        call: crate::interaction_net::Call,
        wait: CoreWaitToken,
    ) -> bool {
        self.runtime
            .cell()
            .with_conditional_mut_via(&self.runtime, |runtime| {
                if runtime.restore_blocked_call(call, wait) {
                    RuntimeNetMutation::Changed(true)
                } else {
                    RuntimeNetMutation::Unchanged(false)
                }
            })
    }

    pub(crate) fn claim_operator_call(
        &self,
        call: crate::interaction_net::OperatorCall,
    ) -> Option<(CoreOperator, Value)> {
        self.runtime
            .cell()
            .with(|runtime| runtime.claim_operator_call(call))
    }

    pub(crate) fn reclaim_blocked_operator_call(
        &self,
        blocked: &BlockedOperatorCall<CoreWaitToken>,
    ) -> Option<(crate::interaction_net::OperatorCall, CoreOperator, Value)> {
        self.runtime
            .cell()
            .with_conditional_mut_via(&self.runtime, |runtime| {
                let Some(call) = runtime.operator_call(blocked.pair) else {
                    return RuntimeNetMutation::Unchanged(None);
                };
                if !runtime.retry_blocked_operator_call(call, &blocked.wait) {
                    return RuntimeNetMutation::Unchanged(None);
                }
                let (operator, data) = runtime
                    .claim_operator_call(call)
                    .expect("reclaimed operator call must expose its payloads");
                RuntimeNetMutation::Changed(Some((call, operator, data)))
            })
    }

    pub(crate) fn complete_claimed_operator_call(
        &self,
        call: crate::interaction_net::OperatorCall,
        result: OperatorYield<CoreSpecialization>,
    ) {
        self.runtime.cell().with_input_edge_mut_via(
            &self.runtime,
            result,
            |runtime, result| runtime.complete_operator_edge_transition(call, result),
            |runtime, result| runtime.complete_operator_call(call, result),
        );
    }

    pub(crate) fn block_claimed_operator_call(
        &self,
        call: crate::interaction_net::OperatorCall,
        wait: CoreWaitToken,
    ) {
        self.runtime.cell().with_mut_via(&self.runtime, |runtime| {
            runtime.block_claimed_operator_call(call, wait)
        });
    }

    pub(crate) fn fail_claimed_operator_call(
        &self,
        call: crate::interaction_net::OperatorCall,
        error: EvaluationHalt,
    ) {
        self.runtime.cell().with_edge_mut_via(
            &self.runtime,
            |runtime| runtime.fail_operator_edge_transition(call),
            |runtime| runtime.fail_operator_call(call, error),
        );
    }

    pub(crate) fn release_claimed_operator_call(
        &self,
        call: crate::interaction_net::OperatorCall,
    ) -> bool {
        self.runtime
            .cell()
            .with_conditional_mut_via(&self.runtime, |runtime| {
                if runtime.release_claimed_operator_call(call) {
                    RuntimeNetMutation::Changed(true)
                } else {
                    RuntimeNetMutation::Unchanged(false)
                }
            })
    }

    pub(crate) fn restore_blocked_operator_call(
        &self,
        call: crate::interaction_net::OperatorCall,
        wait: CoreWaitToken,
    ) -> bool {
        self.runtime
            .cell()
            .with_conditional_mut_via(&self.runtime, |runtime| {
                if runtime.restore_blocked_operator_call(call, wait) {
                    RuntimeNetMutation::Changed(true)
                } else {
                    RuntimeNetMutation::Unchanged(false)
                }
            })
    }
}

impl std::fmt::Debug for CoreRuntimeNet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("CoreRuntimeNet")
            .field(&self.edge)
            .finish()
    }
}

impl PartialEq for CoreRuntimeNet {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other)
    }
}

impl Eq for CoreRuntimeNet {}

pub(crate) struct CorePreparedCopySource {
    root: ManagedCoreNetRoot,
    remote: Port,
}

impl CorePreparedCopySource {
    fn into_inner_for(
        self,
        target: &RuntimeValueAccess<'_>,
    ) -> (PreparedCopySource<CoreSpecialization>, ManagedCoreNetRoot) {
        let source = CoreRuntimeNet::from_root(&self.root, target);
        (PreparedCopySource::new(source, self.remote), self.root)
    }

    #[cfg(test)]
    fn into_inner_for_factory(
        self,
        target: &CoreValueFactory,
    ) -> (PreparedCopySource<CoreSpecialization>, ManagedCoreNetRoot) {
        target.with_runtime_value_access(|access| self.into_inner_for(&access))
    }
}

/// One local synchronization handoff to the evaluator that currently owns a
/// normalization batch or structurally bracketed claim.
///
/// This is deliberately neither a semantic wait token nor cloneable durable
/// state. It may leave scoped value access only so the caller can wait for the
/// exact observed disturbance and immediately retry its normalization
/// request.
pub(crate) struct CoreNetContention {
    inner: NetContention,
}

impl CoreNetContention {
    fn new(contention: NetContention) -> Self {
        Self { inner: contention }
    }

    pub(crate) fn wait_for_disturbance(self) {
        let _ = self.inner.wait_for_disturbance();
    }
}

impl std::fmt::Debug for CoreNetContention {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoreNetContention")
            .field("inner", &self.inner)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CoreFrontierObservation {
    root: ManagedCoreNetRoot,
    observed_topology: u64,
    endpoint: DemandEndpoint,
}

impl CoreFrontierObservation {
    fn from_generic(
        inner: FrontierObservation<CoreSpecialization>,
        access: &RuntimeValueAccess<'_>,
    ) -> Self {
        Self {
            root: inner.source().root_in(access),
            observed_topology: inner.observed_topology_revision(),
            endpoint: inner.endpoint(),
        }
    }

    pub(crate) fn source(&self, access: &RuntimeValueAccess<'_>) -> CoreRuntimeNet {
        CoreRuntimeNet::from_root(&self.root, access)
    }

    pub(crate) fn endpoint(&self) -> DemandEndpoint {
        self.endpoint
    }

    pub(crate) fn root(&self) -> &ManagedCoreNetRoot {
        &self.root
    }

    pub(crate) fn step_active_pair(
        &self,
        access: &CoreRuntimeNetAccess<'_, '_>,
        pair: ActivePairKey,
    ) -> CoreActivePairStep {
        assert!(
            self.source(access.values)
                .same_net_in(access.owner, access.values),
            "frontier observation requires access to its source net"
        );
        access.step_active_pair_if_current(pair, Some(self.observed_topology))
    }

    pub(crate) fn step_cursor(
        &self,
        access: &CoreRuntimeNetAccess<'_, '_>,
        cursor: NodeId,
    ) -> CoreCursorStep {
        assert!(
            self.source(access.values)
                .same_net_in(access.owner, access.values),
            "frontier observation requires access to its source net"
        );
        access.step_cursor_if_current(cursor, Some(self.observed_topology))
    }

    fn to_generic(
        &self,
        access: &RuntimeValueAccess<'_>,
    ) -> FrontierObservation<CoreSpecialization> {
        FrontierObservation::from_snapshot(
            self.source(access),
            self.observed_topology,
            self.endpoint,
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) enum CoreCursorDependency {
    LocalCursor(NodeId),
    SourceCursor(CoreFrontierObservation),
    SourceFrontier(CoreFrontierObservation),
}

impl CoreCursorDependency {
    fn from_generic(
        dependency: CursorDependency<CoreSpecialization>,
        access: &RuntimeValueAccess<'_>,
    ) -> Self {
        match dependency {
            CursorDependency::LocalCursor(cursor) => Self::LocalCursor(cursor),
            CursorDependency::SourceCursor(observation) => {
                Self::SourceCursor(CoreFrontierObservation::from_generic(observation, access))
            }
            CursorDependency::SourceFrontier(observation) => {
                Self::SourceFrontier(CoreFrontierObservation::from_generic(observation, access))
            }
        }
    }

    fn to_generic(&self, access: &RuntimeValueAccess<'_>) -> CursorDependency<CoreSpecialization> {
        match self {
            Self::LocalCursor(cursor) => CursorDependency::LocalCursor(*cursor),
            Self::SourceCursor(observation) => {
                CursorDependency::SourceCursor(observation.to_generic(access))
            }
            Self::SourceFrontier(observation) => {
                CursorDependency::SourceFrontier(observation.to_generic(access))
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum CoreCursorStep {
    Progressed(CursorProgress),
    Dependency(CoreCursorDependency),
    Stable,
    Contended(CoreNetContention),
    Disturbed,
    Gone,
}

impl CoreCursorStep {
    fn from_generic(step: CursorStep<CoreSpecialization>, access: &RuntimeValueAccess<'_>) -> Self {
        match step {
            CursorStep::Progressed(CursorProgress::Claimed) => {
                panic!("a live cursor claim cannot cross the core-net facade")
            }
            CursorStep::Progressed(progress) => Self::Progressed(progress),
            CursorStep::Dependency(dependency) => {
                Self::Dependency(CoreCursorDependency::from_generic(dependency, access))
            }
            CursorStep::Stable => Self::Stable,
            CursorStep::Contended(contention) => {
                Self::Contended(CoreNetContention::new(contention))
            }
            CursorStep::Disturbed => Self::Disturbed,
            CursorStep::Gone => Self::Gone,
        }
    }
}

#[derive(Debug)]
pub(crate) enum CoreActivePairStep {
    Reduction(Reduction),
    Cursor(NodeId),
    BlockedCall(BlockedCall<CoreWaitToken>),
    BlockedOperatorCall(BlockedOperatorCall<CoreWaitToken>),
    Stuck,
    Contended(CoreNetContention),
    Disturbed,
    Gone,
}

impl CoreActivePairStep {
    fn from_generic(step: ActivePairStep<CoreSpecialization>) -> Self {
        match step {
            ActivePairStep::Reduction(Reduction {
                kind:
                    crate::interaction_net::ReductionKind::RemoteCursor {
                        progress: CursorProgress::Claimed,
                        ..
                    },
                ..
            }) => panic!("a live cursor claim cannot cross the core-net facade"),
            ActivePairStep::Reduction(reduction) => Self::Reduction(reduction),
            ActivePairStep::Cursor(cursor) => Self::Cursor(cursor),
            ActivePairStep::BlockedCall(blocked) => Self::BlockedCall(blocked),
            ActivePairStep::BlockedOperatorCall(blocked) => Self::BlockedOperatorCall(blocked),
            ActivePairStep::Stuck(_) => Self::Stuck,
            ActivePairStep::Contended(contention) => {
                Self::Contended(CoreNetContention::new(contention))
            }
            ActivePairStep::Disturbed => Self::Disturbed,
            ActivePairStep::Gone => Self::Gone,
        }
    }
}

// These are representation diagnostics for the observer-free I8A.0 handoff,
// not ABI promises. Durable wrappers retain one registered root plus only
// their edge-free operation snapshot.
#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
const _: () = {
    assert!(std::mem::size_of::<CorePreparedCopySource>() == 16);
    assert!(std::mem::size_of::<CoreFrontierObservation>() == 32);
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};
    use glam_gc::EdgeTransitionObservation;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use syn::visit::{self, Visit};

    #[derive(Default)]
    struct MethodCallInventory {
        current: Option<String>,
        calls: BTreeMap<String, BTreeSet<String>>,
    }

    impl<'ast> Visit<'ast> for MethodCallInventory {
        fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
            let previous = self.current.replace(function.sig.ident.to_string());
            visit::visit_block(self, &function.block);
            self.current = previous;
        }

        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if let Some(current) = self.current.as_ref() {
                self.calls
                    .entry(current.clone())
                    .or_default()
                    .insert(call.method.to_string());
            }
            visit::visit_expr_method_call(self, call);
        }
    }

    fn method_call_inventory(relative: &str) -> BTreeMap<String, BTreeSet<String>> {
        let source = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join(relative),
        )
        .expect("inventoried Rust source must be readable");
        let syntax = syn::parse_file(&source).expect("inventoried Rust source must parse");
        let mut inventory = MethodCallInventory::default();
        inventory.visit_file(&syntax);
        inventory.calls
    }

    fn assert_core_net_durable_owner_inventory(
        runtime: &CoreRuntimeNet,
        prepared: &CorePreparedCopySource,
        contention: &CoreNetContention,
        observation: &CoreFrontierObservation,
        operator: &CoreOperator,
    ) {
        fn assert_core_source(
            _: &<CoreSpecialization as crate::interaction_net::NetSpecialization>::RuntimeSource,
        ) {
        }

        let _: fn(&CoreRuntimeNet) = assert_core_source;

        let CoreRuntimeNet { edge } = runtime;
        let _: &ManagedCoreNetEdge = edge;

        let CorePreparedCopySource { root, remote } = prepared;
        let _: &ManagedCoreNetRoot = root;
        let _: &Port = remote;

        let CoreNetContention { inner } = contention;
        let _: &NetContention = inner;

        let CoreFrontierObservation {
            root,
            observed_topology,
            endpoint,
        } = observation;
        let _: &ManagedCoreNetRoot = root;
        let _: &u64 = observed_topology;
        let _: &DemandEndpoint = endpoint;

        match operator {
            CoreOperator::ApplyArity { arity, supplied }
            | CoreOperator::List { arity, supplied } => {
                let _: &usize = arity;
                let _: &Arc<[Value]> = supplied;
            }
            CoreOperator::FunctionCaptures { code, supplied }
            | CoreOperator::ComputationCaptures { code, supplied } => {
                let _: &Arc<FunctionCode> = code;
                let _: &Arc<[Value]> = supplied;
            }
            CoreOperator::Dict { keys, supplied } => {
                let _: &Arc<[Key]> = keys;
                let _: &Arc<[Value]> = supplied;
            }
            CoreOperator::Builtin(call) => {
                let _: &BuiltinCall = call;
            }
            CoreOperator::Applicable(value) => {
                let _: &Value = value;
            }
            CoreOperator::Access { path, supplied } => {
                let _: &Arc<[CoreDataKey]> = path;
                let _: &Arc<[Value]> = supplied;
            }
            CoreOperator::Request {
                tag,
                arity,
                supplied,
                wrap_effect,
            } => {
                let _: &Key = tag;
                let _: &usize = arity;
                let _: &Arc<[Value]> = supplied;
                let _: &bool = wrap_effect;
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
        core_runtime_net_access_is_not_send,
        CoreRuntimeNetAccess<'static, 'static>,
        Send
    );
    assert_does_not_implement!(
        core_runtime_net_access_is_not_sync,
        CoreRuntimeNetAccess<'static, 'static>,
        Sync
    );

    fn closed_unit_template(values: &CoreValueFactory) -> CoreInteractionNet {
        let mut builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
        let data = builder.data(values.unit());
        builder.finish(data)
    }

    fn claimed_call(
        values: &CoreValueFactory,
        callable: Value,
    ) -> (CoreRuntimeNet, crate::interaction_net::Call) {
        let mut builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
        let [function, argument, result] = builder.bind();
        let callable = builder.data(callable);
        let supplied = builder.data(Value::Number(11.into()));
        builder.wire(function, callable);
        builder.wire(argument, supplied);
        let runtime = values.instantiate_core_net(&builder.finish(result));
        let pair = runtime.test_with(values, |runtime| {
            runtime
                .active_pairs()
                .next()
                .expect("call pair should be active")
        });
        let reduction = runtime.with_test_access(values, |runtime| runtime.step_active_pair(pair));
        let CoreActivePairStep::Reduction(Reduction {
            kind: crate::interaction_net::ReductionKind::Call { bind, data },
            ..
        }) = reduction
        else {
            panic!("callable data should claim a Bind/Data call, got {reduction:?}")
        };
        (runtime, crate::interaction_net::Call { pair, bind, data })
    }

    fn claimed_operator_call(
        values: &CoreValueFactory,
        operator: CoreOperator,
        argument: Value,
    ) -> (CoreRuntimeNet, crate::interaction_net::OperatorCall) {
        let mut builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
        let [input, result] = builder.operator(operator);
        let argument = builder.data(argument);
        builder.wire(input, argument);
        let runtime = values.instantiate_core_net(&builder.finish(result));
        let pair = runtime.test_with(values, |runtime| {
            runtime
                .active_pairs()
                .next()
                .expect("operator pair should be active")
        });
        let reduction = runtime.with_test_access(values, |runtime| runtime.step_active_pair(pair));
        let CoreActivePairStep::Reduction(Reduction {
            kind: crate::interaction_net::ReductionKind::OperatorCall { operator, data },
            ..
        }) = reduction
        else {
            panic!("operator data should claim an Operator/Data call, got {reduction:?}")
        };
        (
            runtime,
            crate::interaction_net::OperatorCall {
                pair,
                operator,
                data,
            },
        )
    }

    fn duplicating_runtime(
        values: &CoreValueFactory,
        payload: Value,
        operator: bool,
    ) -> CoreRuntimeNet {
        let mut builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
        let copies = builder.copy(2);
        if operator {
            let [input, result] = builder.operator(CoreOperator::Applicable(payload));
            builder.wire(copies.input, input);
            let discard = builder.copy(0).input;
            builder.wire(result, discard);
        } else {
            let data = builder.data(payload);
            builder.wire(copies.input, data);
        }
        for output in copies.outputs {
            let discard = builder.copy(0).input;
            builder.wire(output, discard);
        }
        let exposed = builder.data(values.unit());
        values.instantiate_core_net(&builder.finish(exposed))
    }

    #[test]
    fn exact_duplication_deltas_report_one_replaced_and_two_installed_payloads() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let (payload_root, payload) = values.rooted_error_lazy_for_test("duplicated payload");
        let data = duplicating_runtime(&values, Value::Lazy(payload.clone()), false);
        let operator = duplicating_runtime(&values, Value::Lazy(payload), true);
        let data_pair = data.test_with(&values, |runtime| {
            runtime
                .active_pairs()
                .next()
                .expect("Fan/Data pair should be active")
        });
        let operator_pair = operator.test_with(&values, |runtime| {
            runtime
                .active_pairs()
                .next()
                .expect("Fan/Operator pair should be active")
        });
        let probe = values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);

        assert!(matches!(
            data.with_test_access(&values, |runtime| runtime.step_active_pair(data_pair)),
            CoreActivePairStep::Reduction(Reduction {
                kind: crate::interaction_net::ReductionKind::FanData { .. },
                ..
            })
        ));
        assert!(matches!(
            operator.with_test_access(&values, |runtime| runtime.step_active_pair(operator_pair)),
            CoreActivePairStep::Reduction(Reduction {
                kind: crate::interaction_net::ReductionKind::FanOperator { .. },
                ..
            })
        ));

        let records = probe.records();
        assert_eq!(records.len(), 2);
        for record in records {
            assert_eq!(record.leaving_edges(), 1);
            assert_eq!(record.adding_edges(), 2);
        }
        drop((payload_root, data, operator));
    }

    #[test]
    fn exact_call_lowering_deltas_report_copy_and_operator_payloads() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let (old_root, old) = values.rooted_error_lazy_for_test("old callable");
        let (new_root, new) = values.rooted_error_lazy_for_test("new operator");
        let old = Value::Lazy(old);
        let new = Value::Lazy(new);

        let source = values.instantiate_core_net(&closed_unit_template(&values));
        let prepared = source.test_prepare_copy_source(&values);
        let (copy_call, call) = claimed_call(&values, old.clone());
        let (operator_call, operator_call_claim) = claimed_call(&values, old);
        let copy_probe =
            values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);
        copy_call.with_test_access(&values, |runtime| {
            runtime.resume_claimed_call_with_copy(call, prepared);
        });
        let records = copy_probe.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].leaving_edges(), 1);
        assert_eq!(records[0].adding_edges(), 1);

        operator_call.with_test_access(&values, |runtime| {
            runtime.resume_claimed_call_with_operator(
                operator_call_claim,
                CoreOperator::Applicable(new),
            );
        });
        let records = copy_probe.records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].leaving_edges(), 1);
        assert_eq!(records[1].adding_edges(), 1);
        drop((old_root, new_root, source, copy_call, operator_call));
    }

    #[test]
    fn exact_operator_completion_and_stuck_deltas_report_only_changed_payloads() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let (first_root, first) = values.rooted_error_lazy_for_test("operator payload");
        let (second_root, second) = values.rooted_error_lazy_for_test("operator argument");
        let (third_root, third) = values.rooted_error_lazy_for_test("operator result");
        let first = Value::Lazy(first);
        let second = Value::Lazy(second);
        let third = Value::Lazy(third);

        let (operator, call) =
            claimed_operator_call(&values, CoreOperator::Applicable(first), second);
        let (failed, failed_call) = claimed_call(&values, Value::Number(1.into()));
        let completion_probe =
            values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);
        operator.with_test_access(&values, |runtime| {
            runtime.complete_claimed_operator_call(call, OperatorYield::Data(third.clone()));
        });
        let records = completion_probe.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].leaving_edges(), 2);
        assert_eq!(records[0].adding_edges(), 1);

        failed.with_test_access(&values, |runtime| {
            runtime.fail_claimed_call(failed_call, EvaluationHalt::from_value(third));
        });
        let records = completion_probe.records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].leaving_edges(), 0);
        assert_eq!(records[1].adding_edges(), 1);
        drop((first_root, second_root, third_root, operator, failed));
    }

    #[test]
    fn exact_cursor_materialization_delta_reports_only_the_new_payload() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let (payload_root, payload) = values.rooted_error_lazy_for_test("cursor payload");
        let payload = Value::Lazy(payload);
        let mut builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
        let exposed = builder.data(payload);
        let source = values.instantiate_core_net(&builder.finish(exposed));
        let source_copy = source.duplicate_for_test(&values);
        let (target, _, cursor) = CoreRuntimeNet::test_pair_owned_copy_layer(&values, source_copy);
        let pair = target.test_with(&values, |runtime| {
            runtime
                .active_pairs()
                .next()
                .expect("copy cursor pair should be active")
        });
        assert!(matches!(
            target.test_with_optional_mut(&values, |runtime| runtime.reduce_pair(pair)),
            Some(Reduction {
                kind: crate::interaction_net::ReductionKind::RemoteCursor {
                    progress: CursorProgress::Claimed,
                    ..
                },
                ..
            })
        ));
        let probe = values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);

        assert!(matches!(
            target.test_advance_claimed_cursor(&values, cursor),
            Some(CursorProgress::Materialized { .. })
        ));
        let records = probe.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].leaving_edges(), 0);
        assert_eq!(records[0].adding_edges(), 1);
        drop((payload_root, source, target));
    }

    #[test]
    fn cursor_dependency_resolution_reports_exact_removal_and_publishes_only_on_match() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let leaf = values.instantiate_core_net(&closed_unit_template(&values));
        let (source, _) = CoreRuntimeNet::test_copy_layer(&values, leaf);
        let (target, interface) = CoreRuntimeNet::test_copy_layer(&values, source);
        let cursor = match target.test_poll_interface_demand(&values, interface) {
            InterfaceDemand::Cursor(cursor) => cursor,
            demand => panic!("nested copy should demand a cursor, got {demand:?}"),
        };
        let dependency = match target.test_step_cursor(&values, cursor) {
            CoreCursorStep::Dependency(dependency) => dependency,
            step => panic!("nested copy should block on its source, got {step:?}"),
        };
        let before = target.test_with_revisions(&values, |_| ()).1;
        let probe = values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);

        let stale = CoreCursorDependency::LocalCursor(cursor);
        assert_eq!(
            target.with_test_access(&values, |runtime| runtime.resolve_cursor_dependency(
                cursor,
                &stale,
                CursorDependencyDisposition::Progressed,
            )),
            CursorDependencyResolution::Disturbed
        );
        let after_stale = target.test_with_revisions(&values, |_| ()).1;
        assert_eq!(after_stale, before);

        assert_eq!(
            target.with_test_access(&values, |runtime| runtime.resolve_cursor_dependency(
                cursor,
                &dependency,
                CursorDependencyDisposition::Progressed,
            )),
            CursorDependencyResolution::Resolved
        );
        let after = target.test_with_revisions(&values, |_| ()).1;
        assert_eq!(after.topology_revision(), before.topology_revision() + 1);
        assert_eq!(after.disturbance_epoch(), before.disturbance_epoch() + 1);

        let records = probe.records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].leaving_edges(), 0);
        assert_eq!(records[0].adding_edges(), 0);
        assert_eq!(records[1].leaving_edges(), 1);
        assert_eq!(records[1].adding_edges(), 0);
        drop((target, dependency));
    }

    #[test]
    fn final_cursor_join_reports_the_retired_copy_source_once() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let mut source_builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
        let [source_root, left, right] = source_builder.bind();
        source_builder.wire(left, right);
        let source = values.instantiate_core_net(&source_builder.finish(source_root));
        let prepared = source.test_prepare_copy_source(&values);
        let mut target_builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
        let [function, argument, result] = target_builder.bind();
        let [continuation, continuation_argument, exposed] = target_builder.bind();
        let callable = target_builder.data(values.unit());
        let supplied = target_builder.data(Value::Number(1.into()));
        let continued = target_builder.data(Value::Number(2.into()));
        target_builder.wire(function, callable);
        target_builder.wire(argument, supplied);
        target_builder.wire(result, continuation);
        target_builder.wire(continuation_argument, continued);
        let target = values.instantiate_core_net(&target_builder.finish(exposed));
        let call_pair = target.test_with(&values, |runtime| {
            runtime
                .active_pairs()
                .next()
                .expect("call pair should be active")
        });
        let call =
            match target.with_test_access(&values, |runtime| runtime.step_active_pair(call_pair)) {
                CoreActivePairStep::Reduction(Reduction {
                    kind: crate::interaction_net::ReductionKind::Call { bind, data },
                    ..
                }) => crate::interaction_net::Call {
                    pair: call_pair,
                    bind,
                    data,
                },
                step => panic!("target should claim its callable data, got {step:?}"),
            };
        target.with_test_access(&values, |runtime| {
            runtime.resume_claimed_call_with_copy(call, prepared);
        });

        let first_cursor = target
            .test_with_optional_mut(&values, RuntimeNet::reduce_next)
            .and_then(|reduction| match reduction.kind {
                crate::interaction_net::ReductionKind::RemoteCursor {
                    cursor,
                    progress: CursorProgress::Claimed,
                } => Some(cursor),
                _ => None,
            })
            .expect("initial copy cursor should be claimable");
        assert!(matches!(
            target.test_advance_claimed_cursor(&values, first_cursor),
            Some(CursorProgress::Materialized { .. })
        ));
        assert!(matches!(
            target.test_with_optional_mut(&values, RuntimeNet::reduce_next),
            Some(Reduction {
                kind: crate::interaction_net::ReductionKind::BindJoin,
                ..
            })
        ));

        let mut claims = Vec::new();
        for _ in 0..2 {
            let reduction = target
                .test_with_optional_mut(&values, RuntimeNet::reduce_next)
                .expect("each converging cursor should be claimable");
            let crate::interaction_net::ReductionKind::RemoteCursor {
                cursor,
                progress: CursorProgress::Claimed,
            } = reduction.kind
            else {
                panic!("converging cursor should claim, got {reduction:?}")
            };
            claims.push(cursor);
        }
        assert_eq!(
            target.test_advance_claimed_cursor(&values, claims[0]),
            Some(CursorProgress::Blocked)
        );
        let probe = values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);
        assert_eq!(
            target.test_advance_claimed_cursor(&values, claims[1]),
            Some(CursorProgress::Joined)
        );

        let records = probe.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].leaving_edges(), 1);
        assert_eq!(records[0].adding_edges(), 0);
    }

    #[test]
    fn core_net_durable_owner_inventory_is_compile_exhaustive() {
        let _: fn(
            &CoreRuntimeNet,
            &CorePreparedCopySource,
            &CoreNetContention,
            &CoreFrontierObservation,
            &CoreOperator,
        ) = assert_core_net_durable_owner_inventory;
    }

    #[test]
    fn shared_core_net_payload_survives_managed_owner_collection() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let public_values = crate::api::Values::from_core_factory(values.clone());
        let function_runtime = values.instantiate_core_net(&closed_unit_template(&values));
        let code = Arc::new(FunctionCode::new(function_runtime, 1, 0));
        let retained = Arc::downgrade(&code);

        let mut builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
        let [input, result] = builder.operator(CoreOperator::FunctionCaptures {
            code: code.clone(),
            supplied: Arc::from([]),
        });
        let argument = builder.data(values.unit());
        builder.wire(input, argument);
        let runtime = values.instantiate_core_net(&builder.finish(result));
        drop(code);
        let owner = public_values.wrap(Value::Net(crate::core::NetValue::new(runtime)));

        values
            .collect_managed_for_test()
            .expect("a rooted synchronized net should survive collection");
        let retained_code = retained
            .upgrade()
            .expect("the managed net owner must retain its operator payload");
        assert_eq!(
            retained_code.runtime().test_with(&values, |runtime| {
                runtime.interface_data(runtime.exposed()).cloned()
            }),
            Some(values.unit()),
            "an operator's nested function net must be traced through the managed owner"
        );
        drop(retained_code);
        drop(owner);
        values
            .collect_managed_for_test()
            .expect("the retired synchronized net should collect");
        assert!(
            retained.upgrade().is_none(),
            "retiring the managed net owner must retire its operator payload"
        );
    }

    #[test]
    fn prepared_copy_source_is_an_exact_temporary_net_owner() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let baseline = values
            .collect_managed_for_test()
            .expect("the prepared-copy fixture should start collectible");
        let source = values.instantiate_core_net(&closed_unit_template(&values));
        let prepared = source.test_prepare_copy_source(&values);

        let retained = values
            .collect_managed_for_test()
            .expect("the prepared copy source should retain its semantic net");
        assert_eq!(retained.root_entries(), baseline.root_entries() + 1);
        assert_eq!(retained.marked_slots(), baseline.marked_slots() + 1);

        drop(prepared);
        let retired = values
            .collect_managed_for_test()
            .expect("dropping the prepared source should retire its semantic net");
        assert_eq!(retired.root_entries(), baseline.root_entries());
        assert_eq!(retired.finalized_slots(), 1);
    }

    #[test]
    fn frontier_observation_is_an_exact_temporary_net_owner() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let baseline = values
            .collect_managed_for_test()
            .expect("the frontier-observation fixture should start collectible");
        let observation = {
            let source = values.instantiate_core_net(&closed_unit_template(&values));
            let exposed = source.test_with(&values, RuntimeNet::exposed);
            let CorePreparedCopySource { root, .. } = source.test_prepare_copy_source(&values);
            CoreFrontierObservation {
                root,
                observed_topology: 0,
                endpoint: DemandEndpoint::Cursor(exposed.node()),
            }
        };

        let retained = values
            .collect_managed_for_test()
            .expect("the frontier observation should retain its semantic net");
        assert_eq!(retained.root_entries(), baseline.root_entries() + 1);
        assert_eq!(retained.marked_slots(), baseline.marked_slots() + 1);

        drop(observation);
        let retired = values
            .collect_managed_for_test()
            .expect("dropping the frontier observation should retire its semantic net");
        assert_eq!(retired.root_entries(), baseline.root_entries());
        assert_eq!(retired.finalized_slots(), 1);
    }

    #[test]
    #[should_panic(expected = "a live cursor claim cannot cross the core-net facade")]
    fn core_cursor_step_rejects_a_live_claim() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        values.with_runtime_value_access(|access| {
            let _ = CoreCursorStep::from_generic(
                CursorStep::<CoreSpecialization>::Progressed(CursorProgress::Claimed),
                &access,
            );
        });
    }

    #[test]
    #[should_panic(expected = "a live cursor claim cannot cross the core-net facade")]
    fn core_active_pair_step_rejects_a_live_cursor_claim() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let template = closed_unit_template(&values);
        let source = values.instantiate_core_net(&template);
        let (target, _, _) = CoreRuntimeNet::test_pair_owned_copy_layer(&values, source);
        let pair = target.test_with(&values, |runtime| runtime.active_pairs().next().unwrap());
        let reduction = target
            .test_with_optional_mut(&values, |runtime| runtime.reduce_pair(pair))
            .expect("ready cursor pair must be reducible");
        assert!(matches!(
            reduction.kind,
            crate::interaction_net::ReductionKind::RemoteCursor {
                progress: CursorProgress::Claimed,
                ..
            }
        ));

        let _ = CoreActivePairStep::from_generic(ActivePairStep::Reduction(reduction));
    }

    #[test]
    fn core_net_matching_access_reads_its_managed_cell() {
        let first = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let template = closed_unit_template(&first);
        let net = first.instantiate_core_net(&template);

        first.with_runtime_value_access(|access| {
            let net = net.access(&access);
            assert_eq!(
                net.with(|runtime| runtime.interface_data(runtime.exposed()).cloned()),
                Some(first.unit())
            );
        });
    }

    #[test]
    #[should_panic(expected = "does not belong to this heap")]
    fn core_net_access_rejects_a_foreign_runtime() {
        let owner = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let foreign = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let template = closed_unit_template(&owner);
        let net = owner.instantiate_core_net(&template);

        foreign.with_runtime_value_access(|access| {
            let _ = net.access(&access);
        });
    }

    #[test]
    fn identity_only_net_work_outlives_scoped_access() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let template = closed_unit_template(&values);
        let net = values.instantiate_core_net(&template);
        let alias = net.duplicate_for_test(&values);

        values.with_runtime_value_access(|values| {
            let access = net.access(&values);
            let exposed = access.with(RuntimeNet::exposed);
            assert!(access.with(|runtime| runtime.interface_data(exposed).is_some()));
        });

        values.with_runtime_value_access(|access| {
            assert!(net.same_net_in(&alias, &access));
        });
    }

    #[test]
    fn scoped_normalization_batch_closes_and_publishes_once() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let template = closed_unit_template(&values);
        let net = values.instantiate_core_net(&template);
        let initial = net.test_with_revisions(&values, |_| ()).1;

        values.with_runtime_value_access(|values| {
            let access = net.access(&values);
            access
                .with_normalization_batch(|batch| {
                    assert!(thread_has_active_core_normalization_scope());
                    batch.with_mut(|_| ());
                    let during = batch.with_revisions(|_| ()).1;
                    assert!(during.topology_revision() > initial.topology_revision());
                    assert_eq!(
                        during.disturbance_epoch(),
                        initial.disturbance_epoch(),
                        "batch mutation must not publish disturbance before close"
                    );
                    assert!(
                        batch.with_normalization_batch(|_| ()).is_err(),
                        "a competing batch must observe the scoped lease"
                    );
                })
                .expect("first scoped batch must acquire the net");
        });

        assert!(!thread_has_active_core_normalization_scope());
        assert_eq!(net.active_normalization_batch(&values), None);
        let released = net.test_with_revisions(&values, |_| ()).1;
        assert_eq!(
            released.disturbance_epoch(),
            initial.disturbance_epoch() + 1,
            "dirty and contended batch must publish exactly once on close"
        );
    }

    #[test]
    fn scoped_normalization_batch_closes_on_unwind() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let template = closed_unit_template(&values);
        let net = values.instantiate_core_net(&template);

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            values.with_runtime_value_access(|values| {
                let access = net.access(&values);
                let _: Result<(), CoreNetContention> = access.with_normalization_batch(|_| {
                    panic!("forced scoped normalization unwind");
                });
            });
        }));

        assert!(unwind.is_err());
        assert_eq!(net.active_normalization_batch(&values), None);
    }

    #[test]
    fn scoped_normalization_batch_wakes_forced_concurrent_followers() {
        const FOLLOWERS: usize = 4;

        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let template = closed_unit_template(&values);
        let net = values.instantiate_core_net(&template);
        let (leader_ready_tx, leader_ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let leader_values = values.clone();
        let net_root = values.with_runtime_value_access(|access| net.root_in(&access));
        let leader_root = net_root.clone();
        let leader = std::thread::spawn(move || {
            leader_values.with_runtime_value_access(|values| {
                let leader_net = CoreRuntimeNet::from_root(&leader_root, &values);
                let access = leader_net.access(&values);
                access
                    .with_normalization_batch(|_| {
                        leader_ready_tx.send(()).unwrap();
                        release_rx
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .expect("test must release the forced batch leader");
                    })
                    .expect("forced batch leader must acquire the net");
            });
        });

        leader_ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("forced batch leader must publish acquisition");
        let (registered_tx, registered_rx) = std::sync::mpsc::channel();
        let followers = (0..FOLLOWERS)
            .map(|_| {
                let values = values.clone();
                let net_root = net_root.clone();
                let registered_tx = registered_tx.clone();
                std::thread::spawn(move || {
                    let contention = values.with_runtime_value_access(|values| {
                        let net = CoreRuntimeNet::from_root(&net_root, &values);
                        let access = net.access(&values);
                        access
                            .with_normalization_batch(|_| ())
                            .expect_err("leader must retain the normalization batch")
                    });
                    registered_tx.send(()).unwrap();
                    contention.wait_for_disturbance();
                })
            })
            .collect::<Vec<_>>();
        drop(registered_tx);
        for _ in 0..FOLLOWERS {
            registered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("every follower must register before release");
        }

        release_tx.send(()).unwrap();
        leader.join().expect("forced batch leader must finish");
        for follower in followers {
            follower.join().expect("forced batch follower must wake");
        }
        assert_eq!(net.active_normalization_batch(&values), None);
    }

    #[test]
    #[should_panic(expected = "root does not belong to this heap")]
    fn core_copy_source_rejects_a_foreign_runtime() {
        let first = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let second = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let first_template = closed_unit_template(&first);
        let second_template = closed_unit_template(&second);
        let source = first
            .instantiate_core_net(&first_template)
            .test_prepare_copy_source(&first);
        let _target = second.instantiate_core_net(&second_template);

        let _ = source.into_inner_for_factory(&second);
    }

    #[test]
    #[should_panic(expected = "frontier observation requires access to its source net")]
    fn core_frontier_progress_rejects_access_to_another_net() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let leaf = values.instantiate_core_net(&closed_unit_template(&values));
        let (source, _) = CoreRuntimeNet::test_copy_layer(&values, leaf);
        let (target, interface) = CoreRuntimeNet::test_copy_layer(&values, source);
        let cursor = match target.test_poll_interface_demand(&values, interface) {
            InterfaceDemand::Cursor(cursor) => cursor,
            demand => panic!("copy root should expose a cursor, got {demand:?}"),
        };
        let observation = match target.test_step_cursor(&values, cursor) {
            CoreCursorStep::Dependency(CoreCursorDependency::SourceCursor(observation))
            | CoreCursorStep::Dependency(CoreCursorDependency::SourceFrontier(observation)) => {
                observation
            }
            step => panic!("nested copy should expose its source frontier, got {step:?}"),
        };

        target.with_test_access(&values, |wrong_access| match observation.endpoint() {
            DemandEndpoint::Cursor(cursor) => {
                let _ = observation.step_cursor(&wrong_access, cursor);
            }
            DemandEndpoint::ActivePair(pair) => {
                let _ = observation.step_active_pair(&wrong_access, pair);
            }
        });
    }

    #[test]
    fn edge_only_core_net_does_not_retain_the_value_domain() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let domain = Arc::downgrade(values.value_domain());
        let template = closed_unit_template(&values);
        let _net = values.instantiate_core_net(&template);

        drop(values);
        assert!(domain.upgrade().is_none());
    }

    #[test]
    fn core_contention_does_not_retain_the_semantic_net() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let template = closed_unit_template(&values);
        let net = values.instantiate_core_net(&template);
        let contention = values.with_runtime_value_access(|values| {
            let access = net.access(&values);
            access
                .with_normalization_batch(|batch| {
                    batch
                        .with_normalization_batch(|_| ())
                        .expect_err("the outer batch must force contention")
                })
                .expect("the outer batch must acquire the net")
        });

        values
            .collect_managed_for_test()
            .expect("the unrooted semantic net should be reclaimed");
        assert!(
            !contention.inner.wait_for_disturbance(),
            "edge-free contention must observe closure after the semantic net drops"
        );
    }

    #[test]
    fn raw_core_runtime_net_construction_is_confined_to_its_facade() {
        fn visit(root: &Path, path: PathBuf) {
            for entry in fs::read_dir(&path).expect("source directory must be readable") {
                let entry = entry.expect("source entry must be readable");
                let path = entry.path();
                if path.is_dir() {
                    visit(root, path);
                    continue;
                }
                if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                    continue;
                }
                let relative = path
                    .strip_prefix(root)
                    .expect("visited source must remain below source root");
                if relative == Path::new("core_net.rs")
                    || relative == Path::new("interaction_net.rs")
                    || relative.starts_with("interaction_net")
                {
                    continue;
                }
                let source = fs::read_to_string(&path).expect("Rust source must be readable");
                assert!(
                    !source.contains(".instantiate_shared()"),
                    "{} constructs a raw shared interaction net outside the generic runtime or core facade",
                    relative.display()
                );
                assert!(
                    !source.contains(&["SharedRuntime", "Net<CoreSpecialization>"].concat()),
                    "{} names the raw core shared-net owner outside its facade",
                    relative.display()
                );
                assert!(
                    !source.contains("NetContention<CoreSpecialization>"),
                    "{} names raw core-net contention outside its facade",
                    relative.display()
                );
                assert!(
                    !source.contains("NormalizationBatchLease<CoreSpecialization>"),
                    "{} names a raw core normalization lease outside its facade",
                    relative.display()
                );
            }
        }

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        visit(&root, root.clone());
    }

    #[test]
    fn managed_core_net_has_no_legacy_owner() {
        let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let forbidden = [
            ["SharedRuntime", "Net<CoreSpecialization>"].concat(),
            ["Arc<RuntimeNetCell<", "CoreSpecialization>>"].concat(),
            ["Weak<RuntimeNetCell<", "CoreSpecialization>>"].concat(),
            ["Compatibility", "NetEdges"].concat(),
            ["CoreRuntimeNet", "Payload"].concat(),
            ["visit_core_runtime_net", "_edges"].concat(),
            ["adopt_core_net", "_for_test"].concat(),
            ["into_runtime_for_managed", "_test"].concat(),
        ];

        fn visit(source_root: &Path, path: PathBuf, forbidden: &[String]) {
            for entry in fs::read_dir(&path).expect("source directory must be readable") {
                let entry = entry.expect("source entry must be readable");
                let path = entry.path();
                if path.is_dir() {
                    visit(source_root, path, forbidden);
                    continue;
                }
                if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                    continue;
                }

                let source = fs::read_to_string(&path).expect("Rust source must be readable");
                let relative = path
                    .strip_prefix(source_root)
                    .expect("visited source must remain below source root");
                for legacy in forbidden {
                    assert!(
                        !source.contains(legacy),
                        "{} retains obsolete core-net ownership or compatibility surface {legacy}",
                        relative.display()
                    );
                }
            }
        }

        visit(&source_root, source_root.clone(), &forbidden);
    }

    #[test]
    fn durable_core_net_facade_has_no_ordinary_inspection_surface() {
        let source = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("core_net.rs"),
        )
        .expect("core-net source must be readable");
        let facade = source
            .split_once("impl CoreRuntimeNet {")
            .expect("core-net facade implementation must remain present")
            .1
            .split_once("impl CoreRuntimeNetAccess<'_, '_> {")
            .expect("scoped core-net access implementation must remain present")
            .0;

        assert!(facade.contains("pub(crate) fn with_test_access<R>("));
        assert!(facade.contains("values: &CoreValueFactory"));

        for forbidden in [
            "RuntimeValueObserver",
            "pub(crate) fn with<",
            "pub(crate) fn root(",
            "pub(crate) fn belongs_to(",
            "pub(crate) fn domain_is_live(",
            "pub(crate) fn with_mut<",
            "pub(crate) fn poll_interface_demand(",
            "pub(crate) fn resolve_cursor_dependency(",
            "pub(crate) fn step_cursor(",
            "pub(crate) fn step_active_pair(",
            "pub(crate) fn advance_claimed_cursor(",
            "pub(crate) fn prepare_copy_source(",
            "pub(crate) fn resume_claimed_call_with_copy(",
            "pub(crate) fn claim_call(",
            "pub(crate) fn reclaim_blocked_call(",
            "pub(crate) fn resume_claimed_call_with_operator(",
            "pub(crate) fn block_claimed_call(",
            "pub(crate) fn fail_claimed_call(",
            "pub(crate) fn release_claimed_call(",
            "pub(crate) fn restore_blocked_call(",
            "pub(crate) fn claim_operator_call(",
            "pub(crate) fn reclaim_blocked_operator_call(",
            "pub(crate) fn complete_claimed_operator_call(",
            "pub(crate) fn block_claimed_operator_call(",
            "pub(crate) fn fail_claimed_operator_call(",
            "pub(crate) fn release_claimed_operator_call(",
            "pub(crate) fn restore_blocked_operator_call(",
            "pub(crate) fn try_begin_normalization_batch(",
        ] {
            assert!(
                !facade.contains(forbidden),
                "durable core-net facade regained authority-free operation {forbidden}"
            );
        }
    }

    #[test]
    fn managed_core_net_semantic_writers_use_exact_delta_gateways() {
        let core_calls = method_call_inventory("core_net.rs");
        for (writer, gateway) in [
            ("resolve_cursor_dependency", "with_conditional_edge_mut_via"),
            ("resume_claimed_call_with_copy", "with_edge_mut_via"),
            ("resume_claimed_call_with_operator", "with_edge_mut_via"),
            ("fail_claimed_call", "with_edge_mut_via"),
            ("complete_claimed_operator_call", "with_input_edge_mut_via"),
            ("fail_claimed_operator_call", "with_edge_mut_via"),
        ] {
            assert!(
                core_calls
                    .get(writer)
                    .is_some_and(|calls| calls.contains(gateway)),
                "managed core-net writer {writer} lost exact gateway {gateway}"
            );
        }

        let runtime_calls = method_call_inventory("interaction_net/runtime.rs");
        assert!(
            runtime_calls
                .get("step_active_pair_with_gateway")
                .is_some_and(|calls| calls.contains("transition_edges")),
            "active-pair reduction lost its exact edge transition"
        );
        assert!(
            runtime_calls
                .get("finish")
                .is_some_and(|calls| calls.contains("with_input_edge_mut_via")),
            "cursor publication lost its exact edge transition"
        );

        let recursive = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/core/managed/recursive_cells.rs"),
        )
        .expect("managed recursive-cell source must be readable");
        assert!(
            !recursive.contains("fn trace_core_runtime_net("),
            "the retired whole-net mutation bridge returned"
        );
    }
}
