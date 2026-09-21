//! Crate-private protocol for resumable evaluation to outer WHNF.
//!
//! W1A installs the state vocabulary and W1B adds its callback-free regional
//! driver. W1C projects and publishes durable checkpoints at real regional
//! boundaries. W2 uses that protocol for client demand and promise following;
//! later checkpoints extend it through lazy sources and caller frames.

use std::collections::BTreeSet;
use std::sync::Arc;

#[cfg(test)]
use crate::core::RuntimeValueAccess;
use crate::core::{
    CoreValueFactory, DeferredValueId, EvaluationFailure, FunctionValue, LazyId, LazyValue,
    ManagedLazyRoot, ManagedPromiseRoot, PromisedValue, Value,
};
use crate::core_net::CoreWaitToken;
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

pub(crate) mod managed_state;
use managed_state::{
    ManagedLazyCheckpointEdge, ManagedWhnfAccess, ManagedWhnfAccessError, ManagedWhnfRoot,
};

/// One resumable request to reduce a value's outer deferred shells to WHNF.
///
/// The checkpoint is intentionally neither cloneable nor publicly exposed.
/// An outer owner moves one computation between polls and remains solely
/// responsible for the eventual result destination.
pub(crate) struct WhnfComputation {
    checkpoint: DurableWhnfCheckpoint,
}

/// Durable entry mode for one WHNF request.
///
/// A lazy producer begins with the exact lazy identity whose source it owns.
/// Once that source has produced a value, the same computation installs the
/// ordinary rooted demand checkpoint and never reconstructs the source result.
enum DurableWhnfCheckpoint {
    /// Protocol fixture only: production lazy sources now install their
    /// checkpoint beneath the lazy during the same source poll.
    Seed {
        focus: RuntimeValueRoot,
        source_owner: Option<LazyId>,
    },
    ManagedDemand {
        state: ManagedWhnfRoot,
        observation: WhnfPollObservation,
    },
}

#[derive(Clone, Copy, Default)]
struct WhnfPollObservation {
    application_frame_pending: bool,
}

/// Complete raw-edge WHNF state shared by regional execution and net-owned
/// suspension.
///
/// The role wrappers below move this state without inspecting its semantic
/// edges or rebuilding its continuation containers. Only regional work may be
/// evaluated; only net-owned state may outlive its matching access region.
pub(crate) struct WhnfState {
    focus: Value,
    frames: Vec<WhnfContinuation>,
    followed: BTreeSet<DeferredValueId>,
    source_owner: Option<LazyId>,
    cycle_promise: Option<PromisedValue>,
}

/// Callback-free working state beneath one managed-access region.
pub(crate) struct RegionalWhnfWork(WhnfState);

/// Region-authorized mutable view of one canonical WHNF state.
///
/// Construction requires the matching evaluation access and ties this borrow
/// to that access region. Reducers may edit canonical state in place, but
/// cannot move the state into a durable owner or retain this view after the
/// mutator region closes.
pub(crate) struct RegionalWhnfState<'state>(&'state mut WhnfState);

/// Complete WHNF state stored as traced edges inside a runtime net.
///
/// Unlike [`RegionalWhnfWork`], this representation may outlive one access region because its
/// enclosing managed net traces every semantic edge below. Claiming and
/// publishing this state consume one role wrapper and install the other; they
/// never project a copy or rebuild a continuation.
/// Opaque runtime-net ownership wrapper for one complete WHNF checkpoint.
///
/// The type is public only because the public generic interaction-net trait
/// names specialization payloads. Its state and constructors remain internal.
pub struct NetWhnfState(WhnfState);

#[cfg(test)]
pub(crate) struct NetWhnfObservation {
    pub(crate) focus: Option<DeferredValueId>,
    pub(crate) followed: BTreeSet<DeferredValueId>,
    pub(crate) frames: usize,
    pub(crate) source_owner: Option<LazyId>,
    pub(crate) cycle_promise: Option<crate::core::PromiseId>,
}

/// One generic frame inside the canonical traced WHNF state.
pub(crate) struct WhnfFrame {
    #[allow(
        dead_code,
        reason = "the W1 synthetic frame algebra observes control fields only in protocol fixtures"
    )]
    kind: WhnfFrameKind,
    #[allow(
        dead_code,
        reason = "the W1 synthetic frame algebra observes control fields only in protocol fixtures"
    )]
    cursor: usize,
    retained: Vec<Value>,
}

enum WhnfContinuation {
    Generic(WhnfFrame),
    Application {
        arguments: Vec<Value>,
        next: usize,
    },
    DictionaryApplication {
        effect_payload: Value,
        remaining_effect_values: Vec<Value>,
        next_effect_value: usize,
        apply_member: Option<Value>,
    },
    SemanticUndefined {
        purpose: UndefinedPurpose,
        ancestors: Vec<WhnfUndefinedDictionary>,
        phase: UndefinedPhase,
    },
    StaticAccess {
        keys: Arc<[crate::core::Key]>,
        next: usize,
    },
}

