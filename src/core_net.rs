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
#[cfg(feature = "glam-prof")]
use crate::interaction_net::ReductionKind;
#[cfg(test)]
use crate::interaction_net::RuntimeNetRevisions;
use crate::interaction_net::{
    ActivePairKey, ActivePairStep, CursorDependency, CursorDependencyDisposition,
    CursorDependencyResolution, CursorProgress, CursorStep, DemandEndpoint, FrontierObservation,
    InteractionNet, InterfaceDemand, InterfaceRoute, NetContention, NodeId, OperatorYield, Port,
    PreparedCopySource, Reduction, RuntimeNet, RuntimeNetMutation, RuntimeNetPayloadDuplicator,
    SourceFrontier,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreDataKey {
    Key(Key),
    Index,
    PathIndex,
}

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

impl CoreOperator {
    pub(crate) fn duplicate_in(&self, access: &RuntimeValueAccess<'_>) -> Self {
        match self {
            Self::ApplyArity { arity, supplied } => Self::ApplyArity {
                arity: *arity,
                supplied: Arc::clone(supplied),
            },
            Self::FunctionCaptures { code, supplied } => Self::FunctionCaptures {
                code: Arc::clone(code),
                supplied: Arc::clone(supplied),
            },
            Self::ComputationCaptures { code, supplied } => Self::ComputationCaptures {
                code: Arc::clone(code),
                supplied: Arc::clone(supplied),
            },
            Self::Dict { keys, supplied } => Self::Dict {
                keys: Arc::clone(keys),
                supplied: Arc::clone(supplied),
            },
            Self::Builtin(call) => Self::Builtin(call.duplicate_in(access)),
            Self::Applicable(value) => Self::Applicable(access.duplicate_value(value)),
            Self::List { arity, supplied } => Self::List {
                arity: *arity,
                supplied: Arc::clone(supplied),
            },
            Self::Access { path, supplied } => Self::Access {
                path: Arc::clone(path),
                supplied: Arc::clone(supplied),
            },
            Self::Request {
                tag,
                arity,
                supplied,
                wrap_effect,
            } => Self::Request {
                tag: tag.clone(),
                arity: *arity,
                supplied: Arc::clone(supplied),
                wrap_effect: *wrap_effect,
            },
        }
    }
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

impl RuntimeNetPayloadDuplicator<CoreSpecialization> for CoreRuntimeNetAccess<'_, '_> {
    #[inline(always)]
    fn duplicate_data(&self, data: &Value) -> Value {
        self.values.duplicate_value(data)
    }

    #[inline(always)]
    fn duplicate_operator(&self, operator: &CoreOperator) -> CoreOperator {
        operator.duplicate_in(self.values)
    }
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
                .construct_managed_core_net(template.instantiate_with(&access))
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
    pub(crate) fn test_reduce_pair(
        &self,
        values: &CoreValueFactory,
        pair: ActivePairKey,
    ) -> Option<Reduction> {
        self.with_test_access(values, |access| access.reduce_pair_for_test(pair))
    }

    #[cfg(test)]
    pub(crate) fn test_poll_interface_demand(
        &self,
        values: &CoreValueFactory,
        interface: Port,
    ) -> InterfaceDemand {
        self.with_test_access(values, |access| {
            access.poll_interface_demand(interface, &mut InterfaceRoute::default())
        })
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
    ) -> TestPreparedCopySource {
        self.with_test_access(values, |access| TestPreparedCopySource {
            root: access.owner.root_in(access.values),
            remote: access.runtime.with(RuntimeNet::exposed),
        })
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
    #[cfg(feature = "glam-prof")]
    fn record_reduction(&self, kind: &ReductionKind) {
        use crate::interaction_net::CursorProgress;
        use crate::interaction_net::profiling::ReductionEvent;

        let event = match kind {
            ReductionKind::BindJoin => Some(ReductionEvent::BindJoin),
            ReductionKind::FanJoin { .. } => Some(ReductionEvent::FanJoin),
            ReductionKind::FanCommute { .. } => Some(ReductionEvent::FanCommute),
            ReductionKind::FanData { .. } => Some(ReductionEvent::FanData),
            ReductionKind::FanBind { .. } => Some(ReductionEvent::FanBind),
            ReductionKind::FanOperator { .. } => Some(ReductionEvent::FanOperator),
            ReductionKind::Erase => Some(ReductionEvent::Erase),
            ReductionKind::RemoteCursor {
                progress: CursorProgress::Materialized { .. },
                ..
            } => Some(ReductionEvent::CursorMaterialized),
            ReductionKind::RemoteCursor {
                progress: CursorProgress::Joined,
                ..
            } => Some(ReductionEvent::CursorJoined),
            ReductionKind::Call { .. }
            | ReductionKind::CallableCheckpoint { .. }
            | ReductionKind::OperatorCall { .. }
            | ReductionKind::RemoteCursor {
                progress: CursorProgress::Claimed | CursorProgress::Blocked,
                ..
            }
            | ReductionKind::Stuck => None,
        };
        if let Some(event) = event {
            self.values
                .values()
                .interaction_net_profile()
                .record_reduction(event);
        }
    }

    #[cfg(feature = "glam-prof")]
    pub(crate) fn record_driver(&self, event: crate::interaction_net::profiling::DriverEvent) {
        self.values
            .values()
            .interaction_net_profile()
            .record_driver(event);
    }

    #[cfg(all(test, feature = "glam-prof"))]
    pub(crate) fn driver_work_item_limit_reached(&self) -> bool {
        self.values
            .values()
            .interaction_net_profile()
            .driver_work_item_limit_reached()
    }

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
    pub(crate) fn reduce_pair_for_test(&self, pair: ActivePairKey) -> Option<Reduction> {
        self.runtime
            .cell()
            .with_optional_mut_via(&self.runtime, |runtime| {
                runtime.reduce_pair_with_gateway(pair, &self.runtime)
            })
    }

    #[cfg(test)]
    pub(crate) fn reduce_next_for_test(&self) -> Option<Reduction> {
        let pair = self.runtime.with(RuntimeNet::next_ready_pair)?;
        self.reduce_pair_for_test(pair)
    }

    pub(crate) fn poll_interface_demand(
        &self,
        interface: Port,
        route: &mut InterfaceRoute,
    ) -> InterfaceDemand {
        self.runtime
            .cell()
            .with_conditional_mut_via(&self.runtime, |runtime| {
                runtime.poll_interface_demand(interface, route)
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
                runtime.resolve_cursor_dependency_edge_transition_with_gateway(
                    cursor,
                    &expected.to_generic(self.values),
                    &self.runtime,
                )
            },
            |runtime| {
                let resolution = runtime.resolve_cursor_dependency_with_gateway(
                    cursor,
                    &expected.to_generic(self.values),
                    disposition,
                    &self.runtime,
                );
                if resolution == CursorDependencyResolution::Resolved {
                    RuntimeNetMutation::Changed(resolution)
                } else {
                    RuntimeNetMutation::Unchanged(resolution)
                }
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn step_cursor(&self, cursor: NodeId) -> CoreCursorStep {
        self.step_cursor_within(cursor, |_| true)
    }

    #[cfg(test)]
    pub(crate) fn step_active_pair(&self, pair: ActivePairKey) -> CoreActivePairStep {
        self.step_active_pair_within(pair, |_| true)
    }

    /// Steps `cursor`, calling `admit` only if it would claim (see
    /// `RuntimeNetCell::step_cursor_with_gateway`).
    pub(crate) fn step_cursor_within(
        &self,
        cursor: NodeId,
        admit: impl FnOnce(crate::interaction_net::ClaimKind) -> bool,
    ) -> CoreCursorStep {
        self.step_cursor_if_current(cursor, None, admit)
    }

    /// Steps `pair`, calling `admit` only if it would claim (see
    /// `RuntimeNetCell::step_active_pair_with_gateway`).
    pub(crate) fn step_active_pair_within(
        &self,
        pair: ActivePairKey,
        admit: impl FnOnce(crate::interaction_net::ClaimKind) -> bool,
    ) -> CoreActivePairStep {
        self.step_active_pair_if_current(pair, None, admit)
    }

    fn inspect_source_frontier(
        &self,
        source: &CoreRuntimeNet,
        anchor: Port,
    ) -> SourceFrontier<CoreSpecialization> {
        let source = source.access(self.values);
        source.runtime.cell().inspect_source_frontier(
            source.owner.duplicate_in(self.values),
            anchor,
            &source,
        )
    }

    // The cursor and pair steps stay out of line, with the cell's steps
    // inlined into them, so that changes inside net operations do not move
    // the inlining of the evaluator's driver loop around them.
    #[inline(never)]
    fn step_cursor_if_current(
        &self,
        cursor: NodeId,
        expected_topology_revision: Option<u64>,
        admit: impl FnOnce(crate::interaction_net::ClaimKind) -> bool,
    ) -> CoreCursorStep {
        let step = self.runtime.cell().step_cursor_with_gateway(
            cursor,
            expected_topology_revision,
            &self.runtime,
            admit,
            |source, anchor| self.inspect_source_frontier(source, anchor),
        );
        #[cfg(feature = "glam-prof")]
        if let CursorStep::Progressed(progress) = &step {
            let kind = ReductionKind::RemoteCursor {
                cursor,
                progress: *progress,
            };
            self.record_reduction(&kind);
        }
        CoreCursorStep::from_generic(step, self.values)
    }

    // Out of line, as `step_cursor_if_current` is.
    #[inline(never)]
    fn step_active_pair_if_current(
        &self,
        pair: ActivePairKey,
        expected_topology_revision: Option<u64>,
        admit: impl FnOnce(crate::interaction_net::ClaimKind) -> bool,
    ) -> CoreActivePairStep {
        let step = self.runtime.cell().step_active_pair_with_gateway(
            pair,
            expected_topology_revision,
            &self.runtime,
            admit,
            |source, anchor| self.inspect_source_frontier(source, anchor),
        );
        #[cfg(feature = "glam-prof")]
        if let ActivePairStep::Reduction(reduction) = &step {
            self.record_reduction(&reduction.kind);
        }
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
        source: CorePreparedCopySource<'_>,
    ) {
        let source = source.into_inner();
        let whole = source.whole_len();
        self.runtime.cell().with_edge_mut_via(
            &self.runtime,
            |runtime| runtime.resume_call_with_copy_edge_transition(call, whole),
            |runtime| runtime.resume_claimed_call_with_copy(call, source),
        );
        #[cfg(feature = "glam-prof")]
        self.values
            .values()
            .interaction_net_profile()
            .record_reduction(crate::interaction_net::profiling::ReductionEvent::Call);
    }

    pub(crate) fn claim_call(&self, call: crate::interaction_net::Call) -> Option<Value> {
        self.runtime
            .cell()
            .with(|runtime| runtime.claim_call(call, self))
    }

    pub(crate) fn install_claimed_call_checkpoint(
        &self,
        call: crate::interaction_net::Call,
        state: crate::eval::whnf::NetWhnfState,
    ) -> Result<crate::interaction_net::CallableCheckpointCall, Box<crate::eval::whnf::NetWhnfState>>
    {
        let mut state = Some(Box::new(state));
        let result = self.runtime.cell().with_conditional_edge_mut_via(
            &self.runtime,
            |runtime| runtime.install_call_checkpoint_edge_transition(call),
            |runtime| match runtime.install_claimed_call_checkpoint(
                call,
                state
                    .take()
                    .expect("checkpoint input is consumed exactly once"),
            ) {
                Ok(call) => RuntimeNetMutation::Changed(Ok(call)),
                Err(state) => RuntimeNetMutation::Unchanged(Err(state)),
            },
        );
        #[cfg(feature = "glam-prof")]
        if result.is_ok() {
            self.values.values().record_net_driver(
                crate::interaction_net::profiling::DriverEvent::CallableCheckpointInstall,
            );
        }
        result
    }

    pub(crate) fn take_claimed_callable_checkpoint(
        &self,
        call: crate::interaction_net::CallableCheckpointCall,
    ) -> Option<crate::eval::whnf::NetWhnfState> {
        let result = self.runtime.cell().with_conditional_edge_mut_via(
            &self.runtime,
            |runtime| runtime.take_checkpoint_edge_transition(call),
            |runtime| match runtime.take_claimed_callable_checkpoint(call) {
                Some(state) => RuntimeNetMutation::Changed(Some(*state)),
                None => RuntimeNetMutation::Unchanged(None),
            },
        );
        #[cfg(feature = "glam-prof")]
        if result.is_some() {
            self.values.values().record_net_driver(
                crate::interaction_net::profiling::DriverEvent::CallableCheckpointResumption,
            );
        }
        result
    }

    pub(crate) fn callable_checkpoint(
        &self,
        pair: ActivePairKey,
    ) -> Option<crate::interaction_net::CallableCheckpointCall> {
        self.runtime
            .cell()
            .with(|runtime| runtime.callable_checkpoint(pair))
    }

    pub(crate) fn restore_claimed_callable_checkpoint(
        &self,
        call: crate::interaction_net::CallableCheckpointCall,
        state: crate::eval::whnf::NetWhnfState,
    ) -> Result<(), Box<crate::eval::whnf::NetWhnfState>> {
        let mut state = Some(Box::new(state));
        let restored = self.runtime.cell().with_cleanup_edge_mut_via(
            &self.runtime,
            |runtime| runtime.publish_checkpoint_edge_transition(call),
            |runtime| match runtime.restore_claimed_callable_checkpoint(
                call,
                state
                    .take()
                    .expect("checkpoint input is consumed exactly once"),
            ) {
                Ok(()) => RuntimeNetMutation::Changed(Ok(())),
                Err(state) => RuntimeNetMutation::Unchanged(Err(state)),
            },
        );
        restored.unwrap_or_else(|| {
            Err(state
                .take()
                .expect("a skipped restore leaves its checkpoint input unconsumed"))
        })
    }

    pub(crate) fn replace_claimed_callable_checkpoint(
        &self,
        call: crate::interaction_net::CallableCheckpointCall,
        state: crate::eval::whnf::NetWhnfState,
    ) -> Result<crate::interaction_net::CallableCheckpointCall, Box<crate::eval::whnf::NetWhnfState>>
    {
        let mut state = Some(Box::new(state));
        let result = self.runtime.cell().with_conditional_edge_mut_via(
            &self.runtime,
            |runtime| runtime.publish_checkpoint_edge_transition(call),
            |runtime| match runtime.replace_claimed_callable_checkpoint(
                call,
                state
                    .take()
                    .expect("checkpoint input is consumed exactly once"),
            ) {
                Ok(call) => RuntimeNetMutation::Changed(Ok(call)),
                Err(state) => RuntimeNetMutation::Unchanged(Err(state)),
            },
        );
        #[cfg(feature = "glam-prof")]
        if result.is_ok() {
            self.values.values().record_net_driver(
                crate::interaction_net::profiling::DriverEvent::CallableCheckpointReplacement,
            );
        }
        result
    }

    pub(crate) fn resume_claimed_checkpoint_with_copy(
        &self,
        call: crate::interaction_net::CallableCheckpointCall,
        source: CorePreparedCopySource<'_>,
    ) {
        let source = source.into_inner();
        let whole = source.whole_len();
        self.runtime.cell().with_edge_mut_via(
            &self.runtime,
            |runtime| runtime.resume_checkpoint_with_copy_edge_transition(call, whole),
            |runtime| runtime.resume_claimed_checkpoint_with_copy(call, source),
        );
        #[cfg(feature = "glam-prof")]
        self.values
            .values()
            .interaction_net_profile()
            .record_reduction(crate::interaction_net::profiling::ReductionEvent::Call);
        #[cfg(feature = "glam-prof")]
        self.values.values().record_net_driver(
            crate::interaction_net::profiling::DriverEvent::CallableCheckpointTerminalization,
        );
    }

    pub(crate) fn resume_claimed_checkpoint_with_operator(
        &self,
        call: crate::interaction_net::CallableCheckpointCall,
        operator: CoreOperator,
    ) {
        self.runtime.cell().with_edge_mut_via(
            &self.runtime,
            |runtime| runtime.resume_checkpoint_with_operator_edge_transition(call),
            |runtime| runtime.resume_claimed_checkpoint_with_operator(call, operator),
        );
        #[cfg(feature = "glam-prof")]
        self.values
            .values()
            .interaction_net_profile()
            .record_reduction(crate::interaction_net::profiling::ReductionEvent::Call);
        #[cfg(feature = "glam-prof")]
        self.values.values().record_net_driver(
            crate::interaction_net::profiling::DriverEvent::CallableCheckpointTerminalization,
        );
    }

    pub(crate) fn block_callable_checkpoint(
        &self,
        call: crate::interaction_net::CallableCheckpointCall,
        wait: CoreWaitToken,
    ) -> crate::interaction_net::CheckpointBlockResult {
        let result = self
            .runtime
            .cell()
            .with_conditional_mut_via(&self.runtime, |runtime| {
                let result = runtime.block_callable_checkpoint(call, wait);
                if result == crate::interaction_net::CheckpointBlockResult::Blocked {
                    RuntimeNetMutation::Changed(result)
                } else {
                    RuntimeNetMutation::Unchanged(result)
                }
            });
        #[cfg(feature = "glam-prof")]
        self.values.values().record_net_driver(match result {
            crate::interaction_net::CheckpointBlockResult::Blocked => {
                crate::interaction_net::profiling::DriverEvent::CallableCheckpointDependencyBlock
            }
            crate::interaction_net::CheckpointBlockResult::Disturbed => {
                crate::interaction_net::profiling::DriverEvent::CallableCheckpointStaleAdmission
            }
        });
        result
    }

    pub(crate) fn retry_blocked_callable_checkpoint(
        &self,
        blocked: &crate::interaction_net::BlockedCallableCheckpoint<CoreWaitToken>,
    ) -> bool {
        let result = self
            .runtime
            .cell()
            .with_conditional_mut_via(&self.runtime, |runtime| {
                if runtime.retry_blocked_callable_checkpoint(blocked) {
                    RuntimeNetMutation::Changed(true)
                } else {
                    RuntimeNetMutation::Unchanged(false)
                }
            });
        #[cfg(feature = "glam-prof")]
        if result {
            self.values.values().record_net_driver(
                crate::interaction_net::profiling::DriverEvent::CallableCheckpointDependencyRetry,
            );
        }
        result
    }

    pub(crate) fn fail_blocked_callable_checkpoint(
        &self,
        blocked: &crate::interaction_net::BlockedCallableCheckpoint<CoreWaitToken>,
        error: EvaluationHalt,
    ) -> Result<crate::eval::whnf::NetWhnfState, EvaluationHalt> {
        let mut error = Some(error);
        let result = self.runtime.cell().with_conditional_edge_mut_via(
            &self.runtime,
            |runtime| runtime.fail_published_checkpoint_edge_transition(blocked.call),
            |runtime| match runtime.fail_blocked_callable_checkpoint(
                blocked,
                error
                    .take()
                    .expect("checkpoint error is consumed exactly once"),
            ) {
                Ok(state) => RuntimeNetMutation::Changed(Ok(*state)),
                Err(error) => RuntimeNetMutation::Unchanged(Err(error)),
            },
        );
        #[cfg(feature = "glam-prof")]
        if result.is_ok() {
            self.values.values().record_net_driver(
                crate::interaction_net::profiling::DriverEvent::CallableCheckpointTerminalization,
            );
        }
        result
    }

    pub(crate) fn fail_claimed_callable_checkpoint(
        &self,
        call: crate::interaction_net::CallableCheckpointCall,
        error: EvaluationHalt,
    ) -> Result<(), EvaluationHalt> {
        let mut error = Some(error);
        let result = self.runtime.cell().with_conditional_edge_mut_via(
            &self.runtime,
            |runtime| {
                runtime.fail_call_edge_transition(crate::interaction_net::Call {
                    pair: call.pair,
                    bind: call.bind,
                    data: call.checkpoint,
                })
            },
            |runtime| match runtime.fail_claimed_callable_checkpoint(
                call,
                error
                    .take()
                    .expect("checkpoint error is consumed exactly once"),
            ) {
                Ok(()) => RuntimeNetMutation::Changed(Ok(())),
                Err(error) => RuntimeNetMutation::Unchanged(Err(error)),
            },
        );
        #[cfg(feature = "glam-prof")]
        if result.is_ok() {
            self.values.values().record_net_driver(
                crate::interaction_net::profiling::DriverEvent::CallableCheckpointTerminalization,
            );
        }
        result
    }

    pub(crate) fn fail_published_callable_checkpoint(
        &self,
        call: crate::interaction_net::CallableCheckpointCall,
        error: EvaluationHalt,
    ) -> Result<(), EvaluationHalt> {
        let mut error = Some(error);
        let result = self.runtime.cell().with_conditional_edge_mut_via(
            &self.runtime,
            |runtime| runtime.fail_published_checkpoint_edge_transition(call),
            |runtime| match runtime.fail_published_callable_checkpoint(
                call,
                error
                    .take()
                    .expect("checkpoint error is consumed exactly once"),
            ) {
                Ok(()) => RuntimeNetMutation::Changed(Ok(())),
                Err(error) => RuntimeNetMutation::Unchanged(Err(error)),
            },
        );
        #[cfg(feature = "glam-prof")]
        self.values.values().record_net_driver(if result.is_ok() {
            crate::interaction_net::profiling::DriverEvent::CallableCheckpointTerminalization
        } else {
            crate::interaction_net::profiling::DriverEvent::CallableCheckpointStaleAdmission
        });
        result
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
        #[cfg(feature = "glam-prof")]
        self.values
            .values()
            .interaction_net_profile()
            .record_reduction(crate::interaction_net::profiling::ReductionEvent::Call);
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
            .with_cleanup_mut_via(&self.runtime, |runtime| {
                if runtime.release_claimed_call(call) {
                    RuntimeNetMutation::Changed(true)
                } else {
                    RuntimeNetMutation::Unchanged(false)
                }
            })
            .unwrap_or(false)
    }

    pub(crate) fn claim_operator_call(
        &self,
        call: crate::interaction_net::OperatorCall,
    ) -> Option<(CoreOperator, Value)> {
        self.runtime
            .cell()
            .with(|runtime| runtime.claim_operator_call(call, self))
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
        #[cfg(feature = "glam-prof")]
        self.values
            .values()
            .interaction_net_profile()
            .record_reduction(crate::interaction_net::profiling::ReductionEvent::OperatorCall);
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
            .with_cleanup_mut_via(&self.runtime, |runtime| {
                if runtime.release_claimed_operator_call(call) {
                    RuntimeNetMutation::Changed(true)
                } else {
                    RuntimeNetMutation::Unchanged(false)
                }
            })
            .unwrap_or(false)
    }
}

impl<'scope> CoreRuntimeNetAccess<'_, 'scope> {
    /// Prepares this net as the source of a copy installed in the same
    /// access: whole if no evaluation is left in it, since a copy then has
    /// nothing to share, and otherwise through a remote cursor.
    pub(crate) fn prepare_copy_source(&self) -> CorePreparedCopySource<'scope> {
        let copy = match self.runtime.with(|runtime| runtime.whole_copy(self)) {
            Some(copy) => PreparedCopySource::Whole(Box::new(copy)),
            None => PreparedCopySource::new(
                self.owner.duplicate_in(self.values),
                self.runtime.with(RuntimeNet::exposed),
            ),
        };
        CorePreparedCopySource {
            copy,
            _access: std::marker::PhantomData,
        }
    }
}

/// A copy source prepared inside one access for a copy installed in that
/// same access. Its net edge, or a whole copy's payloads, do not root what
/// they reach: no collection runs inside an access, and the copy layer that
/// consumes them traces them.
pub(crate) struct CorePreparedCopySource<'scope> {
    copy: PreparedCopySource<CoreSpecialization>,
    _access: std::marker::PhantomData<&'scope ()>,
}

impl CorePreparedCopySource<'_> {
    fn into_inner(self) -> PreparedCopySource<CoreSpecialization> {
        self.copy
    }
}

/// A rooted copy source, for tests that prepare a copy in one access and
/// install it in another.
#[cfg(test)]
pub(crate) struct TestPreparedCopySource {
    root: ManagedCoreNetRoot,
    remote: Port,
}

#[cfg(test)]
impl TestPreparedCopySource {
    fn in_access<'scope>(
        &self,
        target: &RuntimeValueAccess<'scope>,
    ) -> CorePreparedCopySource<'scope> {
        CorePreparedCopySource {
            copy: PreparedCopySource::new(
                CoreRuntimeNet::from_root(&self.root, target),
                self.remote,
            ),
            _access: std::marker::PhantomData,
        }
    }

    fn into_inner_for_factory(
        self,
        target: &CoreValueFactory,
    ) -> (PreparedCopySource<CoreSpecialization>, ManagedCoreNetRoot) {
        let prepared =
            target.with_runtime_value_access(|access| self.in_access(&access).into_inner());
        (prepared, self.root)
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

pub(crate) struct CoreFrontierObservation {
    source: CoreRuntimeNet,
    observed_topology: u64,
    endpoint: DemandEndpoint,
}

impl CoreFrontierObservation {
    pub(crate) fn duplicate_in(&self, access: &RuntimeValueAccess<'_>) -> Self {
        Self {
            source: self.source.duplicate_in(access),
            observed_topology: self.observed_topology,
            endpoint: self.endpoint,
        }
    }

    fn from_generic(
        inner: FrontierObservation<CoreSpecialization>,
        access: &RuntimeValueAccess<'_>,
    ) -> Self {
        Self {
            source: inner.source().duplicate_in(access),
            observed_topology: inner.observed_topology_revision(),
            endpoint: inner.endpoint(),
        }
    }

    pub(crate) fn source(&self, access: &RuntimeValueAccess<'_>) -> CoreRuntimeNet {
        self.source.duplicate_in(access)
    }

    pub(crate) fn endpoint(&self) -> DemandEndpoint {
        self.endpoint
    }

    pub(crate) fn retained_source(&self) -> &CoreRuntimeNet {
        &self.source
    }

    pub(crate) fn trace_managed_edges(&self, visitor: &mut glam_gc::Visitor<'_>) {
        self.source.trace_managed_edge(visitor);
    }

    pub(crate) fn step_active_pair(
        &self,
        access: &CoreRuntimeNetAccess<'_, '_>,
        pair: ActivePairKey,
        admit: impl FnOnce(crate::interaction_net::ClaimKind) -> bool,
    ) -> CoreActivePairStep {
        assert!(
            self.source(access.values)
                .same_net_in(access.owner, access.values),
            "frontier observation requires access to its source net"
        );
        access.step_active_pair_if_current(pair, Some(self.observed_topology), admit)
    }

    pub(crate) fn step_cursor(
        &self,
        access: &CoreRuntimeNetAccess<'_, '_>,
        cursor: NodeId,
        admit: impl FnOnce(crate::interaction_net::ClaimKind) -> bool,
    ) -> CoreCursorStep {
        assert!(
            self.source(access.values)
                .same_net_in(access.owner, access.values),
            "frontier observation requires access to its source net"
        );
        access.step_cursor_if_current(cursor, Some(self.observed_topology), admit)
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

pub(crate) enum CoreCursorDependency {
    LocalCursor(NodeId),
    SourceCursor(CoreFrontierObservation),
    SourceFrontier(CoreFrontierObservation),
}

impl CoreCursorDependency {
    pub(crate) fn duplicate_in(&self, access: &RuntimeValueAccess<'_>) -> Self {
        match self {
            Self::LocalCursor(cursor) => Self::LocalCursor(*cursor),
            Self::SourceCursor(observation) => Self::SourceCursor(observation.duplicate_in(access)),
            Self::SourceFrontier(observation) => {
                Self::SourceFrontier(observation.duplicate_in(access))
            }
        }
    }

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

    pub(crate) fn trace_managed_edges(&self, visitor: &mut glam_gc::Visitor<'_>) {
        match self {
            Self::LocalCursor(_) => {}
            Self::SourceCursor(observation) | Self::SourceFrontier(observation) => {
                observation.trace_managed_edges(visitor);
            }
        }
    }
}

pub(crate) enum CoreCursorStep {
    Progressed(CursorProgress),
    Dependency(CoreCursorDependency),
    Stable,
    Contended(CoreNetContention),
    Disturbed,
    Gone,
    NotAdmitted,
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
            CursorStep::NotAdmitted => Self::NotAdmitted,
        }
    }
}

pub(crate) enum CoreActivePairStep {
    Reduction(Reduction),
    Cursor(NodeId),
    BlockedCallableCheckpoint(crate::interaction_net::BlockedCallableCheckpoint<CoreWaitToken>),
    Stuck,
    Contended(CoreNetContention),
    Disturbed,
    Gone,
    NotAdmitted,
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
            ActivePairStep::BlockedCallableCheckpoint(blocked) => {
                Self::BlockedCallableCheckpoint(blocked)
            }
            ActivePairStep::Stuck(_) => Self::Stuck,
            ActivePairStep::Contended(contention) => {
                Self::Contended(CoreNetContention::new(contention))
            }
            ActivePairStep::Disturbed => Self::Disturbed,
            ActivePairStep::Gone => Self::Gone,
            ActivePairStep::NotAdmitted => Self::NotAdmitted,
        }
    }
}