struct WhnfUndefinedDictionary {
    members: Vec<Value>,
    next: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UndefinedPurpose {
    EffectPayload,
    EffectExtra,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UndefinedPhase {
    Inspect,
    ReturnTrue,
}

impl From<WhnfFrame> for WhnfContinuation {
    fn from(frame: WhnfFrame) -> Self {
        Self::Generic(frame)
    }
}

impl RegionalWhnfWork {
    pub(crate) fn from_focus(access: &EvaluationValueAccess<'_>, focus: Value) -> Self {
        Self::from_parts(access, focus, Vec::new(), BTreeSet::new(), None, None)
    }

    pub(crate) fn with_source_owner(mut self, source_owner: LazyId) -> Self {
        self.0.source_owner = Some(source_owner);
        self
    }

    pub(crate) fn from_application_checkpoint_in(
        access: &EvaluationValueAccess<'_>,
        function: Value,
        arguments: &[Value],
        source_owner: Option<LazyId>,
    ) -> Self {
        assert!(!arguments.is_empty(), "application requires an argument");
        Self::from_parts(
            access,
            function,
            vec![WhnfContinuation::Application {
                arguments: arguments
                    .iter()
                    .map(|argument| access.values().duplicate_value(argument))
                    .collect(),
                next: 0,
            }],
            BTreeSet::new(),
            source_owner,
            None,
        )
    }

    pub(crate) fn from_static_access_checkpoint_in(
        access: &EvaluationValueAccess<'_>,
        base: Value,
        keys: Arc<[crate::core::Key]>,
        source_owner: Option<LazyId>,
    ) -> Self {
        let frames = (!keys.is_empty())
            .then_some(WhnfContinuation::StaticAccess { keys, next: 0 })
            .into_iter()
            .collect();
        Self::from_parts(access, base, frames, BTreeSet::new(), source_owner, None)
    }

    fn from_parts(
        _access: &EvaluationValueAccess<'_>,
        focus: Value,
        frames: Vec<WhnfContinuation>,
        followed: BTreeSet<DeferredValueId>,
        source_owner: Option<LazyId>,
        cycle_promise: Option<PromisedValue>,
    ) -> Self {
        Self(WhnfState {
            focus,
            frames,
            followed,
            source_owner,
            cycle_promise,
        })
    }

    fn state_in<'access, 'scope>(
        &'access mut self,
        access: &'access EvaluationValueAccess<'scope>,
    ) -> RegionalWhnfState<'access> {
        self.0.regional_in(access)
    }

    #[cfg(test)]
    pub(crate) fn container_identities_for_test(&self) -> Vec<(usize, usize, usize)> {
        self.0.container_identities_for_test()
    }

    #[cfg(test)]
    pub(crate) fn observation_for_test(
        &self,
        access: &RuntimeValueAccess<'_>,
    ) -> NetWhnfObservation {
        self.0.observation_for_test(access)
    }

    /// Reports every managed semantic edge retained by this regional state.
    ///
    /// Composite managed checkpoints use this same canonical walk rather than
    /// projecting child WHNF work into another rooted representation.
    pub(crate) fn trace_managed_edges(&self, visitor: &mut glam_gc::Visitor<'_>) {
        self.0.trace_managed_edges(visitor);
    }
}

impl WhnfState {
    fn poll_observation(&self) -> WhnfPollObservation {
        WhnfPollObservation {
            application_frame_pending: self
                .frames
                .iter()
                .any(|frame| matches!(frame, WhnfContinuation::Application { .. })),
        }
    }

    fn regional_in<'access, 'scope>(
        &'access mut self,
        _access: &'access EvaluationValueAccess<'scope>,
    ) -> RegionalWhnfState<'access> {
        RegionalWhnfState(self)
    }
}

impl std::ops::Deref for RegionalWhnfState<'_> {
    type Target = WhnfState;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl std::ops::DerefMut for RegionalWhnfState<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
    }
}

impl std::ops::Deref for RegionalWhnfWork {
    type Target = WhnfState;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for RegionalWhnfWork {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Shared resumption shapes selected by the W0 census.
#[allow(
    dead_code,
    reason = "W0 selected the complete frame vocabulary; W3-W6 construct the deeper frame families"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WhnfFrameKind {
    DemandThenInspect,
    OrderedOperands,
    CollectionWalk,
    Application,
    KeyConversion,
    AccessPath,
    DiagnosticContext,
}

/// One callback-free regional evaluator transition.
///
/// `Delegate` replaces the current focus without pushing a frame. `Boundary`
/// carries only a durable request; its interpretation belongs to the outer
/// evaluation driver after regional access closes.
#[allow(
    dead_code,
    reason = "W2 needs delegation and terminal steps; W3+ adds explicit continuation frames"
)]
pub(crate) enum RegionalWhnfStep {
    Delegate(Value),
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Failed(Arc<EvaluationFailure>),
}

/// Outcome of driving callback-free WHNF work beneath one managed-access
/// region.
///
/// `Boundary` and `Yielded` deliberately return the exact regional work. W1C
/// will project that work into a durable checkpoint before the access region
/// closes; callers must not store this raw form in a machine.
pub(crate) enum RegionalWhnfDrive {
    Ready(Value),
    Boundary {
        work: RegionalWhnfWork,
        request: RegionalBoundaryRequest,
    },
    Yielded(RegionalWhnfWork),
    Failed(Arc<EvaluationFailure>),
}

pub(crate) enum RegionalWhnfStatus {
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

/// Net-owned counterpart of [`RegionalWhnfDrive`].
///
/// Yield and boundary outcomes contain the complete replacement state. Ready
/// and failed outcomes are consumed by the caller while matching access is
/// still active and therefore need no intermediate roots.
#[allow(
    dead_code,
    reason = "the NC1 split oracle retains the consuming driver; production NC4 claims in place so unwind can restore the same payload"
)]
pub(crate) enum NetWhnfDrive {
    Ready(Value),
    Boundary {
        state: NetWhnfState,
        request: RegionalBoundaryRequest,
    },
    Yielded(NetWhnfState),
    Failed(Arc<EvaluationFailure>),
}

/// WHNF's semantic name for the shared outer evaluation budget.
pub(crate) type WhnfStepBudget = EvaluationStepBudget;

/// Drives bounded callback-free transitions without recursive Rust calls.
///
/// The active access parameter is intentionally required even though the
/// driver itself only rearranges already-projected values. A reducer may copy
/// or inspect those values only through the same region. Dependency handling,
/// callbacks, root publication, and scheduler actions remain outside this
/// loop.
pub(crate) fn drive_regional<'scope>(
    access: &EvaluationValueAccess<'scope>,
    mut work: RegionalWhnfWork,
    budget: &mut WhnfStepBudget,
    reduce: impl FnMut(&EvaluationValueAccess<'scope>, &mut RegionalWhnfState<'_>) -> RegionalWhnfStep,
) -> RegionalWhnfDrive {
    match drive_regional_in_place(access, &mut work, budget, reduce) {
        RegionalWhnfStatus::Ready(value) => RegionalWhnfDrive::Ready(value),
        RegionalWhnfStatus::Boundary(request) => RegionalWhnfDrive::Boundary { work, request },
        RegionalWhnfStatus::Yielded => RegionalWhnfDrive::Yielded(work),
        RegionalWhnfStatus::Failed(failure) => RegionalWhnfDrive::Failed(failure),
    }
}

pub(crate) fn drive_regional_in_place<'scope>(
    access: &EvaluationValueAccess<'scope>,
    work: &mut RegionalWhnfWork,
    budget: &mut WhnfStepBudget,
    reduce: impl FnMut(&EvaluationValueAccess<'scope>, &mut RegionalWhnfState<'_>) -> RegionalWhnfStep,
) -> RegionalWhnfStatus {
    let mut active = work.state_in(access);
    drive_regional_state_in_place(access, &mut active, budget, reduce)
}

fn drive_regional_state_in_place<'scope>(
    access: &EvaluationValueAccess<'scope>,
    work: &mut RegionalWhnfState<'_>,
    budget: &mut WhnfStepBudget,
    mut reduce: impl FnMut(
        &EvaluationValueAccess<'scope>,
        &mut RegionalWhnfState<'_>,
    ) -> RegionalWhnfStep,
) -> RegionalWhnfStatus {
    loop {
        if !budget.try_consume() {
            return RegionalWhnfStatus::Yielded;
        }
        match reduce(access, work) {
            RegionalWhnfStep::Delegate(focus) => work.focus = focus,
            RegionalWhnfStep::Ready(value) => return RegionalWhnfStatus::Ready(value),
            RegionalWhnfStep::Boundary(request) => {
                return RegionalWhnfStatus::Boundary(request);
            }
            RegionalWhnfStep::Failed(failure) => return RegionalWhnfStatus::Failed(failure),
        }
    }
}

/// A regional result which requires orchestration outside managed access.
#[allow(
    dead_code,
    reason = "W2 implements deferred shells; W3-W4 construct direct dependencies and external boundaries"
)]
pub(crate) enum RegionalBoundaryRequest {
    Dependency(WhnfDependency),
    Deferred(WhnfDeferredRequest),
    External(WhnfExternalBoundary),
}

/// One unresolved semantic shell requiring policy outside managed access.
///
/// These registered roots preserve the exact lazy or promise identity. The
/// semantic reducer neither admits a producer nor subscribes to completion;
/// the durable owner performs those actions after its access region closes.
pub(crate) enum WhnfDeferredRequest {
    Lazy(ManagedLazyRoot),
    Promise(ManagedPromiseRoot),
    PromiseFollow(ManagedPromiseRoot),
}

/// External boundary family. Later checkpoints add the source-specific
/// durable payload only when a production boundary is migrated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "external source families are staged for W4")]
pub(crate) enum WhnfExternalBoundary {
    Reflection,
    Host,
    Net,
}

/// Exact completion source which can block a resumable WHNF computation.
///
/// This semantic/control type deliberately does not depend on the scheduler's
/// broader `WorkDependency` vocabulary. The evaluation boundary owns that
/// translation.
#[derive(Clone)]
#[allow(
    dead_code,
    reason = "W2 consumes translated dependencies; W3+ constructs them inside the semantic machine"
)]
pub(crate) enum WhnfDependency {
    Wait(CoreWaitToken),
    Promise(ManagedPromiseRoot),
}

/// Result of one bounded poll of a [`WhnfComputation`].
///
/// `Pending` and `Yielded` never consume the computation's exact checkpoint;
/// the outer owner polls the same instance again.
pub(crate) enum WhnfPoll {
    Ready(RuntimeValueRoot),
    Pending(WhnfDependency),
    Deferred(WhnfDeferredRequest),
    External(WhnfExternalBoundary),
    Yielded,
    Failed(RuntimeFailureRoot),
}

impl NetWhnfState {
    /// Publishes one complete regional successor into net-owned storage.
    ///
    /// Requiring the matching access makes the ownership handoff explicit even
    /// though every raw value moves rather than being duplicated. The caller
    /// must install the returned state beneath its traced net owner before the
    /// region closes.
    pub(crate) fn from_regional(
        _access: &EvaluationValueAccess<'_>,
        work: RegionalWhnfWork,
    ) -> Self {
        Self(work.0)
    }

    /// Claims the complete state for regional execution without walking it.
    pub(crate) fn into_regional(self, _access: &EvaluationValueAccess<'_>) -> RegionalWhnfWork {
        RegionalWhnfWork(self.0)
    }

    #[cfg(test)]
    pub(crate) fn application_checkpoint_for_test(
        access: &EvaluationValueAccess<'_>,
        focus: Value,
        arguments: Vec<Value>,
    ) -> Self {
        Self::from_regional(
            access,
            RegionalWhnfWork::from_parts(
                access,
                focus,
                vec![WhnfContinuation::Application { arguments, next: 0 }],
                BTreeSet::new(),
                None,
                None,
            ),
        )
    }

    #[cfg(test)]
    pub(crate) fn container_identities_for_test(&self) -> Vec<(usize, usize, usize)> {
        self.0.container_identities_for_test()
    }

    #[cfg(test)]
    pub(crate) fn observation_for_test(
        &self,
        access: &RuntimeValueAccess<'_>,
    ) -> NetWhnfObservation {
        self.0.observation_for_test(access)
    }

    /// Drives one bounded callback-free quantum using the same regional
    /// transition loop as ordinary WHNF computation.
    #[allow(
        dead_code,
        reason = "the NC1 split oracle retains this consuming adapter; production NC4 drives an in-place claim so unwind can restore it"
    )]
    pub(crate) fn drive_in(
        self,
        access: &EvaluationValueAccess<'_>,
        budget: &mut WhnfStepBudget,
        reduce: impl FnMut(&EvaluationValueAccess<'_>, &mut RegionalWhnfState<'_>) -> RegionalWhnfStep,
    ) -> NetWhnfDrive {
        match drive_regional(access, self.into_regional(access), budget, reduce) {
            RegionalWhnfDrive::Ready(value) => NetWhnfDrive::Ready(value),
            RegionalWhnfDrive::Boundary { work, request } => NetWhnfDrive::Boundary {
                state: Self::from_regional(access, work),
                request,
            },
            RegionalWhnfDrive::Yielded(work) => {
                NetWhnfDrive::Yielded(Self::from_regional(access, work))
            }
            RegionalWhnfDrive::Failed(failure) => NetWhnfDrive::Failed(failure),
        }
    }
}

#[cfg(test)]
impl WhnfState {
    fn observation_for_test(&self, access: &RuntimeValueAccess<'_>) -> NetWhnfObservation {
        let focus = match &self.focus {
            Value::Lazy(lazy) => Some(lazy.access(access).id().into()),
            Value::Promised(promise) => Some(promise.access(access).id().into()),
            _ => None,
        };
        NetWhnfObservation {
            focus,
            followed: self.followed.clone(),
            frames: self.frames.len(),
            source_owner: self.source_owner,
            cycle_promise: self
                .cycle_promise
                .as_ref()
                .map(|promise| promise.access(access).id()),
        }
    }
}