// These pin the current observer-free representation sizes, not ABI
// promises. A frontier observation is one traceable net edge plus
// only its scalar operation snapshot; a prepared copy source is one net
// edge plus its exposed port, or one boxed whole copy.
#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
const _: () = {
    assert!(std::mem::size_of::<CorePreparedCopySource<'static>>() == 16);
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
        prepared: &CorePreparedCopySource<'_>,
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

        let CorePreparedCopySource { copy, _access } = prepared;
        match copy {
            PreparedCopySource::Cursor { source, remote } => {
                let _: &CoreRuntimeNet = source;
                let _: &Port = remote;
            }
            PreparedCopySource::Whole(copy) => {
                let _: &crate::interaction_net::WholeCopy<CoreSpecialization> = copy;
            }
        }

        let CoreNetContention { inner } = contention;
        let _: &NetContention = inner;

        let CoreFrontierObservation {
            source,
            observed_topology,
            endpoint,
        } = observation;
        let _: &CoreRuntimeNet = source;
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
        values.with_runtime_value_access(|access| {
            let mut builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
            let data = builder.data(access.unit());
            builder.finish(data)
        })
    }

    #[cfg(feature = "glam-prof")]
    fn two_bind_join_template(values: &CoreValueFactory) -> CoreInteractionNet {
        values.with_runtime_value_access(|access| {
            let mut builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
            for _ in 0..2 {
                let left = builder.bind();
                let right = builder.bind();
                builder.wire(left[0], right[0]);
                // Each bind's consuming auxiliary takes data and its providing
                // one is erased, so every pair is a polarized application.
                for [consumes, provides] in [[left[1], left[2]], [right[1], right[2]]] {
                    let data = builder.data(access.unit());
                    builder.wire(consumes, data);
                    let erase = builder.copy(0).input;
                    builder.wire(provides, erase);
                }
            }
            let exposed = builder.data(access.unit());
            // The two join pairs are independent work beside the exposed value.
            builder.disconnected_for_test().finish(exposed)
        })
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
            panic!("callable data should claim a Bind/Data call")
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
            panic!("operator data should claim an Operator/Data call")
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
        let exposed = builder.data(values.with_runtime_value_access(|access| access.unit()));
        // The duplication pair is independent work beside the exposed value.
        values.instantiate_core_net(&builder.disconnected_for_test().finish(exposed))
    }

    #[test]
    fn exact_duplication_deltas_report_one_replaced_and_two_installed_payloads() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let (payload_root, payload) = values.rooted_error_lazy_for_test("duplicated payload");
        let data = duplicating_runtime(
            &values,
            Value::Lazy(payload.duplicate_for_test(&values)),
            false,
        );
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
        let (copy_call, call) = claimed_call(&values, old.duplicate_for_test(&values));
        let (operator_call, operator_call_claim) = claimed_call(&values, old);
        let copy_probe =
            values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);
        copy_call.with_test_access(&values, |runtime| {
            runtime.resume_claimed_call_with_copy(call, prepared.in_access(runtime.values));
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

    /// A source with no evaluation left is copied whole, so the copy reports
    /// each payload it installs rather than an edge to the source.
    #[test]
    fn whole_copies_report_each_copied_payload() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let (callable_root, callable) = values.rooted_error_lazy_for_test("callable");
        let (data_root, data) = values.rooted_error_lazy_for_test("copied data");
        let (operator_root, operator) = values.rooted_error_lazy_for_test("copied operator");

        // A bind holding data and an operator whose result is erased: no
        // active pair is left.
        let mut builder = crate::interaction_net::NetBuilder::<CoreSpecialization>::new();
        let [root, argument, result] = builder.bind();
        let data = builder.data(Value::Lazy(data));
        let [input, output] = builder.operator(CoreOperator::Applicable(Value::Lazy(operator)));
        let erase = builder.push(crate::interaction_net::Node::Erase);
        builder.wire(argument, data);
        builder.wire(result, input);
        builder.wire(output, Port::principal(erase));
        let source = values.instantiate_core_net(&builder.finish(root));

        let (caller, call) = claimed_call(&values, Value::Lazy(callable));
        let probe = values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);
        caller.with_test_access(&values, |runtime| {
            let prepared = source.access(runtime.values).prepare_copy_source();
            assert_eq!(prepared.copy.whole_len(), Some(4));
            runtime.resume_claimed_call_with_copy(call, prepared);
        });
        let records = probe.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].leaving_edges(), 1);
        assert_eq!(records[0].adding_edges(), 2);
        drop((callable_root, data_root, operator_root, source, caller));
    }

    #[cfg(feature = "glam-prof")]
    #[test]
    fn profiling_classifies_each_committed_rule_family_exactly_once() {
        use crate::interaction_net::profiling::NetReductionCounts;

        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let runtime = values.instantiate_core_net(&closed_unit_template(&values));
        let fan = crate::interaction_net::FanIdentity::for_test(1);
        let other_fan = crate::interaction_net::FanIdentity::for_test(2);
        let node = runtime.test_with(&values, |runtime| runtime.exposed().node());
        runtime.with_test_access(&values, |runtime| {
            for kind in [
                ReductionKind::BindJoin,
                ReductionKind::FanJoin {
                    identity: fan.clone(),
                },
                ReductionKind::FanCommute {
                    left: fan.clone(),
                    right: other_fan,
                },
                ReductionKind::FanData {
                    identity: fan.clone(),
                },
                ReductionKind::FanBind {
                    identity: fan.clone(),
                },
                ReductionKind::FanOperator { identity: fan },
                ReductionKind::Erase,
                ReductionKind::RemoteCursor {
                    cursor: node,
                    progress: CursorProgress::Materialized { node },
                },
                ReductionKind::RemoteCursor {
                    cursor: node,
                    progress: CursorProgress::Joined,
                },
            ] {
                runtime.record_reduction(&kind);
            }
            for kind in [
                ReductionKind::Call {
                    bind: node,
                    data: node,
                },
                ReductionKind::OperatorCall {
                    operator: node,
                    data: node,
                },
                ReductionKind::RemoteCursor {
                    cursor: node,
                    progress: CursorProgress::Claimed,
                },
                ReductionKind::RemoteCursor {
                    cursor: node,
                    progress: CursorProgress::Blocked,
                },
                ReductionKind::Stuck,
            ] {
                runtime.record_reduction(&kind);
            }
        });

        assert_eq!(
            values.interaction_net_profile_snapshot().reductions,
            NetReductionCounts {
                bind_join: 1,
                fan_join: 1,
                fan_commute: 1,
                fan_data: 1,
                fan_bind: 1,
                fan_operator: 1,
                erase: 1,
                call: 0,
                operator_call: 0,
                cursor_materialized: 1,
                cursor_joined: 1,
            }
        );
    }

    #[cfg(feature = "glam-prof")]
    #[test]
    fn profiling_semantic_signature_is_independent_of_ready_pair_order() {
        let reduce_in_order = |reverse| {
            let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
            let runtime = values.instantiate_core_net(&two_bind_join_template(&values));
            let mut pairs = runtime.test_with(&values, |runtime| {
                runtime.active_pairs().collect::<Vec<_>>()
            });
            assert_eq!(pairs.len(), 2);
            if reverse {
                pairs.reverse();
            }
            for pair in pairs {
                assert!(matches!(
                    runtime.with_test_access(&values, |runtime| runtime.step_active_pair(pair)),
                    CoreActivePairStep::Reduction(Reduction {
                        kind: ReductionKind::BindJoin,
                        ..
                    })
                ));
            }
            values.interaction_net_profile_snapshot()
        };

        let forward = reduce_in_order(false);
        let reverse = reduce_in_order(true);
        assert_eq!(forward.reductions, reverse.reductions);
        assert_eq!(forward.reductions.bind_join, 2);
    }

    #[cfg(feature = "glam-prof")]
    #[test]
    fn profiling_counts_calls_only_when_the_claimed_rewrite_commits() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let source = values.instantiate_core_net(&closed_unit_template(&values));
        let prepared = source.test_prepare_copy_source(&values);
        let copy_unit = values.with_runtime_value_access(|access| access.unit());
        let operator_unit = values.with_runtime_value_access(|access| access.unit());
        let (copy_runtime, copy_call) = claimed_call(&values, copy_unit);
        let (operator_runtime, operator_call) = claimed_call(&values, operator_unit);

        assert_eq!(
            values.interaction_net_profile_snapshot().reductions.call,
            0,
            "recognizing and claiming a Bind/Data pair is not a committed rewrite"
        );

        copy_runtime.with_test_access(&values, |runtime| {
            assert!(runtime.release_claimed_call(copy_call));
        });
        assert_eq!(values.interaction_net_profile_snapshot().reductions.call, 0);

        let pair = copy_runtime.test_with(&values, |runtime| {
            runtime
                .active_pairs()
                .next()
                .expect("released call is ready")
        });
        let reduction =
            copy_runtime.with_test_access(&values, |runtime| runtime.step_active_pair(pair));
        let CoreActivePairStep::Reduction(Reduction {
            kind: crate::interaction_net::ReductionKind::Call { bind, data },
            ..
        }) = reduction
        else {
            panic!("released pair must be claimable again")
        };
        copy_runtime.with_test_access(&values, |runtime| {
            runtime.resume_claimed_call_with_copy(
                crate::interaction_net::Call { pair, bind, data },
                prepared.in_access(runtime.values),
            );
        });
        assert_eq!(values.interaction_net_profile_snapshot().reductions.call, 1);

        let operator =
            values.with_runtime_value_access(|access| CoreOperator::Applicable(access.unit()));
        operator_runtime.with_test_access(&values, |runtime| {
            runtime.resume_claimed_call_with_operator(operator_call, operator);
        });
        assert_eq!(values.interaction_net_profile_snapshot().reductions.call, 2);
    }

    #[cfg(feature = "glam-prof")]
    #[test]
    fn profiling_counts_operator_calls_only_when_completion_commits() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let (operator, data) = values.with_runtime_value_access(|access| {
            (CoreOperator::Applicable(access.unit()), access.unit())
        });
        let (runtime, call) = claimed_operator_call(&values, operator, data);

        assert_eq!(
            values
                .interaction_net_profile_snapshot()
                .reductions
                .operator_call,
            0
        );
        runtime.with_test_access(&values, |runtime| {
            assert!(runtime.release_claimed_operator_call(call));
        });
        assert_eq!(
            values
                .interaction_net_profile_snapshot()
                .reductions
                .operator_call,
            0
        );

        let pair = runtime.test_with(&values, |runtime| {
            runtime
                .active_pairs()
                .next()
                .expect("released call is ready")
        });
        let reduction = runtime.with_test_access(&values, |runtime| runtime.step_active_pair(pair));
        let CoreActivePairStep::Reduction(Reduction {
            kind: crate::interaction_net::ReductionKind::OperatorCall { operator, data },
            ..
        }) = reduction
        else {
            panic!("released operator pair must be claimable again")
        };
        let yielded = values.with_runtime_value_access(|access| OperatorYield::Data(access.unit()));
        runtime.with_test_access(&values, |runtime| {
            runtime.complete_claimed_operator_call(
                crate::interaction_net::OperatorCall {
                    pair,
                    operator,
                    data,
                },
                yielded,
            );
        });
        assert_eq!(
            values
                .interaction_net_profile_snapshot()
                .reductions
                .operator_call,
            1
        );
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
            runtime.complete_claimed_operator_call(
                call,
                OperatorYield::Data(third.duplicate_for_test(&values)),
            );
        });
        let records = completion_probe.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].leaving_edges(), 2);
        assert_eq!(records[0].adding_edges(), 1);

        failed.with_test_access(&values, |runtime| {
            let halt = EvaluationHalt::from_value(runtime.values(), third);
            runtime.fail_claimed_call(failed_call, halt);
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
            target.with_test_access(&values, |runtime| runtime.reduce_pair_for_test(pair)),
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
            _ => panic!("nested copy should block on its source"),
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
        let callable =
            target_builder.data(values.with_runtime_value_access(|access| access.unit()));
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
                _ => panic!("target should claim its callable data"),
            };
        target.with_test_access(&values, |runtime| {
            runtime.resume_claimed_call_with_copy(call, prepared.in_access(runtime.values));
        });

        let first_cursor = target
            .with_test_access(&values, |access| access.reduce_next_for_test())
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
            target.with_test_access(&values, |access| access.reduce_next_for_test()),
            Some(Reduction {
                kind: crate::interaction_net::ReductionKind::BindJoin,
                ..
            })
        ));

        let mut claims = Vec::new();
        for _ in 0..2 {
            let reduction = target
                .with_test_access(&values, |access| access.reduce_next_for_test())
                .expect("each converging cursor should be claimable");
            let crate::interaction_net::ReductionKind::RemoteCursor {
                cursor,
                progress: CursorProgress::Claimed,
            } = reduction.kind
            else {
                panic!("converging cursor should claim")
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
            &CorePreparedCopySource<'_>,
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
        let argument = builder.data(values.with_runtime_value_access(|access| access.unit()));
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
        let retained_data = retained_code.runtime().with_test_access(&values, |access| {
            access.with(|runtime| {
                runtime
                    .interface_data(runtime.exposed())
                    .map(|value| access.values().duplicate_value(value))
            })
        });
        values.with_runtime_value_access(|access| {
            access.assert_same_representation_for_test(&retained_data, &Some(access.unit()));
        });
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
    fn frontier_observation_is_a_nonrooting_edge_for_managed_driver_state() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let public_values = crate::api::Values::from_core_factory(values.clone());
        let baseline = values
            .collect_managed_for_test()
            .expect("the frontier-observation fixture should start collectible");
        let source = values.instantiate_core_net(&closed_unit_template(&values));
        let exposed = source.test_with(&values, RuntimeNet::exposed);
        let owner = public_values.wrap(Value::Net(crate::core::NetValue::new(
            source.duplicate_for_test(&values),
        )));
        let _observation = values.with_runtime_value_access(|access| CoreFrontierObservation {
            source: source.duplicate_in(&access),
            observed_topology: 0,
            endpoint: DemandEndpoint::Cursor(exposed.node()),
        });

        let retained = values
            .collect_managed_for_test()
            .expect("the explicit external owner should retain the semantic net");
        assert_eq!(retained.root_entries(), baseline.root_entries() + 1);
        assert!(retained.marked_slots() > baseline.marked_slots());

        drop(owner);
        let retired = values
            .collect_managed_for_test()
            .expect("dropping the explicit owner should retire its semantic net");
        assert_eq!(retired.root_entries(), baseline.root_entries());
        assert!(retired.finalized_slots() >= 1);
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
            .with_test_access(&values, |access| access.reduce_pair_for_test(pair))
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
            let data = net.with(|runtime| {
                runtime
                    .interface_data(runtime.exposed())
                    .map(|value| access.duplicate_value(value))
            });
            access.assert_same_representation_for_test(&data, &Some(access.unit()));
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
    fn panic_under_the_net_lock_neither_aborts_nor_blocks_collection() {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let public_values = crate::api::Values::from_core_factory(values.clone());
        let net = values.instantiate_core_net(&closed_unit_template(&values));

        // The batch guard unwinds through a net the panic just poisoned.
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            values.with_runtime_value_access(|values| {
                let access = net.access(&values);
                let _: Result<(), CoreNetContention> = access.with_normalization_batch(|_| {
                    access.with_mut(|_| panic!("forced panic under the net lock"));
                });
            });
        }));
        assert!(unwind.is_err());

        let owner = public_values.wrap(Value::Net(crate::core::NetValue::new(net)));
        values
            .collect_managed_for_test()
            .expect("a rooted poisoned net must remain traceable");
        drop(owner);
        values
            .collect_managed_for_test()
            .expect("a retired poisoned net must collect");
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
            _ => panic!("nested copy should expose its source frontier"),
        };

        target.with_test_access(&values, |wrong_access| match observation.endpoint() {
            DemandEndpoint::Cursor(cursor) => {
                let _ = observation.step_cursor(&wrong_access, cursor, |_| true);
            }
            DemandEndpoint::ActivePair(pair) => {
                let _ = observation.step_active_pair(&wrong_access, pair, |_| true);
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