impl WhnfState {
    #[cfg(test)]
    fn container_identities_for_test(&self) -> Vec<(usize, usize, usize)> {
        let mut identities = vec![(
            self.frames.as_ptr() as usize,
            self.frames.len(),
            self.frames.capacity(),
        )];
        for frame in &self.frames {
            match frame {
                WhnfContinuation::Generic(frame) => identities.push((
                    frame.retained.as_ptr() as usize,
                    frame.retained.len(),
                    frame.retained.capacity(),
                )),
                WhnfContinuation::Application { arguments, .. } => identities.push((
                    arguments.as_ptr() as usize,
                    arguments.len(),
                    arguments.capacity(),
                )),
                WhnfContinuation::DictionaryApplication {
                    remaining_effect_values,
                    ..
                } => identities.push((
                    remaining_effect_values.as_ptr() as usize,
                    remaining_effect_values.len(),
                    remaining_effect_values.capacity(),
                )),
                WhnfContinuation::SemanticUndefined { ancestors, .. } => {
                    identities.push((
                        ancestors.as_ptr() as usize,
                        ancestors.len(),
                        ancestors.capacity(),
                    ));
                    identities.extend(ancestors.iter().map(|ancestor| {
                        (
                            ancestor.members.as_ptr() as usize,
                            ancestor.members.len(),
                            ancestor.members.capacity(),
                        )
                    }));
                }
                WhnfContinuation::StaticAccess { keys, .. } => {
                    identities.push((keys.as_ptr() as usize, keys.len(), keys.len()));
                }
            }
        }
        identities
    }

    fn trace_managed_edges(&self, visitor: &mut glam_gc::Visitor<'_>) {
        trace_whnf_value(&self.focus, visitor);
        for frame in &self.frames {
            frame.trace_managed_edges(visitor);
        }
        if let Some(promise) = &self.cycle_promise {
            promise.trace_managed_edge(visitor);
        }
    }
}

impl WhnfContinuation {
    fn trace_managed_edges(&self, visitor: &mut glam_gc::Visitor<'_>) {
        match self {
            Self::Generic(frame) => trace_whnf_values(&frame.retained, visitor),
            Self::Application { arguments, .. } => trace_whnf_values(arguments, visitor),
            Self::DictionaryApplication {
                effect_payload,
                remaining_effect_values,
                apply_member,
                ..
            } => {
                trace_whnf_value(effect_payload, visitor);
                trace_whnf_values(remaining_effect_values, visitor);
                if let Some(value) = apply_member {
                    trace_whnf_value(value, visitor);
                }
            }
            Self::SemanticUndefined { ancestors, .. } => {
                for ancestor in ancestors {
                    trace_whnf_values(&ancestor.members, visitor);
                }
            }
            Self::StaticAccess { .. } => {}
        }
    }
}

fn trace_whnf_value(value: &Value, visitor: &mut glam_gc::Visitor<'_>) {
    crate::core::trace_compatibility_value_managed_edges(value, visitor);
}

fn trace_whnf_values(values: &[Value], visitor: &mut glam_gc::Visitor<'_>) {
    for value in values {
        trace_whnf_value(value, visitor);
    }
}

// SAFETY: every semantic `Value` position is traversed through the core's
// compile-exhaustive compatibility walk, and the promise breadcrumb reports
// its exact managed edge. Scalar cursors, IDs, enum tags, and key paths contain
// no managed edge. The representation performs no active work during Drop.
unsafe impl glam_gc::Trace for NetWhnfState {
    fn trace(&self, visitor: &mut glam_gc::Visitor<'_>) {
        self.0.trace_managed_edges(visitor);
    }
}

#[cfg(test)]
// SAFETY: direct destruction releases only passive compatibility values and
// ordinary collections. Managed identities are inert edges under the Trace
// contract above; no callback or runtime capability is invoked by Drop.
unsafe impl crate::core::ManagedFamily for NetWhnfState {
    const DROP_RECORD: crate::core::ManagedDropRecord = crate::core::ManagedDropRecord::passive(
        "NC2.0 canonical net-owned WHNF fixture",
        "src/eval/whnf.rs",
        "direct Drop releases passive compatibility values",
        "every managed identity is reported by WhnfState's canonical edge walk",
    );
}

impl WhnfComputation {
    pub(crate) fn from_root(focus: RuntimeValueRoot) -> Self {
        Self {
            checkpoint: DurableWhnfCheckpoint::Seed {
                focus,
                source_owner: None,
            },
        }
    }

    /// Reports whether an application checkpoint failed before consuming all
    /// of its arguments. Terminal failure publishes the last regional state,
    /// so an outer owner can distinguish callable/application failure from a
    /// failure encountered while forcing the completed result.
    pub(crate) fn application_frame_pending(&self) -> bool {
        match &self.checkpoint {
            DurableWhnfCheckpoint::ManagedDemand { observation, .. } => {
                observation.application_frame_pending
            }
            DurableWhnfCheckpoint::Seed { .. } => false,
        }
    }

    pub(crate) fn from_application_checkpoint_in(
        access: &EvaluationValueAccess<'_>,
        function: Value,
        arguments: &[Value],
        source_owner: Option<LazyId>,
    ) -> Self {
        let work = RegionalWhnfWork::from_application_checkpoint_in(
            access,
            function,
            arguments,
            source_owner,
        );
        Self::from_structured_work_in(access, work)
    }

    #[cfg(test)]
    pub(crate) fn from_static_access_checkpoint_in(
        access: &EvaluationValueAccess<'_>,
        base: Value,
        keys: Arc<[crate::core::Key]>,
        source_owner: Option<LazyId>,
    ) -> Self {
        let work =
            RegionalWhnfWork::from_static_access_checkpoint_in(access, base, keys, source_owner);
        Self::from_structured_work_in(access, work)
    }

    fn from_structured_work_in(access: &EvaluationValueAccess<'_>, work: RegionalWhnfWork) -> Self {
        let observation = work.poll_observation();
        let state = ManagedWhnfRoot::from_regional_in(access, work)
            .expect("canonical WHNF state must fit its reviewed managed slot");
        Self {
            checkpoint: DurableWhnfCheckpoint::ManagedDemand { state, observation },
        }
    }

    pub(crate) fn from_promise_root(
        values: &CoreValueFactory,
        promise: &ManagedPromiseRoot,
    ) -> Self {
        let focus = values.construct_runtime_value_root(|access| {
            Value::Promised(PromisedValue::from_root(promise, access))
        });
        Self::from_root(focus)
    }

    pub(crate) fn runtime_id(&self) -> crate::runtime::EvaluationRuntimeId {
        match &self.checkpoint {
            DurableWhnfCheckpoint::Seed { focus, .. } => focus.runtime_id(),
            DurableWhnfCheckpoint::ManagedDemand { state, .. } => state.runtime_id(),
        }
    }

    fn promote_seed_in(&mut self, access: &EvaluationValueAccess<'_>) {
        let work = match &self.checkpoint {
            DurableWhnfCheckpoint::Seed {
                focus,
                source_owner,
            } => Some(RegionalWhnfWork::from_parts(
                access,
                access.clone_root(focus),
                Vec::new(),
                BTreeSet::new(),
                *source_owner,
                None,
            )),
            DurableWhnfCheckpoint::ManagedDemand { .. } => None,
        };
        let Some(work) = work else {
            return;
        };
        let state = ManagedWhnfRoot::from_regional_in(access, work)
            .expect("canonical WHNF state must fit its reviewed managed slot");
        let prior = std::mem::replace(
            &mut self.checkpoint,
            DurableWhnfCheckpoint::ManagedDemand {
                state,
                observation: WhnfPollObservation::default(),
            },
        );
        drop(prior);
    }

    /// Polls one bounded callback-free quantum beneath matching value access.
    ///
    /// A minimal seed is promoted while its input root remains installed; all
    /// subsequent polls mutate the single canonical managed state in place.
    /// Returned boundary dispositions contain no active access and are
    /// interpreted by the outer owner only after its access callback returns.
    pub(crate) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        budget: &mut WhnfStepBudget,
        reduce: impl FnMut(&EvaluationValueAccess<'_>, &mut RegionalWhnfState<'_>) -> RegionalWhnfStep,
    ) -> WhnfPoll {
        self.promote_seed_in(access);
        let mut reduce = reduce;
        let DurableWhnfCheckpoint::ManagedDemand { state, observation } = &mut self.checkpoint
        else {
            panic!("a lazy source must install its result before WHNF demand")
        };
        let managed = state.access(access).unwrap_or_else(|error| match error {
            ManagedWhnfAccessError::RuntimeMismatch => {
                panic!("managed WHNF state and poll access must share one runtime")
            }
            ManagedWhnfAccessError::Poisoned => {
                panic!("poison must be reported while locking managed WHNF state")
            }
        });
        let (status, observed) = match drive_managed_state_in(&managed, access, budget, &mut reduce)
        {
            Ok(result) => result,
            Err(error) => return managed_state_error_poll(access, error),
        };
        *observation = observed;
        regional_status_poll(access, status)
    }

    /// Polls the production outer-shell reducer beneath one managed region.
    ///
    /// W2 initially handles only lazy and promise shells. Later phases extend
    /// the same reducer with caller frames and source-specific work without
    /// changing the durable publication boundary.
    pub(crate) fn poll_semantic_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        budget: &mut WhnfStepBudget,
    ) -> WhnfPoll {
        self.poll_in(access, budget, reduce_semantic_shell)
    }
}

impl ManagedLazyCheckpointEdge {
    /// Polls the exact state installed beneath a managed lazy without creating
    /// another registered root or rebuilding its continuation containers.
    pub(crate) fn poll_semantic_in(
        &self,
        access: &EvaluationValueAccess<'_>,
        budget: &mut WhnfStepBudget,
    ) -> Option<WhnfPoll> {
        let _ = self.duplicate_whnf_in(access.values())?;
        let managed = self.access(access);
        let mut reduce = reduce_semantic_shell;
        let (status, _) = match drive_managed_state_in(&managed, access, budget, &mut reduce) {
            Ok(result) => result,
            Err(error) => return Some(managed_state_error_poll(access, error)),
        };
        Some(regional_status_poll(access, status))
    }
}

fn drive_managed_state_in(
    managed: &ManagedWhnfAccess<'_, '_>,
    access: &EvaluationValueAccess<'_>,
    budget: &mut WhnfStepBudget,
    reduce: &mut impl FnMut(&EvaluationValueAccess<'_>, &mut RegionalWhnfState<'_>) -> RegionalWhnfStep,
) -> Result<(RegionalWhnfStatus, WhnfPollObservation), ManagedWhnfAccessError> {
    managed.with_state_transition(|work| {
        let status = drive_regional_state_in_place(access, work, budget, reduce);
        (status, work.poll_observation())
    })
}

fn managed_state_error_poll(
    access: &EvaluationValueAccess<'_>,
    error: ManagedWhnfAccessError,
) -> WhnfPoll {
    match error {
        ManagedWhnfAccessError::RuntimeMismatch => {
            unreachable!("managed WHNF access was already provenance-checked")
        }
        ManagedWhnfAccessError::Poisoned => {
            let failure = Arc::new(EvaluationFailure::message(
                "managed WHNF evaluation state was poisoned by an earlier unwind",
            ));
            WhnfPoll::Failed(access.values().root_runtime_failure(failure))
        }
    }
}

fn regional_status_poll(
    access: &EvaluationValueAccess<'_>,
    status: RegionalWhnfStatus,
) -> WhnfPoll {
    match status {
        RegionalWhnfStatus::Ready(value) => {
            WhnfPoll::Ready(access.values().root_runtime_value(value))
        }
        RegionalWhnfStatus::Boundary(request) => match request {
            RegionalBoundaryRequest::Dependency(dependency) => WhnfPoll::Pending(dependency),
            RegionalBoundaryRequest::Deferred(deferred) => WhnfPoll::Deferred(deferred),
            RegionalBoundaryRequest::External(boundary) => WhnfPoll::External(boundary),
        },
        RegionalWhnfStatus::Yielded => WhnfPoll::Yielded,
        RegionalWhnfStatus::Failed(failure) => {
            WhnfPoll::Failed(access.values().root_runtime_failure(failure))
        }
    }
}

pub(crate) fn reduce_semantic_shell(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfState<'_>,
) -> RegionalWhnfStep {
    match &work.0.focus {
        Value::Lazy(lazy) => match access.lazy(lazy).cached() {
            Some(Ok(value)) => {
                work.0.followed.insert(access.lazy(lazy).id().into());
                RegionalWhnfStep::Delegate(value.into_value())
            }
            Some(Err(failure)) => RegionalWhnfStep::Failed(failure),
            None => {
                let id = access.lazy(lazy).id();
                if work.0.source_owner == Some(id)
                    && let Some(promise) = &work.0.cycle_promise
                {
                    RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Deferred(
                        WhnfDeferredRequest::PromiseFollow(promise.root_in(access.values())),
                    ))
                } else {
                    RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Deferred(
                        WhnfDeferredRequest::Lazy(lazy.root_in(access.values())),
                    ))
                }
            }
        },
        Value::Promised(promise) => match access.promise(promise).assignment() {
            Some(Ok(value)) => {
                work.0.cycle_promise = Some(promise.duplicate_in(access.values()));
                if work.0.followed.insert(access.promise(promise).id().into()) {
                    RegionalWhnfStep::Delegate(value)
                } else {
                    RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Deferred(
                        WhnfDeferredRequest::PromiseFollow(promise.root_in(access.values())),
                    ))
                }
            }
            Some(Err(failure)) => {
                if let Some(producer) = access.promise(promise).producer() {
                    producer.acknowledge_propagated_failure();
                }
                RegionalWhnfStep::Failed(failure)
            }
            None => RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Deferred(
                WhnfDeferredRequest::Promise(promise.root_in(access.values())),
            )),
        },
        _ if work.0.frames.is_empty() => {
            RegionalWhnfStep::Ready(access.values().duplicate_value(&work.0.focus))
        }
        _ => resume_semantic_frame(access, work),
    }
}

enum DirectApplicationStep {
    Applied { value: Value, consumed: usize },
    Dictionary(crate::core::Dict),
    Failed(Arc<EvaluationFailure>),
}

fn resume_semantic_frame(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfState<'_>,
) -> RegionalWhnfStep {
    match work.frames.last() {
        Some(WhnfContinuation::SemanticUndefined { .. }) => {
            return resume_semantic_undefined(access, work);
        }
        Some(WhnfContinuation::Application { .. }) => {}
        Some(WhnfContinuation::DictionaryApplication { .. }) => {
            unreachable!("dictionary application must be evaluating an undefined candidate")
        }
        Some(WhnfContinuation::StaticAccess { .. }) => {
            return resume_static_access(access, work);
        }
        Some(WhnfContinuation::Generic(_)) => {
            unreachable!("W3C has not activated the remaining generic frame families")
        }
        None => unreachable!("a semantic frame resume requires one frame"),
    }

    let function = access.values().duplicate_value(&work.focus);
    let (arguments, next) = match work.frames.last() {
        Some(WhnfContinuation::Application { arguments, next }) => (arguments, *next),
        _ => unreachable!(),
    };
    let step = apply_whnf_callable(access, function, &arguments[next..]);
    match step {
        DirectApplicationStep::Applied { value, consumed } => {
            advance_application(access, work, value, consumed)
        }
        DirectApplicationStep::Dictionary(dict) => begin_dictionary_application(access, work, dict),
        DirectApplicationStep::Failed(failure) => RegionalWhnfStep::Failed(failure),
    }
}

fn resume_static_access(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfState<'_>,
) -> RegionalWhnfStep {
    let frame = work
        .frames
        .pop()
        .expect("static access must retain its path frame");
    let WhnfContinuation::StaticAccess { keys, next } = frame else {
        unreachable!()
    };
    let Value::Dict(dict) = &work.focus else {
        return RegionalWhnfStep::Failed(Arc::new(EvaluationFailure::message(
            "value access base is not a dictionary",
        )));
    };
    let value = dict
        .get(&keys[next])
        .map(|value| access.values().duplicate_value(value))
        .unwrap_or_else(|| Value::Dict(crate::core::Dict::new_sync()));
    let next = next + 1;
    if next < keys.len() {
        work.frames
            .push(WhnfContinuation::StaticAccess { keys, next });
    }
    RegionalWhnfStep::Delegate(value)
}

fn advance_application(
    _access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfState<'_>,
    value: Value,
    consumed: usize,
) -> RegionalWhnfStep {
    let Some(WhnfContinuation::Application { arguments, next }) = work.frames.last_mut() else {
        unreachable!("an application result requires its application frame")
    };
    *next += consumed;
    let complete = *next == arguments.len();
    if complete {
        work.frames.pop();
    }
    RegionalWhnfStep::Delegate(value)
}

fn begin_dictionary_application(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfState<'_>,
    dict: crate::core::Dict,
) -> RegionalWhnfStep {
    let diagnostic_kind = if dict.is_empty() { "Undefined" } else { "Dict" };
    let apply_member = dict
        .get(&*crate::core::keys::APPLY)
        .map(|value| access.values().duplicate_value(value));
    let Some(effect_payload) = dict
        .get(&*crate::core::keys::EFF)
        .map(|value| access.values().duplicate_value(value))
    else {
        return dictionary_application_fallback(access, apply_member, diagnostic_kind);
    };
    let remaining_effect_values = dict
        .iter()
        .filter(|(key, _)| *key != &*crate::core::keys::EFF)
        .map(|(_, value)| access.values().duplicate_value(value))
        .collect();
    let candidate = access.values().duplicate_value(&effect_payload);
    work.frames.push(WhnfContinuation::DictionaryApplication {
        effect_payload,
        remaining_effect_values,
        next_effect_value: 0,
        apply_member,
    });
    begin_semantic_undefined(access, work, candidate, UndefinedPurpose::EffectPayload)
}

fn begin_semantic_undefined(
    _access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfState<'_>,
    candidate: Value,
    purpose: UndefinedPurpose,
) -> RegionalWhnfStep {
    work.frames.push(WhnfContinuation::SemanticUndefined {
        purpose,
        ancestors: Vec::new(),
        phase: UndefinedPhase::Inspect,
    });
    RegionalWhnfStep::Delegate(candidate)
}

fn resume_semantic_undefined(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfState<'_>,
) -> RegionalWhnfStep {
    let frame = work
        .frames
        .pop()
        .expect("semantic-undefined work must retain its frame");
    let WhnfContinuation::SemanticUndefined {
        purpose,
        mut ancestors,
        phase,
    } = frame
    else {
        unreachable!()
    };
    match phase {
        UndefinedPhase::Inspect => {
            let retained_focus = access.values().duplicate_value(&work.focus);
            let Value::Dict(dict) = &retained_focus else {
                return finish_semantic_undefined(access, work, purpose, false);
            };
            let members = dict
                .iter()
                .map(|(_, value)| access.values().duplicate_value(value))
                .collect::<Vec<_>>();
            let Some(first) = members.first() else {
                work.frames.push(WhnfContinuation::SemanticUndefined {
                    purpose,
                    ancestors,
                    phase: UndefinedPhase::ReturnTrue,
                });
                return RegionalWhnfStep::Delegate(retained_focus);
            };
            let first = access.values().duplicate_value(first);
            ancestors.push(WhnfUndefinedDictionary { members, next: 1 });
            work.frames.push(WhnfContinuation::SemanticUndefined {
                purpose,
                ancestors,
                phase: UndefinedPhase::Inspect,
            });
            RegionalWhnfStep::Delegate(first)
        }
        UndefinedPhase::ReturnTrue => {
            let retained_focus = access.values().duplicate_value(&work.focus);
            let Some(ancestor) = ancestors.last_mut() else {
                return finish_semantic_undefined(access, work, purpose, true);
            };
            if ancestor.next < ancestor.members.len() {
                let next = access
                    .values()
                    .duplicate_value(&ancestor.members[ancestor.next]);
                ancestor.next += 1;
                work.frames.push(WhnfContinuation::SemanticUndefined {
                    purpose,
                    ancestors,
                    phase: UndefinedPhase::Inspect,
                });
                RegionalWhnfStep::Delegate(next)
            } else {
                ancestors.pop();
                work.frames.push(WhnfContinuation::SemanticUndefined {
                    purpose,
                    ancestors,
                    phase: UndefinedPhase::ReturnTrue,
                });
                RegionalWhnfStep::Delegate(retained_focus)
            }
        }
    }
}

fn finish_semantic_undefined(
    access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfState<'_>,
    purpose: UndefinedPurpose,
    undefined: bool,
) -> RegionalWhnfStep {
    let frame = work
        .frames
        .pop()
        .expect("dictionary application must underlie its undefined walk");
    let WhnfContinuation::DictionaryApplication {
        effect_payload,
        remaining_effect_values,
        mut next_effect_value,
        apply_member,
    } = frame
    else {
        unreachable!("semantic-undefined result must return to dictionary application")
    };

    let effect_rejected = match purpose {
        UndefinedPurpose::EffectPayload => undefined,
        UndefinedPurpose::EffectExtra => !undefined,
    };
    if effect_rejected {
        return dictionary_application_fallback(access, apply_member, "Dict");
    }
    if next_effect_value < remaining_effect_values.len() {
        let candidate = access
            .values()
            .duplicate_value(&remaining_effect_values[next_effect_value]);
        next_effect_value += 1;
        work.frames.push(WhnfContinuation::DictionaryApplication {
            effect_payload,
            remaining_effect_values,
            next_effect_value,
            apply_member,
        });
        return begin_semantic_undefined(access, work, candidate, UndefinedPurpose::EffectExtra);
    }

    let argument = match work.frames.last() {
        Some(WhnfContinuation::Application { arguments, next }) => {
            access.values().duplicate_value(&arguments[*next])
        }
        _ => unreachable!("dictionary application must retain its caller frame"),
    };
    let effect = super::application::effect_value(
        access.values(),
        super::application::apply_effect_function_value(access.values(), effect_payload, argument),
    );
    advance_application(access, work, effect, 1)
}

fn dictionary_application_fallback(
    access: &EvaluationValueAccess<'_>,
    apply_member: Option<Value>,
    diagnostic_kind: &str,
) -> RegionalWhnfStep {
    if let Some(apply_member) = apply_member
        && !super::value::is_undefined_dict_value(access.values(), &apply_member)
    {
        return RegionalWhnfStep::Delegate(apply_member);
    }
    RegionalWhnfStep::Failed(Arc::new(EvaluationFailure::message(format!(
        "application requires a function value, received {diagnostic_kind}"
    ))))
}

fn apply_whnf_callable(
    access: &EvaluationValueAccess<'_>,
    function: Value,
    arguments: &[Value],
) -> DirectApplicationStep {
    debug_assert!(!arguments.is_empty());
    match function {
        Value::Builtin(builtin) => apply_whnf_builtin(access, builtin, Vec::new(), arguments),
        Value::PartialBuiltin(call) => apply_whnf_builtin(
            access,
            call.builtin,
            call.arguments
                .iter()
                .map(|argument| access.values().duplicate_value(argument))
                .collect(),
            arguments,
        ),
        Value::Function(function) => apply_whnf_function(access, function, arguments),
        Value::Dict(dict) => DirectApplicationStep::Dictionary(dict),
        value => DirectApplicationStep::Failed(Arc::new(EvaluationFailure::message(format!(
            "application requires a function value, received {}",
            value.diagnostic_kind_name()
        )))),
    }
}

fn apply_whnf_builtin(
    access: &EvaluationValueAccess<'_>,
    builtin: crate::core::Builtin,
    mut supplied: Vec<Value>,
    arguments: &[Value],
) -> DirectApplicationStep {
    let remaining = builtin
        .arity()
        .checked_sub(supplied.len())
        .expect("a partial builtin cannot contain too many arguments");
    let consumed = remaining.min(arguments.len());
    supplied.extend(
        arguments[..consumed]
            .iter()
            .map(|argument| access.values().duplicate_value(argument)),
    );
    DirectApplicationStep::Applied {
        value: Value::builtin_call_in(access.values(), builtin, supplied),
        consumed,
    }
}

fn apply_whnf_function(
    access: &EvaluationValueAccess<'_>,
    function: FunctionValue,
    arguments: &[Value],
) -> DirectApplicationStep {
    let remaining = function.remaining_arity();
    let consumed = remaining.min(arguments.len());
    let saturating = arguments[..consumed]
        .iter()
        .map(|argument| access.values().duplicate_value(argument))
        .collect::<Vec<_>>();
    let value = if consumed < remaining {
        let stage = function.duplicate_stage_in(access.values());
        Value::Function(FunctionValue::new(
            super::net::attach_net_many_in(access.values(), stage, saturating),
            remaining - consumed,
        ))
    } else {
        Value::Lazy(LazyValue::from_function_call_in(
            access.values(),
            function,
            Arc::from(saturating),
        ))
    };
    DirectApplicationStep::Applied { value, consumed }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

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

    assert_does_not_implement!(whnf_computation_is_not_clone, WhnfComputation, Clone);

    #[allow(dead_code)]
    enum UnboxedCallableCheckpointPrototype {
        Existing(crate::interaction_net::RuntimeNode<crate::core_net::CoreSpecialization>),
        CallableCheckpoint(NetWhnfState),
    }

    #[allow(dead_code)]
    enum BoxedCallableCheckpointPrototype {
        Existing(crate::interaction_net::RuntimeNode<crate::core_net::CoreSpecialization>),
        CallableCheckpoint(Box<NetWhnfState>),
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn regional_whnf_and_callable_checkpoint_layout_baseline() {
        assert_send::<RegionalWhnfWork>();
        assert_send::<NetWhnfState>();
        assert_send::<BoxedCallableCheckpointPrototype>();

        let continuation_families = [
            WhnfContinuation::Generic(WhnfFrame {
                kind: WhnfFrameKind::DemandThenInspect,
                cursor: 0,
                retained: Vec::new(),
            }),
            WhnfContinuation::Application {
                arguments: Vec::new(),
                next: 0,
            },
            WhnfContinuation::DictionaryApplication {
                effect_payload: Value::Number(0.into()),
                remaining_effect_values: Vec::new(),
                next_effect_value: 0,
                apply_member: None,
            },
            WhnfContinuation::SemanticUndefined {
                purpose: UndefinedPurpose::EffectPayload,
                ancestors: Vec::new(),
                phase: UndefinedPhase::Inspect,
            },
            WhnfContinuation::StaticAccess {
                keys: Arc::from([]),
                next: 0,
            },
        ];
        for continuation in &continuation_families {
            assert_eq!(
                std::mem::size_of_val(continuation),
                std::mem::size_of::<WhnfContinuation>()
            );
        }

        let runtime_node = std::mem::size_of::<
            crate::interaction_net::RuntimeNode<crate::core_net::CoreSpecialization>,
        >();
        assert_eq!(
            std::mem::size_of::<Box<NetWhnfState>>(),
            std::mem::size_of::<usize>(),
            "the complete checkpoint remains one pointer in its runtime node"
        );
        assert_eq!(
            std::mem::size_of::<BoxedCallableCheckpointPrototype>(),
            runtime_node,
            "a boxed checkpoint must preserve the ordinary runtime-node extent"
        );
        assert!(
            std::mem::size_of::<UnboxedCallableCheckpointPrototype>() > runtime_node,
            "the baseline must keep detecting the current unboxed size-class increase"
        );

        #[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
        {
            assert_eq!(std::mem::size_of::<Value>(), 64);
            assert_eq!(std::mem::size_of::<WhnfFrame>(), 40);
            assert_eq!(std::mem::size_of::<WhnfUndefinedDictionary>(), 32);
            assert_eq!(std::mem::size_of::<WhnfContinuation>(), 160);
            assert_eq!(std::mem::size_of::<WhnfState>(), 128);
            assert_eq!(std::mem::size_of::<RegionalWhnfWork>(), 128);
            assert_eq!(std::mem::size_of::<NetWhnfState>(), 128);
            assert_eq!(runtime_node, 96);
            assert_eq!(
                std::mem::size_of::<UnboxedCallableCheckpointPrototype>(),
                128
            );
        }
    }

    #[test]
    fn root_per_field_whnf_compatibility_is_retired() {
        let source = include_str!("whnf.rs");
        let production = source
            .split_once("#[cfg(test)]\nmod tests")
            .expect("WHNF source should retain its test boundary")
            .0;
        for retired in [
            "struct DurableWhnfState",
            "struct DurableWhnfFrame",
            "enum DurableWhnfContinuation",
            "struct DurableUndefinedDictionary",
            "LegacyDemand",
            "fn publish_checkpoint",
            "fn root_continuation",
        ] {
            assert!(
                !production.contains(retired),
                "W6G.3f must retire `{retired}` instead of leaving an unreachable compatibility path"
            );
        }
    }

    #[test]
    fn whnf_protocol_remains_private_and_has_only_named_production_owners() {
        let source = include_str!("whnf.rs");
        for declaration in [
            "pub(crate) struct WhnfComputation",
            "pub(crate) enum WhnfFrameKind",
            "pub(crate) enum WhnfDependency",
            "pub(crate) enum WhnfPoll",
        ] {
            assert!(
                source.contains(declaration),
                "WHNF protocol declaration must remain crate-private: {declaration}"
            );
        }
        let task_machine_impl = ["impl EvaluationTaskMachine", " for WhnfComputation"].concat();
        assert!(!source.contains(&task_machine_impl));

        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let core = fs::read_to_string(manifest.join("src/core.rs"))
            .expect("core value source should be readable");
        assert!(
            !core.contains("WhnfComputation"),
            "WHNF progress must not become a Value or LazySource variant"
        );
        let deferred = fs::read_to_string(manifest.join("src/eval/value.rs"))
            .expect("deferred evaluator source should be readable");
        assert!(deferred.contains("computation: super::whnf::WhnfComputation"));
        let client =
            fs::read_to_string(manifest.join("src/evaluation/coordinator/client_demand.rs"))
                .expect("client-demand source should be readable");
        assert!(
            client.contains("ClientDemandOperation(pub(in crate::evaluation) WhnfComputation)")
        );

        let reflection = fs::read_to_string(manifest.join("src/reflection/machine.rs"))
            .expect("reflection machine source should be readable");
        assert!(
            reflection.contains("computation: WhnfComputation"),
            "W5 reflection decoding must own its resumable WHNF computation"
        );
    }

    #[test]
    fn selected_frame_protocol_is_compile_exhaustive() {
        fn classify(kind: WhnfFrameKind) -> usize {
            match kind {
                WhnfFrameKind::DemandThenInspect => 0,
                WhnfFrameKind::OrderedOperands => 1,
                WhnfFrameKind::CollectionWalk => 2,
                WhnfFrameKind::Application => 3,
                WhnfFrameKind::KeyConversion => 4,
                WhnfFrameKind::AccessPath => 5,
                WhnfFrameKind::DiagnosticContext => 6,
            }
        }

        assert_eq!(classify(WhnfFrameKind::DiagnosticContext), 6);
    }
}

#[cfg(test)]
#[path = "whnf/tests/w1b.rs"]
mod w1b_tests;

#[cfg(test)]
#[path = "whnf/tests/w1c.rs"]
mod w1c_tests;

#[cfg(test)]
#[path = "whnf/tests/w2a.rs"]
mod w2a_tests;

#[cfg(test)]
#[path = "whnf/tests/w2b.rs"]
mod w2b_tests;

#[cfg(test)]
#[path = "whnf/tests/w3b_application.rs"]
mod w3b_application_tests;

#[cfg(test)]
#[path = "whnf/tests/w3c_access.rs"]
mod w3c_access_tests;

#[cfg(test)]
#[path = "whnf/tests/nc1.rs"]
mod nc1_tests;

#[cfg(test)]
#[path = "whnf/tests/w6g3a.rs"]
mod w6g3a_tests;

#[cfg(test)]
#[path = "whnf/tests/w6g3b.rs"]
mod w6g3b_tests;

#[cfg(test)]
#[path = "whnf/tests/w6g3c.rs"]
mod w6g3c_tests;

#[cfg(test)]
#[path = "whnf/tests/w6g3e.rs"]
mod w6g3e_tests;

#[cfg(test)]
#[path = "whnf/tests/w6g1f1.rs"]
mod w6g1f1_tests;
