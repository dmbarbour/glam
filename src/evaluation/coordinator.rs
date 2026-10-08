//! Runtime-owned work coordination independent of worker ownership.

use std::collections::VecDeque;

use crate::trusted_hash::{TrustedHashMap, TrustedHashSet};
use std::fmt;
use std::num::NonZeroU64;
#[cfg(test)]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use crate::counted_condvar::CountedCondvar;
use std::time::Duration;

#[cfg(test)]
use crate::core::LazyValue;
#[cfg(test)]
use crate::core::PromisedValue;
use crate::core::{
    CoreValueFactory, EvaluationFailure, EvaluationPanic, ManagedPromiseRoot, PromiseAssignment,
    PromiseId, RuntimeValueAccess,
};
use crate::runtime::{
    EvaluationRuntimeId, RuntimeCoreState, RuntimeIds, RuntimeMutationAdmission,
    RuntimeMutationAuthority, RuntimeMutationGuard, RuntimeValueRoot,
};

#[cfg(test)]
use super::EvaluationSession;
use super::{EvaluationDemandState, RuntimeObservationEpoch, RuntimeObservationState};

mod client_demand;
mod completion;
mod deferred;
#[cfg(test)]
mod generation_inventory;
mod reflection;
#[cfg(test)]
mod registry_inventory;
mod settlement;
mod spark;
mod task;
pub(crate) use client_demand::{
    ClaimedClientDemand, ClientDemandHandle, ClientDemandOperation, ClientDemandPoll,
    ClientDemandResult, ClientDemandSink, ClientDemandSnapshot,
};
use client_demand::{
    ClientDemandRecord, ClientDemandRetirement, detach_client_demand, queue_client_demand,
};
#[cfg(test)]
use completion::DependencyWakeBatch;
pub(crate) use completion::{
    CompletionSubscriptionOutcome, CompletionSubscriptions, CompletionWake, WakeRegistration,
    WorkDependencyKey,
};
pub(super) use deferred::{
    AbandonedDeferredWork, ClaimedDeferredWork, ClaimedLazyRoute, DeferredLazyCycleMember,
    DeferredProducer, DeferredWorkPoll, DeferredWorkReservation,
};
use deferred::{
    DeferredIndexes, DeferredWork, LazyRouteWork, begin_deferred_abandonment, claim_deferred,
    claim_lazy_route, deferred_work_mut,
};
#[cfg(test)]
pub(super) use reflection::ReflectionWorkSnapshot;
#[cfg(test)]
use reflection::reflection_work;
use reflection::{
    AbandonedReflectionWork, ReflectionIndexes, ReflectionWork, claim_reflection,
    insert_task_failure, reflection_work_mut, remove_ready_reflection,
};
pub(super) use reflection::{
    ClaimedReflectionWork, ReflectionCancellation, ReflectionWorkPoll, ReflectionWorkState,
};
pub(crate) use settlement::{
    RuntimeCoordinatorReadiness, RuntimeDeadlockWorkSnapshot, RuntimeDependencySnapshot,
    RuntimeExitSnapshot, RuntimeWorkKindSnapshot, RuntimeWorkStateSnapshot,
    ValidatedRuntimeSettlementPlan,
};
#[cfg(test)]
use spark::spark_work_mut;
pub(crate) use spark::{ClaimedSparkWork, SparkWork, SparkWorkPoll};
use spark::{SparkRetirement, claim_spark, detach_spark};
pub(crate) use task::{
    EvaluationExitBlock, EvaluationMachinePoll, EvaluationSessionId, EvaluationTaskBlock,
    EvaluationTaskCancellation, EvaluationTaskHandle, EvaluationTaskId, EvaluationTaskMachine,
    EvaluationTaskStatus, EvaluationWaitPoll, EvaluationWaitTerminal, EvaluationWaitToken,
    ExitIntent, InitialTaskDisposition, LocalPromiseOwner, PendingTaskPolicy,
    PreparedEvaluationTask, PromiseProducerObligation, PromiseProducerPublication,
    ReflectionTaskResultPolicy, RuntimeFailureLedger, TaskFailureLedger, TaskStatusPublisher,
    TaskStatusWake,
};
use task::{TaskStatusUpdate, TaskTerminalPublisher, terminal_task_status};

impl RuntimeCoreState for EvaluationWorkCoordinator {
    fn core_poisoned(&self) -> bool {
        self.state.is_poisoned()
    }

    fn wake_parked(&self) {
        // Parked waiters relock the poisoned state and fail loudly.
        self.notify_all(CoordinatorMutationKind::RuntimePoison);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EvaluationWorkId(NonZeroU64);

impl EvaluationWorkId {
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

type TrustedWorkIdSet = TrustedHashSet<EvaluationWorkId>;
type TrustedWorkIdMap<V> = TrustedHashMap<EvaluationWorkId, V>;

/// What a published coordinator mutation can have changed in a retained
/// exact route.
#[derive(Debug, Clone, Copy)]
pub(super) enum RouteHazard<'a> {
    /// The mutation's kind cannot affect a retained route.
    None,
    /// These works' route-visible state changed: their state, subscription
    /// epoch or dependency, or a wait-index entry that maps to them. A route
    /// revalidates only from its lowest touched frame.
    Works(&'a [EvaluationWorkId]),
    /// The mutation cannot name what it changed; every route revalidates in
    /// full.
    All,
}

/// Route hazards retained for incremental validation. A route whose last
/// validation predates the oldest retained hazard validates in full.
const EXACT_ROUTE_HAZARD_LOG_CAPACITY: usize = 4096;

#[cfg(test)]
pub(crate) fn test_wake_registration() -> WakeRegistration {
    WakeRegistration {
        work: EvaluationWorkId(NonZeroU64::MAX),
        subscription_epoch: 0,
    }
}

#[derive(Default)]
struct WorkControl {
    close_reason: Option<WorkCloseReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkCloseReason {
    ExplicitCancellation,
    ClientDemandAbandoned,
    DemandSessionClosed,
    ExecutorShutdown,
}

enum ProducerSettlementObligation {
    ReflectionTask(TaskTerminalPublisher),
    DeferredClaim {
        wait: EvaluationWaitToken,
        producer: DeferredProducer,
    },
}

/// Producer state which must be disposed before a work record retires.
///
/// Ordinary terminalization consumes the static producer entry once, publishes
/// every task terminal surface, then settles dynamically registered promises
/// before the work record may retire.
#[derive(Default)]
struct SettlementObligations {
    producer: Option<ProducerSettlementObligation>,
    owned_promises: Vec<TaskOwnedPromiseObligation>,
}

#[derive(Clone)]
struct TaskOwnedPromiseObligation {
    promise: PromiseId,
    root: ManagedPromiseRoot,
    producer: Arc<PromiseProducerObligation>,
    wait: EvaluationWaitToken,
    terminal: TaskPromiseTerminalMapper,
}

#[derive(Clone)]
pub(crate) enum TaskPromiseTerminalMapper {
    UnresolvedFailure,
    ReflectionReturnValue,
    ReflectionGate { target: RuntimeValueRoot },
}

impl TaskPromiseTerminalMapper {
    fn assignment(
        &self,
        access: &RuntimeValueAccess<'_>,
        terminal: &EvaluationWaitTerminal,
        unresolved_failure: &Arc<EvaluationFailure>,
    ) -> PromiseAssignment {
        if matches!(terminal, EvaluationWaitTerminal::Panicked(_)) {
            unreachable!("a panicked producer records its panic instead of assigning promises");
        }
        let operation = match self {
            Self::UnresolvedFailure => return Err(unresolved_failure.clone()),
            Self::ReflectionReturnValue => "reflection_task",
            Self::ReflectionGate { .. } => "reflection_annotation",
        };

        match terminal {
            EvaluationWaitTerminal::Complete(value) => match self {
                Self::ReflectionReturnValue => Ok(value.clone_core_with(access)),
                Self::ReflectionGate { target } => Ok(target.clone_core_with(access)),
                Self::UnresolvedFailure => unreachable!("handled above"),
            },
            EvaluationWaitTerminal::Failed(failure) | EvaluationWaitTerminal::Killed(failure) => {
                Err(Arc::new(failure.as_failure().with_context_in(
                    access,
                    crate::diagnostic::evaluation_context_frame_in(access, operation),
                )))
            }
            EvaluationWaitTerminal::Cancelled => Err(reflection_terminal_failure(
                access,
                operation,
                match self {
                    Self::ReflectionReturnValue => "reflection result task was cancelled",
                    Self::ReflectionGate { .. } => "reflection annotation task was cancelled",
                    Self::UnresolvedFailure => unreachable!("handled above"),
                },
            )),
            EvaluationWaitTerminal::Abandoned => Err(reflection_terminal_failure(
                access,
                operation,
                "reflection task was abandoned when its evaluation session closed",
            )),
            EvaluationWaitTerminal::Exited => Err(reflection_terminal_failure(
                access,
                operation,
                "reflection task exited without producing a result",
            )),
            EvaluationWaitTerminal::Panicked(_) => unreachable!("rejected above"),
        }
    }
}

fn reflection_terminal_failure(
    access: &RuntimeValueAccess<'_>,
    operation: &str,
    message: &str,
) -> Arc<EvaluationFailure> {
    Arc::new(EvaluationFailure::message(message).with_context_in(
        access,
        crate::diagnostic::evaluation_context_frame_in(access, operation),
    ))
}

impl TaskOwnedPromiseObligation {
    fn publish_terminal_guarded(
        self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        terminal: &EvaluationWaitTerminal,
        unresolved_failure: &Arc<EvaluationFailure>,
    ) -> (PromiseProducerPublication, CompletionWake) {
        let values = coordinator
            .value_observer()
            .upgrade()
            .expect("a registered promise root must retain a live value domain owner");
        let (publication, wake) = values.with_runtime_value_access(|access| {
            let assignment = self
                .terminal
                .assignment(&access, terminal, unresolved_failure);
            self.root
                .publish_guarded(&access, coordinator, mutation, assignment, |assignment| {
                    self.producer.publish_assignment_guarded(
                        &access,
                        coordinator,
                        mutation,
                        assignment,
                    )
                })
                .unwrap_or_else(|_| {
                    panic!("a terminalizing task-owned promise must remain unresolved")
                })
        });
        (publication.retain_snapshot_root(self.root), wake)
    }

    /// Publishes a panicked producer without assigning the promise.
    fn publish_panicked_guarded(
        self,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        mutation: &dyn RuntimeMutationAuthority,
        report: &EvaluationPanic,
    ) -> (PromiseProducerPublication, CompletionWake) {
        let (publication, wake) =
            self.root
                .publish_producer_panic_guarded(coordinator, mutation, || {
                    self.producer
                        .publish_panic_guarded(coordinator, mutation, report)
                });
        (publication.retain_snapshot_root(self.root), wake)
    }
}

impl SettlementObligations {
    fn reflection_task(wait: EvaluationWaitToken) -> Self {
        Self {
            producer: Some(ProducerSettlementObligation::ReflectionTask(
                TaskTerminalPublisher::new(wait),
            )),
            owned_promises: Vec::new(),
        }
    }

    fn deferred_claim(wait: EvaluationWaitToken, producer: DeferredProducer) -> Self {
        Self {
            producer: Some(ProducerSettlementObligation::DeferredClaim { wait, producer }),
            owned_promises: Vec::new(),
        }
    }

    fn take_producer(&mut self) -> Option<ProducerSettlementObligation> {
        self.producer.take()
    }

    fn task_publisher_mut(&mut self) -> Option<&mut TaskTerminalPublisher> {
        match self.producer.as_mut()? {
            ProducerSettlementObligation::ReflectionTask(publisher) => Some(publisher),
            ProducerSettlementObligation::DeferredClaim { .. } => None,
        }
    }

    fn add_owned_promise(&mut self, obligation: TaskOwnedPromiseObligation) {
        self.owned_promises.push(obligation);
    }

    fn take_owned_promise(
        &mut self,
        wait: &EvaluationWaitToken,
        promise: PromiseId,
    ) -> Option<TaskOwnedPromiseObligation> {
        let index = self
            .owned_promises
            .iter()
            .position(|obligation| obligation.wait == *wait && obligation.promise == promise)?;
        Some(self.owned_promises.swap_remove(index))
    }

    fn is_empty(&self) -> bool {
        self.producer.is_none() && self.owned_promises.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkState {
    Dormant,
    Reserved,
    Queued,
    Running,
    /// A lazy route admitted while another route forces its lazy inline. It
    /// is busy, like `Running`, until that inline claim ends; see
    /// `EvaluationWorkCoordinator::release_inline_lazy`.
    InlineForced,
    Blocked,
    ExitWaiting,
    Terminalizing,
}

#[derive(Clone)]
pub(crate) enum WorkDependency {
    Wait(EvaluationWaitToken),
    Promise(ManagedPromiseRoot),
    #[cfg(test)]
    Test(TestWorkDependency),
}

impl WorkDependency {
    fn runtime_id(&self) -> EvaluationRuntimeId {
        match self {
            Self::Wait(wait) => wait.runtime_id(),
            Self::Promise(promise) => promise.runtime_id(),
            #[cfg(test)]
            Self::Test(dependency) => dependency.runtime,
        }
    }

    fn key(&self) -> WorkDependencyKey {
        match self {
            Self::Wait(wait) => WorkDependencyKey::Wait(wait.get()),
            Self::Promise(promise) => WorkDependencyKey::Promise(promise.id().get()),
            #[cfg(test)]
            Self::Test(dependency) => WorkDependencyKey::Test(dependency.id.get()),
        }
    }

    fn same_source(&self, other: &Self) -> bool {
        self.runtime_id() == other.runtime_id() && self.key() == other.key()
    }

    /// The producer wait through which scheduler graph traversal can continue.
    ///
    /// Resolver-owned promises have no producer edge. Task-owned promises
    /// project through the producer obligation while retaining the promise as
    /// the exact completion source in the machine block.
    pub(super) fn producer_wait(&self) -> Option<EvaluationWaitToken> {
        match self {
            Self::Wait(wait) => Some(wait.clone()),
            Self::Promise(promise) => promise.producer().and_then(|task| task.try_wait()),
            #[cfg(test)]
            Self::Test(_) => None,
        }
    }

    pub(super) fn into_wait(self) -> Option<EvaluationWaitToken> {
        match self {
            Self::Wait(wait) => Some(wait),
            Self::Promise(_) => None,
            #[cfg(test)]
            Self::Test(_) => None,
        }
    }

    fn subscribe_work(
        &self,
        runtime: EvaluationRuntimeId,
        registration: WakeRegistration,
    ) -> CompletionSubscriptionOutcome {
        match self {
            Self::Wait(wait) => wait.subscribe_work(runtime, registration),
            Self::Promise(promise) => promise.subscribe_work(runtime, registration),
            #[cfg(test)]
            Self::Test(_) => {
                unreachable!("synthetic completion sources install their own subscription")
            }
        }
    }

    fn unsubscribe_work(&self, registration: WakeRegistration) -> bool {
        match self {
            Self::Wait(wait) => wait.unsubscribe_work(registration),
            Self::Promise(promise) => promise.unsubscribe_work(registration),
            #[cfg(test)]
            Self::Test(_) => false,
        }
    }

    /// Whether this dependency can no longer change: its wait is terminal,
    /// its promise is assigned, or its promise's producer panicked.
    pub(crate) fn is_terminal(&self) -> bool {
        match self {
            Self::Wait(wait) => wait.terminal_poll().is_some(),
            Self::Promise(promise) => {
                promise.is_terminal()
                    || promise
                        .producer()
                        .is_some_and(|producer| producer.panic_report().is_some())
            }
            #[cfg(test)]
            Self::Test(_) => false,
        }
    }

    /// The panic that interrupted this dependency's producer, if any.
    ///
    /// A waiter on a panicked producer halts instead of reinstalling work,
    /// which would only rerun the panic.
    pub(crate) fn panic_report(&self) -> Option<EvaluationPanic> {
        match self {
            Self::Wait(wait) => match wait.terminal_poll()? {
                EvaluationWaitPoll::Panicked(report) => Some(report),
                _ => None,
            },
            Self::Promise(promise) => promise.producer()?.panic_report().cloned(),
            #[cfg(test)]
            Self::Test(_) => None,
        }
    }

    fn abandon(self) {
        match self {
            Self::Wait(wait) => wait.abandon_deferred_producer(),
            Self::Promise(_) => {}
            #[cfg(test)]
            Self::Test(_) => {}
        }
    }
}

impl PartialEq for WorkDependency {
    fn eq(&self, other: &Self) -> bool {
        self.same_source(other)
    }
}

impl Eq for WorkDependency {}

impl fmt::Debug for WorkDependency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wait(wait) => formatter.debug_tuple("Wait").field(wait).finish(),
            Self::Promise(promise) => formatter.debug_tuple("Promise").field(promise).finish(),
            #[cfg(test)]
            Self::Test(dependency) => formatter.debug_tuple("Test").field(dependency).finish(),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct TestWorkDependency {
    runtime: EvaluationRuntimeId,
    id: NonZeroU64,
}

enum WorkKind {
    Spark(SparkWork),
    Reflection(ReflectionWork),
    Deferred(DeferredWork),
    LazyRoute(LazyRouteWork),
}

struct WorkRecord {
    id: EvaluationWorkId,
    demand_session: EvaluationSessionId,
    subscription_epoch: u64,
    control: WorkControl,
    obligations: SettlementObligations,
    state: WorkState,
    kind: WorkKind,
}

/// Temporary strong route from one detached work claim to its demand domain.
///
/// The coordinator registry remains weak. A claim upgrades that registry only
/// while its machine or operation is detached, so later scheduler admission
/// has one authoritative source for the matching value domain without adding
/// another durable coordinator-to-domain edge.
pub(super) struct ClaimedDemandSession {
    demand: Arc<EvaluationDemandState>,
}

impl ClaimedDemandSession {
    fn registered(
        state: &WorkCoordinatorState,
        session: EvaluationSessionId,
        runtime: EvaluationRuntimeId,
    ) -> Option<Self> {
        let demand = state.demand_sessions.get(&session)?.upgrade()?;
        if demand.is_closed() {
            return None;
        }
        if demand.id != session || demand.values.runtime_id() != runtime {
            return None;
        }
        Some(Self { demand })
    }

    pub(in crate::evaluation) fn id(&self) -> EvaluationSessionId {
        self.demand.id
    }

    pub(in crate::evaluation) fn demand(&self) -> Arc<EvaluationDemandState> {
        self.demand.clone()
    }

    pub(in crate::evaluation) fn values(&self) -> &CoreValueFactory {
        &self.demand.values
    }

    pub(in crate::evaluation) fn assert_runtime(&self, runtime: EvaluationRuntimeId) {
        assert_eq!(
            self.values().runtime_id(),
            runtime,
            "claimed demand session must match the polling coordinator runtime"
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservationRegistration {
    wake: WakeRegistration,
    observed_epoch: RuntimeObservationEpoch,
}

pub(super) enum ClaimedTaskWork {
    Reflection(ClaimedReflectionWork),
    Deferred(ClaimedDeferredWork),
    LazyRoute(ClaimedLazyRoute),
}

impl ClaimedTaskWork {
    pub(in crate::evaluation) fn id(&self) -> EvaluationWorkId {
        match self {
            Self::Reflection(work) => work.id,
            Self::Deferred(work) => work.id,
            Self::LazyRoute(work) => work.id,
        }
    }

    pub(in crate::evaluation) fn demand(&self) -> &ClaimedDemandSession {
        match self {
            Self::Reflection(work) => &work.demand,
            Self::Deferred(work) => &work.demand,
            Self::LazyRoute(work) => &work.demand,
        }
    }
}

pub(super) struct SessionClosureWork {
    pub(super) reflection: Vec<AbandonedReflectionWork>,
    pub(super) deferred: Vec<AbandonedDeferredWork>,
    retired_sparks: Vec<SparkRetirement>,
    client_demands: Vec<ClientDemandRetirement>,
}

impl SessionClosureWork {
    fn finish_sparks(&mut self) {
        for record in self.retired_sparks.drain(..) {
            record.abandon();
        }
    }

    fn finish_client_demands(&mut self) {
        for record in self.client_demands.drain(..) {
            record.finish();
        }
    }

    pub(super) fn finish(mut self) {
        self.finish_sparks();
        self.finish_client_demands();
    }
}

impl Drop for SessionClosureWork {
    fn drop(&mut self) {
        self.finish_sparks();
        self.finish_client_demands();
    }
}

impl ClaimedClientDemand {
    pub(super) fn poll(
        &mut self,
        poll_context: &super::EvaluationPollContext,
        step_budget: &mut super::EvaluationStepBudget,
    ) -> ClientDemandPoll {
        assert_eq!(
            self.operation
                .as_ref()
                .expect("claimed client demand must retain its operation")
                .runtime_id(),
            self.demand.values().runtime_id(),
            "client-demand operation must match its claimed demand session"
        );
        let originating_task = self
            .operation
            .as_ref()
            .expect("claimed client demand must retain its operation")
            .originating_task();
        let context = super::EvalContext::for_client_demand(self.demand.demand(), originating_task);
        let operation = self
            .operation
            .as_mut()
            .expect("claimed client demand must retain its operation");
        operation.poll(poll_context, &context, step_budget)
    }
}

#[derive(Default)]
struct WorkCoordinatorState {
    demand_sessions: TrustedHashMap<EvaluationSessionId, Weak<EvaluationDemandState>>,
    failures: RuntimeFailureLedger,
    pending_failure_reports: RuntimeFailureLedger,
    work: TrustedHashMap<EvaluationWorkId, WorkRecord>,
    work_by_session: TrustedHashMap<EvaluationSessionId, TrustedHashSet<EvaluationWorkId>>,
    client_demands: TrustedHashMap<EvaluationWorkId, ClientDemandRecord>,
    client_demands_by_session:
        TrustedHashMap<EvaluationSessionId, TrustedHashSet<EvaluationWorkId>>,
    ready_tasks: VecDeque<EvaluationWorkId>,
    ready_task_set: TrustedHashSet<EvaluationWorkId>,
    background_roots: VecDeque<EvaluationWorkId>,
    ready_client_demands: VecDeque<EvaluationWorkId>,
    ready_client_demand_set: TrustedHashSet<EvaluationWorkId>,
    reflection: ReflectionIndexes,
    deferred: DeferredIndexes,
    promise_by_wait: TrustedHashMap<EvaluationWaitToken, EvaluationWorkId>,
    observation_waiters: TrustedHashMap<EvaluationWorkId, ObservationRegistration>,
    spark_workers: usize,
    prefer_spark: bool,
    /// Broad scheduler/readiness revision observed by host wait loops.
    work_generation: u64,
    /// Narrow revision for mutations which can invalidate a retained exact
    /// producer route. This is not semantic state and is never exposed. Each
    /// entry of `exact_route_hazards` advances it by one.
    exact_route_hazard_revision: u64,
    /// The most recent route hazards, oldest first: a touched work, or `None`
    /// for a mutation that touched every route.
    exact_route_hazards: VecDeque<Option<EvaluationWorkId>>,
    /// Profiling: route frames that hazard validations walked.
    #[cfg(any(test, feature = "glam-prof"))]
    exact_route_validated_frames: std::sync::atomic::AtomicU64,
}

/// Factual source of one broad coordinator revision publication.
///
/// Variants describe the state transition, not route policy;
/// `affects_exact_route` decides which can invalidate a retained route.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoordinatorMutationKind {
    DemandSessionRegistry,
    ExecutorAvailability,
    FreshWorkAdmission,
    WorkActivation,
    ClientDemandAdmission,
    TaskPromiseIndexAdmission,
    TaskPromiseIndexRetirement,
    WorkClaim,
    WorkRequeue,
    WorkRelease,
    ClientDemandRelease,
    DependencyPromotion,
    DependencyWake,
    ObservationWake,
    Cancellation,
    SessionClosure,
    TerminalSettlement,
    WorkRetirement,
    FailureLedger,
    StageSettlement,
    /// A panic tore runtime-core state; parked waiters must observe it.
    RuntimePoison,
    #[cfg(test)]
    WorkPark,
    #[cfg(test)]
    TestTransition,
}

impl CoordinatorMutationKind {
    /// Whether this transition can change the target-to-tip projection of a
    /// retained exact route.
    const fn affects_exact_route(self) -> bool {
        match self {
            Self::TaskPromiseIndexRetirement
            | Self::WorkRelease
            | Self::DependencyWake
            | Self::ObservationWake
            | Self::Cancellation
            | Self::SessionClosure
            | Self::TerminalSettlement
            | Self::WorkRetirement
            | Self::StageSettlement => true,
            #[cfg(test)]
            Self::WorkPark | Self::TestTransition => true,
            _ => false,
        }
    }

    /// Whether publication can satisfy at least one predicate parked on the
    /// coordinator's shared condition variable.
    const fn notifies_waiters(self) -> bool {
        !matches!(
            self,
            Self::DemandSessionRegistry
                | Self::TaskPromiseIndexAdmission
                | Self::TaskPromiseIndexRetirement
                | Self::WorkClaim
                | Self::FailureLedger
        )
    }
}

#[cfg(any(test, feature = "glam-prof"))]
const PRODUCTION_COORDINATOR_MUTATION_KIND_COUNT: usize = 21;
#[cfg(all(test, feature = "glam-prof"))]
const COORDINATOR_MUTATION_KIND_COUNT: usize = 23;
#[cfg(all(test, not(feature = "glam-prof")))]
const COORDINATOR_MUTATION_KIND_COUNT: usize = 23;
#[cfg(all(not(test), feature = "glam-prof"))]
const COORDINATOR_MUTATION_KIND_COUNT: usize = 21;

impl WorkCoordinatorState {
    /// Publishes one scheduler-visible mutation.
    ///
    /// Keep every production revision advance behind this boundary so each
    /// one is classified by a `CoordinatorMutationKind` as coordinator paths
    /// are added or reorganized.
    ///
    /// A route-affecting kind names what it changed, so retained exact routes
    /// revalidate only from their lowest touched frame; every other kind
    /// passes `RouteHazard::None`.
    fn advance_work_generation(&mut self, kind: CoordinatorMutationKind, hazard: RouteHazard<'_>) {
        debug_assert_eq!(
            kind.affects_exact_route(),
            !matches!(hazard, RouteHazard::None),
            "{kind:?} must name its exact-route hazard exactly when it can affect a route"
        );
        self.work_generation = self.work_generation.wrapping_add(1);
        match hazard {
            RouteHazard::None => {}
            RouteHazard::Works(works) => {
                for work in works {
                    self.record_exact_route_hazard(Some(*work));
                }
            }
            RouteHazard::All => self.record_exact_route_hazard(None),
        }
    }

    fn record_exact_route_hazard(&mut self, work: Option<EvaluationWorkId>) {
        if self.exact_route_hazards.len() == EXACT_ROUTE_HAZARD_LOG_CAPACITY {
            self.exact_route_hazards.pop_front();
        }
        self.exact_route_hazards.push_back(work);
        self.exact_route_hazard_revision = self.exact_route_hazard_revision.wrapping_add(1);
    }

    /// The lowest frame of `route` that a hazard since `validated` may have
    /// changed, or `None` when no hazard touched a member. Frame `i` is
    /// `route.parents[i]`, and the current tip is frame `parents.len()`.
    fn exact_route_hazard_start(&self, route: &ExactDemandRoute, validated: u64) -> Option<usize> {
        let since = usize::try_from(self.exact_route_hazard_revision.wrapping_sub(validated))
            .unwrap_or(usize::MAX);
        if since > self.exact_route_hazards.len() {
            return Some(0);
        }
        let mut lowest = None;
        for hazard in self.exact_route_hazards.iter().rev().take(since) {
            let Some(work) = hazard else {
                return Some(0);
            };
            if let Some(depth) = route.members.get(work) {
                lowest = Some(lowest.map_or(*depth, |lowest: usize| lowest.min(*depth)));
            }
        }
        lowest
    }
}

/// Runtime-owned scheduling state shared by serial and worker execution.
///
/// Spark payloads and reflection/deferred lifecycle records, including their
/// claimable machine slots, have stable work records here. Session reporting
/// registrations retain only weak demand-state liveness and closure state.
pub(crate) struct EvaluationWorkCoordinator {
    runtime: EvaluationRuntimeId,
    values: crate::core::RuntimeValueObserver,
    ids: Arc<RuntimeIds>,
    admission: Arc<RuntimeMutationAdmission>,
    observations: Arc<RuntimeObservationState>,
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    background_demand: Mutex<Option<Arc<EvaluationDemandState>>>,
    state: Mutex<WorkCoordinatorState>,
    work_available: CountedCondvar,
    #[cfg(test)]
    work_wait_probe: OnceLock<std::sync::mpsc::Sender<()>>,
    #[cfg(test)]
    test_values: Option<CoreValueFactory>,
    #[cfg(test)]
    terminal_publication_probe: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    reflection_release_status_probe: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(any(test, feature = "glam-prof"))]
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    exact_route_profile: Mutex<ExactDemandRouteProfile>,
    #[cfg(any(test, feature = "glam-prof"))]
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    exact_route_mutation_profile: Mutex<ExactRouteMutationProfile>,
    #[cfg(any(test, feature = "glam-prof"))]
    /// Leaf lock: critical sections make only whole updates, so poison is recovered.
    notification_profile: Mutex<CoordinatorNotificationProfile>,
    #[cfg(test)]
    exact_selection_probe: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// Test-owned accounting for exact-route discovery and handoff.
///
/// Ordinary builds contain neither this state nor updates to it. Tests and the
/// static interaction-net profiling feature expose it without installing a
/// dynamic observer on the scheduler path.
#[cfg(any(test, feature = "glam-prof"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ExactDemandRouteProfile {
    pub(super) complete_searches: usize,
    pub(super) edges_visited: usize,
    pub(super) maximum_depth: usize,
    pub(super) fast_handoffs: usize,
    pub(super) o1_accepted_releases: usize,
    pub(super) hazard_validations: usize,
    pub(super) successful_hazard_validations: usize,
    pub(super) failed_hazard_validations: usize,
    pub(super) checkpoint_invalidations: usize,
    pub(super) cold_fallbacks: usize,
    pub(super) missing_release_fallbacks: usize,
    pub(super) current_work_mismatch_fallbacks: usize,
    pub(super) guarded_release_mutation_fallbacks: usize,
    pub(super) changed_dependency_fallbacks: usize,
    pub(super) retired_work_fallbacks: usize,
    pub(super) branched_work_fallbacks: usize,
}

#[cfg(any(test, feature = "glam-prof"))]
#[derive(Debug, Default)]
struct ExactRouteMutationProfile {
    moved_poll_windows: u64,
    o1_accepted_releases: u64,
    hazard_validations: u64,
    successful_hazard_validations: u64,
    failed_hazard_validations: u64,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CoordinatorWaiterClass {
    Worker,
    ExactClient,
    SessionDrain,
    TaskObserver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CoordinatorWaiterOutcome {
    Productive,
    Relevant,
    Unrelated,
}

#[cfg(any(test, feature = "glam-prof"))]
#[derive(Debug, Default)]
struct CoordinatorNotificationProfile {
    notify_one: [u64; COORDINATOR_MUTATION_KIND_COUNT],
    notify_all: [u64; COORDINATOR_MUTATION_KIND_COUNT],
    released: [u64; 4],
    productive: [u64; 4],
    relevant: [u64; 4],
    unrelated: [u64; 4],
}

#[cfg(any(test, feature = "glam-prof"))]
impl CoordinatorNotificationProfile {
    fn snapshot(
        &self,
    ) -> crate::interaction_net::profiling::CoordinatorNotificationProfileSnapshot {
        use crate::interaction_net::profiling::{
            CoordinatorNotificationCallCounts, CoordinatorNotificationProfileSnapshot,
            CoordinatorWaiterOutcomeCounts,
        };

        let waiter = |class: CoordinatorWaiterClass| {
            let index = class as usize;
            CoordinatorWaiterOutcomeCounts {
                released: self.released[index],
                productive: self.productive[index],
                relevant: self.relevant[index],
                unrelated: self.unrelated[index],
            }
        };
        CoordinatorNotificationProfileSnapshot {
            calls: CoordinatorNotificationCallCounts {
                notify_one: mutation_counts_from_array(&self.notify_one),
                notify_all: mutation_counts_from_array(&self.notify_all),
            },
            workers: waiter(CoordinatorWaiterClass::Worker),
            exact_clients: waiter(CoordinatorWaiterClass::ExactClient),
            session_drains: waiter(CoordinatorWaiterClass::SessionDrain),
            task_observers: waiter(CoordinatorWaiterClass::TaskObserver),
        }
    }
}

#[cfg(any(test, feature = "glam-prof"))]
impl ExactRouteMutationProfile {
    fn snapshot(&self) -> crate::interaction_net::profiling::ExactRouteMutationProfileSnapshot {
        use crate::interaction_net::profiling::ExactRouteMutationProfileSnapshot;

        ExactRouteMutationProfileSnapshot {
            complete_searches: 0,
            records_visited: 0,
            maximum_depth: 0,
            fast_handoffs: 0,
            cold_fallbacks: 0,
            missing_release_fallbacks: 0,
            current_work_mismatch_fallbacks: 0,
            guarded_release_mutation_fallbacks: 0,
            changed_dependency_fallbacks: 0,
            retired_work_fallbacks: 0,
            branched_work_fallbacks: 0,
            moved_poll_windows: self.moved_poll_windows,
            o1_accepted_releases: self.o1_accepted_releases,
            hazard_validations: self.hazard_validations,
            successful_hazard_validations: self.successful_hazard_validations,
            failed_hazard_validations: self.failed_hazard_validations,
            validated_frames: 0,
        }
    }
}

#[cfg(any(test, feature = "glam-prof"))]
fn mutation_counts_from_array(
    counts: &[u64; COORDINATOR_MUTATION_KIND_COUNT],
) -> crate::interaction_net::profiling::CoordinatorMutationCounts {
    use crate::interaction_net::profiling::CoordinatorMutationCounts;

    debug_assert!(counts.len() >= PRODUCTION_COORDINATOR_MUTATION_KIND_COUNT);
    CoordinatorMutationCounts {
        demand_session_registry: counts[CoordinatorMutationKind::DemandSessionRegistry as usize],
        executor_availability: counts[CoordinatorMutationKind::ExecutorAvailability as usize],
        fresh_work_admission: counts[CoordinatorMutationKind::FreshWorkAdmission as usize],
        work_activation: counts[CoordinatorMutationKind::WorkActivation as usize],
        client_demand_admission: counts[CoordinatorMutationKind::ClientDemandAdmission as usize],
        task_promise_index_admission: counts
            [CoordinatorMutationKind::TaskPromiseIndexAdmission as usize],
        task_promise_index_retirement: counts
            [CoordinatorMutationKind::TaskPromiseIndexRetirement as usize],
        work_claim: counts[CoordinatorMutationKind::WorkClaim as usize],
        work_requeue: counts[CoordinatorMutationKind::WorkRequeue as usize],
        work_release: counts[CoordinatorMutationKind::WorkRelease as usize],
        client_demand_release: counts[CoordinatorMutationKind::ClientDemandRelease as usize],
        dependency_promotion: counts[CoordinatorMutationKind::DependencyPromotion as usize],
        dependency_wake: counts[CoordinatorMutationKind::DependencyWake as usize],
        observation_wake: counts[CoordinatorMutationKind::ObservationWake as usize],
        cancellation: counts[CoordinatorMutationKind::Cancellation as usize],
        session_closure: counts[CoordinatorMutationKind::SessionClosure as usize],
        terminal_settlement: counts[CoordinatorMutationKind::TerminalSettlement as usize],
        work_retirement: counts[CoordinatorMutationKind::WorkRetirement as usize],
        failure_ledger: counts[CoordinatorMutationKind::FailureLedger as usize],
        stage_settlement: counts[CoordinatorMutationKind::StageSettlement as usize],
    }
}

pub(super) enum CoordinatorSelection {
    Task(ClaimedTaskWork),
    Spark(ClaimedSparkWork),
    None,
}

pub(super) enum CausalChildSelection {
    Claimed(ClaimedTaskWork),
    Busy,
    None,
}

pub(super) enum ExactTargetSelection {
    Claimed(ClaimedTaskWork),
    Busy,
    None,
}

/// Non-authoritative position on one foreground exact-demand route.
///
/// The original wait remains owned by the driver. These scheduler identities
/// merely avoid rediscovering a route whose hazard revision is still known.
/// Hazard movement triggers guarded frame validation; only a concrete route
/// mismatch discards the checkpoint and rebuilds it from that original wait.
#[derive(Debug, Default)]
pub(crate) struct ExactDemandRoute {
    target: Option<(EvaluationRuntimeId, u64)>,
    current: Option<EvaluationWorkId>,
    parents: Vec<ExactDemandRouteFrame>,
    /// Every frame's work with its depth: `parents[depth]`, or the current
    /// tip at depth `parents.len()`. Depths are stable because the route
    /// only grows and shrinks at its tip.
    members: TrustedWorkIdMap<usize>,
    /// Last scheduler revision observed while the route was reconciled.
    generation: Option<u64>,
    /// Last exact-route hazard revision proved by fast acceptance or guarded
    /// frame validation.
    hazard_revision: Option<u64>,
    invalidation: Option<ExactRouteFallbackReason>,
}

#[derive(Debug, Clone, Copy)]
struct ExactDemandRouteFrame {
    work: EvaluationWorkId,
    subscription_epoch: u64,
    dependency: WorkDependencyKey,
}

impl ExactDemandRoute {
    fn prepare(&mut self, target: &EvaluationWaitToken) {
        let target = (target.runtime_id(), target.get());
        if self.target != Some(target) {
            self.target = Some(target);
            self.current = None;
            self.parents.clear();
            self.members.clear();
            self.generation = None;
            self.hazard_revision = None;
            self.invalidation = None;
        }
    }

    fn reset_to_target(&mut self, target: &EvaluationWaitToken) {
        self.target = Some((target.runtime_id(), target.get()));
        self.current = None;
        self.parents.clear();
        self.members.clear();
        self.generation = None;
        self.hazard_revision = None;
        self.invalidation = None;
    }

    pub(crate) fn invalidate(&mut self, reason: ExactRouteFallbackReason) {
        self.generation = None;
        self.hazard_revision = None;
        self.invalidation = Some(reason);
    }

    pub(crate) fn invalidate_missing_release(&mut self) {
        self.invalidate(ExactRouteFallbackReason::MissingRelease);
    }

    /// Records the current tip's producer, which becomes the new tip once
    /// the tip's frame is pushed. False when it is already a member: a
    /// cycle.
    fn admit_producer(&mut self, producer: EvaluationWorkId) -> bool {
        match self.members.entry(producer) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(self.parents.len() + 1);
                true
            }
        }
    }

    fn apply_validated_release(&mut self, release: ExactRouteRelease) {
        match release.disposition {
            ExactRouteDisposition::Runnable | ExactRouteDisposition::Busy => {}
            ExactRouteDisposition::Blocked {
                subscription_epoch,
                dependency,
                producer: Some(producer),
            } => {
                if !self.admit_producer(producer) {
                    self.generation = Some(release.end_generation);
                    self.hazard_revision = Some(release.end_hazard_revision);
                    return;
                }
                self.parents.push(ExactDemandRouteFrame {
                    work: release.work,
                    subscription_epoch,
                    dependency,
                });
                self.current = Some(producer);
            }
            ExactRouteDisposition::Blocked { producer: None, .. }
            | ExactRouteDisposition::Parked => {}
            ExactRouteDisposition::Terminal => {
                if let Some(current) = self.current {
                    self.members.remove(&current);
                }
                self.current = self.parents.pop().map(|frame| frame.work);
            }
        }
        self.generation = Some(release.end_generation);
        self.hazard_revision = Some(release.end_hazard_revision);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactRouteFallbackReason {
    MissingRelease,
    CurrentWorkMismatch,
    GuardedReleaseMutation,
    ChangedDependency,
    RetiredWork,
    BranchedWork,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ExactRouteRelease {
    work: EvaluationWorkId,
    start_generation: u64,
    end_generation: u64,
    start_hazard_revision: u64,
    end_hazard_revision: u64,
    uninterrupted: bool,
    disposition: ExactRouteDisposition,
}

#[derive(Debug, Clone, Copy)]
enum ExactRouteDisposition {
    Runnable,
    Busy,
    Blocked {
        subscription_epoch: u64,
        dependency: WorkDependencyKey,
        producer: Option<EvaluationWorkId>,
    },
    Parked,
    Terminal,
}

pub(super) struct ExactRouteReleaseTracker {
    work: EvaluationWorkId,
    start_generation: u64,
    expected_generation: u64,
    start_hazard_revision: u64,
}

impl ExactRouteReleaseTracker {
    fn new(state: &WorkCoordinatorState, work: EvaluationWorkId) -> Self {
        Self {
            work,
            start_generation: state.work_generation,
            expected_generation: state.work_generation,
            start_hazard_revision: state.exact_route_hazard_revision,
        }
    }

    fn changed(&mut self, changed: bool) {
        if changed {
            self.expected_generation = self.expected_generation.wrapping_add(1);
        }
    }

    fn finish(self, state: &WorkCoordinatorState) -> ExactRouteRelease {
        let disposition = exact_route_disposition_locked(state, self.work);
        ExactRouteRelease {
            work: self.work,
            start_generation: self.start_generation,
            end_generation: state.work_generation,
            start_hazard_revision: self.start_hazard_revision,
            end_hazard_revision: state.exact_route_hazard_revision,
            uninterrupted: state.work_generation == self.expected_generation,
            disposition,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExactTargetStatus {
    Ready,
    Busy,
    None,
}

impl fmt::Debug for EvaluationWorkCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        formatter
            .debug_struct("EvaluationWorkCoordinator")
            .field("runtime", &self.runtime)
            .field("session_count", &state.demand_sessions.len())
            .field("ready_task_count", &state.ready_task_set.len())
            .field(
                "work_count",
                &(state.work.len() + state.client_demands.len()),
            )
            .field("work_generation", &state.work_generation)
            .finish_non_exhaustive()
    }
}

impl EvaluationWorkCoordinator {
    pub(crate) fn new(
        values: &CoreValueFactory,
        admission: Arc<RuntimeMutationAdmission>,
        observations: Arc<RuntimeObservationState>,
    ) -> Arc<Self> {
        let coordinator = Arc::new(Self {
            runtime: values.runtime_id(),
            values: values.runtime_value_observer(),
            ids: values.ids().clone(),
            admission,
            observations,
            background_demand: Mutex::new(None),
            state: Mutex::new(WorkCoordinatorState::default()),
            work_available: CountedCondvar::new(),
            #[cfg(test)]
            work_wait_probe: OnceLock::new(),
            #[cfg(test)]
            test_values: None,
            #[cfg(test)]
            terminal_publication_probe: Mutex::new(None),
            #[cfg(test)]
            reflection_release_status_probe: Mutex::new(None),
            #[cfg(any(test, feature = "glam-prof"))]
            exact_route_profile: Mutex::new(ExactDemandRouteProfile::default()),
            #[cfg(any(test, feature = "glam-prof"))]
            exact_route_mutation_profile: Mutex::new(ExactRouteMutationProfile::default()),
            #[cfg(any(test, feature = "glam-prof"))]
            notification_profile: Mutex::new(CoordinatorNotificationProfile::default()),
            #[cfg(test)]
            exact_selection_probe: Mutex::new(None),
        });
        coordinator.register_runtime_core();
        coordinator
    }

    /// Whether a panic tore runtime-core state. Destructors check this
    /// before touching scheduler state: dropping a poisoned runtime's
    /// handles is a no-op, never a second panic.
    pub(crate) fn runtime_poisoned(&self) -> bool {
        self.admission.is_poisoned()
    }

    /// The set-once poison mark, without probing core state. Cheap enough
    /// for the worker loop.
    pub(crate) fn runtime_poison_marked(&self) -> bool {
        self.admission.poison_marked()
    }

    /// Collects at a quantum boundary when collector pressure asks for it:
    /// after a claimed task, spark or client demand is polled and before it
    /// is released. See `RuntimeMutationAdmission::service_collection_pressure`.
    pub(crate) fn service_collection_pressure(&self) {
        if let Some(values) = self.values.upgrade() {
            self.admission.service_collection_pressure(&values);
        }
    }

    pub(crate) fn mark_runtime_poisoned(&self) {
        self.admission.mark_poisoned();
    }

    pub(crate) fn wake_parked_workers(&self) {
        self.notify_all(CoordinatorMutationKind::ExecutorAvailability);
    }

    /// Simulates a scheduler bug: a panic inside a coordinator critical
    /// section, under mutation authority when `with_authority` is set and in
    /// a read-only section otherwise.
    #[cfg(test)]
    pub(crate) fn poison_scheduler_for_test(&self, with_authority: bool) {
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _mutation = with_authority.then(|| self.admission.mutation_guard());
            let _state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            panic!("forced scheduler panic");
        }));
        assert!(panicked.is_err());
    }

    /// Registers the scheduler state as runtime core: a panic that tears it
    /// poisons the whole runtime.
    fn register_runtime_core(self: &Arc<Self>) {
        let core: std::sync::Weak<dyn RuntimeCoreState> = Arc::<Self>::downgrade(self);
        self.admission.register_core(core);
    }

    pub(super) fn background_demand_or_init(
        &self,
        initialize: impl FnOnce() -> Arc<EvaluationDemandState>,
    ) -> Arc<EvaluationDemandState> {
        let mut background = self
            .background_demand
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(demand) = background.as_ref() {
            return demand.clone();
        }
        let demand = initialize();
        self.register_demand(&demand);
        *background = Some(demand.clone());
        demand
    }

    pub(super) fn background_demand(&self) -> Option<Arc<EvaluationDemandState>> {
        self.background_demand
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Releases the runtime-owned background demand at runtime teardown.
    ///
    /// Workers retain only a coordinator route while idle. Keeping this
    /// value-domain owner inside that coordinator after the public runtime is
    /// gone would otherwise delay value-domain retirement until an idle
    /// worker observes shutdown.
    pub(crate) fn release_background_demand(&self) {
        let demand = self
            .background_demand
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        drop(demand);
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        values: CoreValueFactory,
        admission: Arc<RuntimeMutationAdmission>,
    ) -> Arc<Self> {
        let coordinator = Arc::new(Self {
            runtime: values.runtime_id(),
            values: values.runtime_value_observer(),
            ids: values.ids().clone(),
            admission,
            observations: RuntimeObservationState::new(),
            background_demand: Mutex::new(None),
            state: Mutex::new(WorkCoordinatorState::default()),
            work_available: CountedCondvar::new(),
            work_wait_probe: OnceLock::new(),
            test_values: Some(values.clone()),
            terminal_publication_probe: Mutex::new(None),
            reflection_release_status_probe: Mutex::new(None),
            exact_route_profile: Mutex::new(ExactDemandRouteProfile::default()),
            exact_route_mutation_profile: Mutex::new(ExactRouteMutationProfile::default()),
            notification_profile: Mutex::new(CoordinatorNotificationProfile::default()),
            exact_selection_probe: Mutex::new(None),
        });
        coordinator.register_runtime_core();
        values.attach_work_coordinator(&coordinator);
        coordinator
    }

    #[cfg(test)]
    pub(crate) fn test_values(&self) -> CoreValueFactory {
        self.test_values
            .as_ref()
            .expect("synthetic execution resources must install test values")
            .clone()
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub(crate) fn value_observer(&self) -> crate::core::RuntimeValueObserver {
        self.values.clone()
    }

    #[cfg(test)]
    pub(crate) fn shared_mutation_admission(&self) -> Arc<RuntimeMutationAdmission> {
        self.admission.clone()
    }

    #[cfg(test)]
    pub(crate) fn shared_observations(&self) -> Arc<RuntimeObservationState> {
        self.observations.clone()
    }

    #[cfg(test)]
    pub(super) fn runtime_locks_are_free(&self) -> bool {
        self.state.try_lock().is_ok() && self.admission.try_settlement_guard().is_some()
    }

    #[cfg(any(test, feature = "glam-prof"))]
    pub(super) fn record_complete_exact_route_search(&self, depth: usize) {
        let mut profile = self
            .exact_route_profile
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        profile.complete_searches += 1;
        profile.edges_visited += depth;
        profile.maximum_depth = profile.maximum_depth.max(depth);
    }

    #[cfg(any(test, feature = "glam-prof"))]
    pub(super) fn record_exact_route_handoffs(&self, handoffs: usize) {
        self.exact_route_profile
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .fast_handoffs += handoffs;
    }

    #[cfg(any(test, feature = "glam-prof"))]
    fn record_exact_route_fallback(&self, reason: ExactRouteFallbackReason) {
        let mut profile = self
            .exact_route_profile
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        profile.checkpoint_invalidations += 1;
        profile.cold_fallbacks += 1;
        match reason {
            ExactRouteFallbackReason::MissingRelease => profile.missing_release_fallbacks += 1,
            ExactRouteFallbackReason::CurrentWorkMismatch => {
                profile.current_work_mismatch_fallbacks += 1;
            }
            ExactRouteFallbackReason::GuardedReleaseMutation => {
                profile.guarded_release_mutation_fallbacks += 1;
            }
            ExactRouteFallbackReason::ChangedDependency => {
                profile.changed_dependency_fallbacks += 1;
            }
            ExactRouteFallbackReason::RetiredWork => profile.retired_work_fallbacks += 1,
            ExactRouteFallbackReason::BranchedWork => profile.branched_work_fallbacks += 1,
        }
    }

    #[cfg(test)]
    pub(super) fn exact_demand_route_profile(&self) -> ExactDemandRouteProfile {
        *self
            .exact_route_profile
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(any(test, feature = "glam-prof"))]
    fn record_exact_route_moved_poll_window(&self) {
        let mut profile = self
            .exact_route_mutation_profile
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        profile.moved_poll_windows = profile.moved_poll_windows.wrapping_add(1);
    }

    #[cfg(any(test, feature = "glam-prof"))]
    fn record_exact_route_reconciliation(
        &self,
        validation: Option<Result<usize, ExactRouteFallbackReason>>,
        accepted: bool,
    ) {
        {
            let mut profile = self
                .exact_route_mutation_profile
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match validation {
                None if accepted => {
                    profile.o1_accepted_releases = profile.o1_accepted_releases.wrapping_add(1);
                }
                Some(result) => {
                    profile.hazard_validations = profile.hazard_validations.wrapping_add(1);
                    if result.is_ok() {
                        profile.successful_hazard_validations =
                            profile.successful_hazard_validations.wrapping_add(1);
                    } else {
                        profile.failed_hazard_validations =
                            profile.failed_hazard_validations.wrapping_add(1);
                    }
                }
                None => {}
            }
        }
        #[cfg(any(test, feature = "glam-prof"))]
        {
            let mut route = self
                .exact_route_profile
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match validation {
                None if accepted => route.o1_accepted_releases += 1,
                Some(result) => {
                    route.hazard_validations += 1;
                    if result.is_ok() {
                        route.successful_hazard_validations += 1;
                    } else {
                        route.failed_hazard_validations += 1;
                    }
                }
                None => {}
            }
        }
    }

    #[cfg(any(test, feature = "glam-prof"))]
    pub(crate) fn exact_route_mutation_profile(
        &self,
    ) -> crate::interaction_net::profiling::ExactRouteMutationProfileSnapshot {
        let mut snapshot = self
            .exact_route_mutation_profile
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot();
        snapshot.validated_frames = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .exact_route_validated_frames
            .load(std::sync::atomic::Ordering::Relaxed);
        let route = self
            .exact_route_profile
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        snapshot.complete_searches = route.complete_searches as u64;
        snapshot.records_visited = route.edges_visited as u64;
        snapshot.maximum_depth = route.maximum_depth as u64;
        snapshot.fast_handoffs = route.fast_handoffs as u64;
        snapshot.cold_fallbacks = route.cold_fallbacks as u64;
        snapshot.missing_release_fallbacks = route.missing_release_fallbacks as u64;
        snapshot.current_work_mismatch_fallbacks = route.current_work_mismatch_fallbacks as u64;
        snapshot.guarded_release_mutation_fallbacks =
            route.guarded_release_mutation_fallbacks as u64;
        snapshot.changed_dependency_fallbacks = route.changed_dependency_fallbacks as u64;
        snapshot.retired_work_fallbacks = route.retired_work_fallbacks as u64;
        snapshot.branched_work_fallbacks = route.branched_work_fallbacks as u64;
        snapshot
    }

    #[cfg(any(test, feature = "glam-prof"))]
    pub(crate) fn coordinator_notification_profile(
        &self,
    ) -> crate::interaction_net::profiling::CoordinatorNotificationProfileSnapshot {
        self.notification_profile
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot()
    }

    fn notify_all(&self, kind: CoordinatorMutationKind) {
        if !kind.notifies_waiters() {
            return;
        }
        #[cfg(any(test, feature = "glam-prof"))]
        {
            let mut profile = self
                .notification_profile
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let index = kind as usize;
            profile.notify_all[index] = profile.notify_all[index].wrapping_add(1);
        }
        #[cfg(not(any(test, feature = "glam-prof")))]
        let _ = kind;
        self.work_available.notify_all();
    }

    fn record_waiter_release(&self, class: CoordinatorWaiterClass) {
        #[cfg(any(test, feature = "glam-prof"))]
        {
            let mut profile = self
                .notification_profile
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let index = class as usize;
            profile.released[index] = profile.released[index].wrapping_add(1);
        }
        #[cfg(not(any(test, feature = "glam-prof")))]
        let _ = class;
    }

    pub(super) fn record_waiter_outcome(
        &self,
        class: CoordinatorWaiterClass,
        outcome: CoordinatorWaiterOutcome,
    ) {
        #[cfg(any(test, feature = "glam-prof"))]
        {
            let mut profile = self
                .notification_profile
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let counts = match outcome {
                CoordinatorWaiterOutcome::Productive => &mut profile.productive,
                CoordinatorWaiterOutcome::Relevant => &mut profile.relevant,
                CoordinatorWaiterOutcome::Unrelated => &mut profile.unrelated,
            };
            let index = class as usize;
            counts[index] = counts[index].wrapping_add(1);
        }
        #[cfg(not(any(test, feature = "glam-prof")))]
        let _ = (class, outcome);
    }

    #[cfg(test)]
    pub(super) fn set_exact_selection_probe(&self, probe: impl FnOnce() + Send + 'static) {
        *self
            .exact_selection_probe
            .lock()
            .expect("exact selection probe was poisoned") = Some(Box::new(probe));
    }

    #[cfg(test)]
    pub(super) fn settlement_admission_is_free(&self) -> bool {
        self.admission.try_settlement_guard().is_some()
    }

    #[cfg(test)]
    pub(super) fn set_reflection_release_status_probe(
        &self,
        probe: impl FnOnce() + Send + 'static,
    ) {
        *self
            .reflection_release_status_probe
            .lock()
            .expect("reflection release status probe was poisoned") = Some(Box::new(probe));
    }

    #[cfg(test)]
    fn run_reflection_release_status_probe(&self) {
        let probe = self
            .reflection_release_status_probe
            .lock()
            .expect("reflection release status probe was poisoned")
            .take();
        if let Some(probe) = probe {
            probe();
        }
    }

    #[cfg(test)]
    pub(super) fn set_terminal_publication_probe(&self, probe: impl FnOnce() + Send + 'static) {
        *self
            .terminal_publication_probe
            .lock()
            .expect("terminal publication probe was poisoned") = Some(Box::new(probe));
    }

    #[cfg(test)]
    fn run_terminal_publication_probe(&self) {
        if let Some(probe) = self
            .terminal_publication_probe
            .lock()
            .expect("terminal publication probe was poisoned")
            .take()
        {
            probe();
        }
    }

    pub(crate) fn current_observation_epoch(&self) -> RuntimeObservationEpoch {
        self.observations.current()
    }

    /// Returns one persistent owner bucket from the runtime failure ledger.
    pub(super) fn failure_snapshot(&self, session: EvaluationSessionId) -> TaskFailureLedger {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .failures
            .get(&session)
            .cloned()
            .unwrap_or_else(TaskFailureLedger::new_sync)
    }

    #[cfg(test)]
    pub(crate) fn failure_ledger_snapshot(&self) -> RuntimeFailureLedger {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .failures
            .clone()
    }

    /// Captures the persistent failure ledger and commits every not-yet-
    /// reported failure to the current settlement report.
    ///
    /// The persistent ledger remains authoritative until explicit
    /// acknowledgement. Only the separate reporting obligations move into the
    /// report, so later settlements do not ask a presentation layer to
    /// remember which failures it has already rendered.
    pub(crate) fn failure_ledgers_for_settlement(
        &self,
    ) -> (RuntimeFailureLedger, RuntimeFailureLedger) {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let pending = std::mem::replace(
            &mut state.pending_failure_reports,
            RuntimeFailureLedger::new_sync(),
        );
        (state.failures.clone(), pending)
    }

    #[cfg(test)]
    pub(crate) fn publish_runtime_observation(&self) {
        let mutation = self.admission.mutation_guard();
        let epoch = self.observations.advance();
        let changed = self.publish_runtime_observation_guarded(&mutation, epoch);
        drop(mutation);
        self.observations.notify_all();
        self.notify_runtime_observation(changed);
    }

    pub(crate) fn mutation_guard(&self) -> RuntimeMutationGuard<'_> {
        self.admission.mutation_guard()
    }

    pub(super) fn register_demand(&self, demand: &Arc<EvaluationDemandState>) {
        debug_assert_eq!(demand.values.runtime_id(), self.runtime);
        self.publish_transition(CoordinatorMutationKind::DemandSessionRegistry, |state| {
            let replaced = state
                .demand_sessions
                .insert(demand.id, Arc::downgrade(demand));
            assert!(
                replaced.is_none(),
                "evaluation session identities must be unique within a runtime"
            );
        });
    }

    /// Closes one demand session in a single guarded coordinator transition.
    ///
    /// Non-running task work enters terminalization immediately. Running work
    /// retains its exclusive claim and its first close reason until release.
    /// Spark dependencies are abandoned only after runtime locks and mutation
    /// admission have been released.
    pub(super) fn close_session(&self, session: EvaluationSessionId) -> SessionClosureWork {
        let mutation = self.admission.mutation_guard();
        let (reflection, deferred, retired_sparks, client_demands, changed) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let initial_generation = state.work_generation;
            let work = state
                .work_by_session
                .get(&session)
                .into_iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            let mut reflection = Vec::new();
            let mut deferred = Vec::new();
            let mut retired_sparks = Vec::new();
            let mut client_demands = Vec::new();
            let mut changed = false;
            for id in work {
                let Some(record) = state.work.get(&id) else {
                    continue;
                };
                if matches!(record.state, WorkState::Terminalizing) {
                    // The operation which published terminalization owns its
                    // producer settlement and retirement tail.
                    continue;
                }
                let running = matches!(record.state, WorkState::Running);
                match &record.kind {
                    WorkKind::Reflection(reflection_work) => {
                        let task = reflection_work.task;
                        let cancel = matches!(
                            record.control.close_reason,
                            Some(WorkCloseReason::ExplicitCancellation)
                        );
                        let record = state
                            .work
                            .get_mut(&id)
                            .expect("indexed reflection work must remain registered");
                        if record.control.close_reason.is_none() {
                            debug_assert!(!cancel);
                            record.control.close_reason =
                                Some(WorkCloseReason::DemandSessionClosed);
                            changed = true;
                        }
                        if running {
                            continue;
                        }
                        record.state = WorkState::Terminalizing;
                        state.observation_waiters.remove(&id);
                        remove_ready_reflection(&mut state, id);
                        reflection.push(AbandonedReflectionWork { id, task, cancel });
                        changed = true;
                    }
                    WorkKind::Deferred(_) => {
                        let record = state
                            .work
                            .get_mut(&id)
                            .expect("indexed deferred work must remain registered");
                        if record.control.close_reason.is_none() {
                            record.control.close_reason =
                                Some(WorkCloseReason::DemandSessionClosed);
                            changed = true;
                        }
                        if running {
                            continue;
                        }
                        deferred.push(begin_deferred_abandonment(&mut state, id));
                        changed = true;
                    }
                    WorkKind::LazyRoute(_) => {
                        unreachable!("runtime-owned lazy route entered session closure")
                    }
                    WorkKind::Spark(_) => {
                        if running {
                            let record = state
                                .work
                                .get_mut(&id)
                                .expect("indexed running spark work must remain registered");
                            if record.control.close_reason.is_none() {
                                record.control.close_reason =
                                    Some(WorkCloseReason::DemandSessionClosed);
                                changed = true;
                            }
                        } else if let Some(record) = detach_spark(&mut state, id) {
                            retired_sparks.push(record);
                            changed = true;
                        }
                    }
                }
            }
            let clients = state
                .client_demands_by_session
                .get(&session)
                .into_iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            for id in clients {
                let Some(record) = state.client_demands.get_mut(&id) else {
                    continue;
                };
                if record.control.close_reason.is_none() {
                    record.control.close_reason = Some(WorkCloseReason::DemandSessionClosed);
                    changed = true;
                }
                if matches!(record.state, WorkState::Running) {
                    continue;
                }
                client_demands.push(detach_client_demand(
                    &mut state,
                    id,
                    None,
                    None,
                    ClientDemandResult::Abandoned,
                ));
                changed = true;
            }
            changed |= prune_closed_session_registration(&mut state, session);
            if changed {
                state.advance_work_generation(
                    CoordinatorMutationKind::SessionClosure,
                    RouteHazard::All,
                );
            }
            (
                reflection,
                deferred,
                retired_sparks,
                client_demands,
                state.work_generation != initial_generation,
            )
        };
        drop(mutation);
        if changed {
            self.notify_all(CoordinatorMutationKind::SessionClosure);
        }
        SessionClosureWork {
            reflection,
            deferred,
            retired_sparks,
            client_demands,
        }
    }

    pub(super) fn executor_started(&self, worker_count: usize) {
        self.publish_transition(CoordinatorMutationKind::ExecutorAvailability, |state| {
            state.spark_workers = worker_count;
        });
    }

    pub(super) fn executor_stopped(&self) {
        let mutation = self.admission.mutation_guard();
        let retired = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            state.spark_workers = 0;
            let ids = state.work.keys().copied().collect::<Vec<_>>();
            let mut retired = Vec::new();
            for id in ids {
                if !state
                    .work
                    .get(&id)
                    .is_some_and(|record| matches!(record.kind, WorkKind::Spark(_)))
                {
                    continue;
                }
                let is_running = state
                    .work
                    .get(&id)
                    .is_some_and(|record| matches!(record.state, WorkState::Running));
                if is_running {
                    let record = state
                        .work
                        .get_mut(&id)
                        .expect("running spark work must remain registered");
                    record.control.close_reason = Some(WorkCloseReason::ExecutorShutdown);
                } else if state
                    .work
                    .get(&id)
                    .is_some_and(|record| matches!(record.kind, WorkKind::Spark(_)))
                    && let Some(record) = detach_spark(&mut state, id)
                {
                    retired.push(record);
                }
            }
            state.advance_work_generation(
                CoordinatorMutationKind::ExecutorAvailability,
                RouteHazard::None,
            );
            retired
        };
        drop(mutation);
        self.notify_all(CoordinatorMutationKind::ExecutorAvailability);
        for record in retired {
            record.abandon();
        }
    }

    pub(super) fn work_generation(&self) -> u64 {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .work_generation
    }

    #[cfg(test)]
    pub(crate) fn scheduler_inventory_for_test(&self) -> (u64, usize, usize) {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        (
            state.work_generation,
            state.demand_sessions.len(),
            state.work.len() + state.client_demands.len(),
        )
    }

    pub(super) fn session_has_ready_task(&self, session: EvaluationSessionId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state.ready_task_set.iter().any(|id| {
            state
                .work
                .get(id)
                .is_some_and(|record| record.demand_session == session)
        })
    }

    /// Selects one executor-worker root.
    ///
    /// Foreground client evaluations are intentionally absent. The client
    /// which owns that record polls it directly and workers begin only from
    /// background spark or reflection roots. A deferred descendant is
    /// claimable only when an exact dependency path from one of those roots
    /// reaches it.
    pub(super) fn select_worker(&self) -> CoordinatorSelection {
        let mutation = self.admission.mutation_guard();
        let (selection, changed) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let initial_generation = state.work_generation;
            let selection = claim_causal_background(&mut state, self.runtime, true);
            if !matches!(selection, CoordinatorSelection::None) {
                state
                    .advance_work_generation(CoordinatorMutationKind::WorkClaim, RouteHazard::None);
            }
            (selection, state.work_generation != initial_generation)
        };
        drop(mutation);
        if changed {
            self.notify_all(CoordinatorMutationKind::WorkClaim);
        }
        selection
    }

    /// Claims one lifecycle-bearing work item for the host runtime pump.
    ///
    /// Unlike worker selection, this deliberately ignores sparks. Sparks are
    /// best-effort hints which only workers execute; the host pump normalizes
    /// any unclaimed spark records separately once useful work is quiescent.
    pub(super) fn select_runtime_pump(&self) -> CoordinatorSelection {
        let mutation = self.admission.mutation_guard();
        let (selection, changed) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let initial_generation = state.work_generation;
            let selection = claim_causal_background(&mut state, self.runtime, false);
            if !matches!(selection, CoordinatorSelection::None) {
                state
                    .advance_work_generation(CoordinatorMutationKind::WorkClaim, RouteHazard::None);
            }
            (selection, state.work_generation != initial_generation)
        };
        drop(mutation);
        if changed {
            self.notify_all(CoordinatorMutationKind::WorkClaim);
        }
        selection
    }
}

impl EvaluationWorkCoordinator {
    /// Restores coordinator-claimed task work which was selected but not
    /// polled. This is used only when an executor begins shutdown between
    /// selection and polling. Both task kinds return their detached machine to
    /// the coordinator record before becoming claimable again.
    pub(super) fn requeue_unpolled_task(&self, claimed: ClaimedTaskWork) {
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let id = match claimed {
                ClaimedTaskWork::Reflection(mut claim) => {
                    let record = state
                        .work
                        .get_mut(&claim.id)
                        .expect("unpolled reflection work must remain registered");
                    assert!(matches!(record.state, WorkState::Running));
                    let reflection = reflection_work_mut(record);
                    assert!(
                        reflection.machine.is_none(),
                        "running reflection work must have detached its machine"
                    );
                    reflection.machine = claim.machine.take();
                    reflection.block = claim.prior_block;
                    record.state = WorkState::Queued;
                    claim.id
                }
                ClaimedTaskWork::Deferred(mut claimed) => {
                    let record = state
                        .work
                        .get_mut(&claimed.id)
                        .expect("unpolled deferred work must remain registered");
                    assert!(matches!(record.state, WorkState::Running));
                    let deferred = deferred_work_mut(record);
                    assert!(
                        deferred.machine.is_none(),
                        "running deferred work must have detached its machine"
                    );
                    deferred.machine = claimed.machine.take();
                    deferred.block = claimed.prior_block;
                    record.state = WorkState::Queued;
                    claimed.id
                }
                ClaimedTaskWork::LazyRoute(claimed) => {
                    let record = state
                        .work
                        .get_mut(&claimed.id)
                        .expect("unpolled lazy route must remain registered");
                    assert!(matches!(record.state, WorkState::Running));
                    let WorkKind::LazyRoute(route) = &mut record.kind else {
                        unreachable!()
                    };
                    route.block = claimed.prior_block;
                    record.state = WorkState::Queued;
                    claimed.id
                }
            };
            queue_task(&mut state, id);
            state.advance_work_generation(CoordinatorMutationKind::WorkRequeue, RouteHazard::None);
        }
        drop(mutation);
        self.notify_all(CoordinatorMutationKind::WorkRequeue);
    }

    /// Claims one reflection root owned by this demand session, or the first
    /// claimable item on that root's exact producer chain. Merely sharing a
    /// session does not grant drain authority, and sparks and foreground
    /// client records are never session-drain roots.
    pub(super) fn claim_ready_task_for_session(
        &self,
        session: EvaluationSessionId,
    ) -> Option<ClaimedTaskWork> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let selection = claim_causal_session_background(&mut state, self.runtime, session);
            if !matches!(selection, CoordinatorSelection::None) {
                state
                    .advance_work_generation(CoordinatorMutationKind::WorkClaim, RouteHazard::None);
            }
            match selection {
                CoordinatorSelection::Task(claimed) => Some(claimed),
                CoordinatorSelection::Spark(_) => {
                    unreachable!("a session drain cannot select spark work")
                }
                CoordinatorSelection::None => None,
            }
        };
        drop(mutation);
        if claimed.is_some() {
            self.notify_all(CoordinatorMutationKind::WorkClaim);
        }
        claimed
    }

    /// Mechanical queue-selection hook for coordinator unit tests which are
    /// not exercising session-drain authority. Production session drains must
    /// use `claim_ready_task_for_session` and therefore start from an owned
    /// reflection root.
    #[cfg(test)]
    pub(in crate::evaluation) fn claim_ready_session_machine_for_test(
        &self,
        session: EvaluationSessionId,
    ) -> Option<ClaimedTaskWork> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let claimed = claim_ready_task(&mut state, self.runtime, Some(session));
            if claimed.is_some() {
                state
                    .advance_work_generation(CoordinatorMutationKind::WorkClaim, RouteHazard::None);
            }
            claimed
        };
        drop(mutation);
        if claimed.is_some() {
            self.notify_all(CoordinatorMutationKind::WorkClaim);
        }
        claimed
    }

    /// Test-only compatibility selector for fixtures which advance the old
    /// mixed scheduler one quantum at a time.
    ///
    /// Production worker and runtime selectors must use causal root traversal.
    #[cfg(test)]
    pub(in crate::evaluation) fn claim_ready_task_for_test(&self) -> Option<ClaimedTaskWork> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let claimed = claim_ready_task(&mut state, self.runtime, None);
            if claimed.is_some() {
                state
                    .advance_work_generation(CoordinatorMutationKind::WorkClaim, RouteHazard::None);
            }
            claimed
        };
        drop(mutation);
        if claimed.is_some() {
            self.notify_all(CoordinatorMutationKind::WorkClaim);
        }
        claimed
    }

    /// Claims one exact task dependency and detaches its opaque machine from
    /// the coordinator record. All reporting identity remains in the stable
    /// work record while the machine is claimed.
    #[cfg(test)]
    pub(super) fn claim_task(&self, task: EvaluationTaskId) -> Option<ClaimedTaskWork> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let id = state
                .reflection
                .by_task
                .get(&task)
                .or_else(|| state.deferred.by_task.get(&task))
                .copied()?;
            let work = match state.work.get(&id)?.kind {
                WorkKind::Reflection(_) => claim_reflection_task(&mut state, self.runtime, id),
                WorkKind::Deferred(_) => claim_deferred(&mut state, self.runtime, id, false)
                    .map(ClaimedTaskWork::Deferred),
                WorkKind::LazyRoute(_) => None,
                WorkKind::Spark(_) => None,
            }?;
            state.advance_work_generation(CoordinatorMutationKind::WorkClaim, RouteHazard::None);
            Some(work)
        };
        drop(mutation);
        if claimed.is_some() {
            self.notify_all(CoordinatorMutationKind::WorkClaim);
        }
        claimed
    }

    pub(super) fn claim_work(&self, id: EvaluationWorkId) -> Option<ClaimedTaskWork> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let work = claim_task_work_locked(&mut state, self.runtime, id, false)?;
            state.advance_work_generation(CoordinatorMutationKind::WorkClaim, RouteHazard::None);
            Some(work)
        };
        drop(mutation);
        if claimed.is_some() {
            self.notify_all(CoordinatorMutationKind::WorkClaim);
        }
        claimed
    }

    /// Finds and claims the first runnable producer on one exact wait route.
    ///
    /// Route discovery and claiming share runtime mutation admission and one
    /// coordinator-state critical section. A queued ancestor therefore wins
    /// before its retained prior block, and the selected producer cannot
    /// retire or change dependency between the probe and claim.
    #[cfg(test)]
    pub(super) fn claim_exact_target(&self, target: &EvaluationWaitToken) -> ExactTargetSelection {
        let mut route = ExactDemandRoute::default();
        self.claim_exact_target_on_route(target, &mut route)
    }

    /// Claims the next runnable record through a retained exact-demand route.
    ///
    /// A matching hazard revision makes the local zipper a sufficient proof
    /// for the next descent or return. Hazard movement validates its compact
    /// frames under the coordinator lock before claiming; a concrete mismatch
    /// alone rebuilds from the authoritative target.
    pub(super) fn claim_exact_target_on_route(
        &self,
        target: &EvaluationWaitToken,
        route: &mut ExactDemandRoute,
    ) -> ExactTargetSelection {
        debug_assert_eq!(target.runtime_id(), self.runtime);
        route.prepare(target);
        #[cfg(test)]
        let selection_probe = self
            .exact_selection_probe
            .lock()
            .expect("exact selection probe was poisoned")
            .take();
        let mutation = self.admission.mutation_guard();
        let (selection, _depth, _handoffs, _fallback) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let (probe, handoffs, fallback) = exact_route_probe_locked(&state, target, route);
            let selection = match probe.selection {
                CausalBackgroundProbe::Ready(id) => {
                    #[cfg(test)]
                    if let Some(probe) = selection_probe {
                        probe();
                    }
                    match claim_task_work_locked(&mut state, self.runtime, id, false) {
                        Some(claimed) => {
                            state.advance_work_generation(
                                CoordinatorMutationKind::WorkClaim,
                                RouteHazard::None,
                            );
                            route.current = Some(id);
                            route.generation = Some(state.work_generation);
                            route.hazard_revision = Some(state.exact_route_hazard_revision);
                            ExactTargetSelection::Claimed(claimed)
                        }
                        None => {
                            route.invalidate(ExactRouteFallbackReason::CurrentWorkMismatch);
                            ExactTargetSelection::None
                        }
                    }
                }
                CausalBackgroundProbe::Busy(id) => {
                    route.current = Some(id);
                    route.generation = Some(state.work_generation);
                    route.hazard_revision = Some(state.exact_route_hazard_revision);
                    ExactTargetSelection::Busy
                }
                CausalBackgroundProbe::None => {
                    route.generation = Some(state.work_generation);
                    route.hazard_revision = Some(state.exact_route_hazard_revision);
                    ExactTargetSelection::None
                }
            };
            (selection, probe.depth, handoffs, fallback)
        };
        drop(mutation);
        #[cfg(any(test, feature = "glam-prof"))]
        {
            if _depth != 0 {
                self.record_complete_exact_route_search(_depth);
            }
            self.record_exact_route_handoffs(_handoffs);
            if let Some(reason) = _fallback {
                self.record_exact_route_fallback(reason);
            }
        }
        if matches!(selection, ExactTargetSelection::Claimed(_)) {
            self.notify_all(CoordinatorMutationKind::WorkClaim);
        }
        selection
    }

    /// Observes the current exact-route scheduling state without claiming it.
    /// This is used only where a caller must decide whether to wait or retry;
    /// execution paths use [`Self::claim_exact_target`] instead.
    #[cfg(test)]
    pub(super) fn exact_target_status(&self, target: &EvaluationWaitToken) -> ExactTargetStatus {
        debug_assert_eq!(target.runtime_id(), self.runtime);
        let (status, _depth) = {
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let probe = exact_target_probe_locked(&state, target);
            let status = match probe.selection {
                CausalBackgroundProbe::Ready(_) => ExactTargetStatus::Ready,
                CausalBackgroundProbe::Busy(_) => ExactTargetStatus::Busy,
                CausalBackgroundProbe::None => ExactTargetStatus::None,
            };
            (status, probe.depth)
        };
        #[cfg(test)]
        self.record_complete_exact_route_search(_depth);
        status
    }

    pub(super) fn exact_target_status_on_route(
        &self,
        target: &EvaluationWaitToken,
        route: &mut ExactDemandRoute,
    ) -> ExactTargetStatus {
        debug_assert_eq!(target.runtime_id(), self.runtime);
        route.prepare(target);
        let (status, _depth, _handoffs, _fallback) = {
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let (probe, handoffs, fallback) = exact_route_probe_locked(&state, target, route);
            let status = match probe.selection {
                CausalBackgroundProbe::Ready(id) => {
                    route.current = Some(id);
                    ExactTargetStatus::Ready
                }
                CausalBackgroundProbe::Busy(id) => {
                    route.current = Some(id);
                    ExactTargetStatus::Busy
                }
                CausalBackgroundProbe::None => ExactTargetStatus::None,
            };
            route.generation = Some(state.work_generation);
            route.hazard_revision = Some(state.exact_route_hazard_revision);
            (status, probe.depth, handoffs, fallback)
        };
        #[cfg(any(test, feature = "glam-prof"))]
        {
            if _depth != 0 {
                self.record_complete_exact_route_search(_depth);
            }
            self.record_exact_route_handoffs(_handoffs);
            if let Some(reason) = _fallback {
                self.record_exact_route_fallback(reason);
            }
        }
        status
    }

    /// Reconciles one exactly claimed poll with its caller-local route.
    ///
    /// Neutral scheduler movement is accepted in O(1). A narrower hazard
    /// movement validates the retained route frames under the coordinator
    /// mutex; only a concrete mismatch discards the zipper.
    pub(super) fn reconcile_exact_route_release(
        &self,
        target: &EvaluationWaitToken,
        route: &mut ExactDemandRoute,
        release: ExactRouteRelease,
    ) -> bool {
        debug_assert_eq!(target.runtime_id(), self.runtime);
        let _scheduler_revision_at_poll_end = release.start_generation;
        let (validation, reason) = {
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if route.current != Some(release.work) {
                (None, Some(ExactRouteFallbackReason::CurrentWorkMismatch))
            } else if !release.uninterrupted {
                (None, Some(ExactRouteFallbackReason::GuardedReleaseMutation))
            } else if route.hazard_revision == Some(release.start_hazard_revision) {
                (None, None)
            } else {
                let validation = validate_exact_route_locked(&state, target, route, false);
                (Some(validation), validation.err())
            }
        };

        #[cfg(any(test, feature = "glam-prof"))]
        if route.generation != Some(release.start_generation) {
            self.record_exact_route_moved_poll_window();
        }

        if let Some(reason) = reason {
            route.invalidate(reason);
            #[cfg(any(test, feature = "glam-prof"))]
            self.record_exact_route_reconciliation(validation, false);
            return false;
        }

        #[cfg(any(test, feature = "glam-prof"))]
        self.record_exact_route_reconciliation(validation, true);
        #[cfg(not(any(test, feature = "glam-prof")))]
        let _ = validation;
        route.apply_validated_release(release);
        true
    }

    /// Claims a launched child (or its exact producer) reachable from the
    /// target/caller task identities. This never scans the same-session ready
    /// queue: launch provenance is a helping route, not a completion wait.
    pub(super) fn claim_causal_child_work(
        &self,
        target: Option<&EvaluationWaitToken>,
        caller_tasks: [Option<EvaluationTaskId>; 2],
    ) -> CausalChildSelection {
        let mutation = self.admission.mutation_guard();
        let selection = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let mut excluded = TrustedHashSet::default();
            loop {
                match causal_child_probe_locked(&state, target, caller_tasks, &excluded) {
                    CausalChildProbe::Ready(id) => {
                        let claimed = claim_task_work_locked(&mut state, self.runtime, id, false);
                        if let Some(claimed) = claimed {
                            state.advance_work_generation(
                                CoordinatorMutationKind::WorkClaim,
                                RouteHazard::None,
                            );
                            break CausalChildSelection::Claimed(claimed);
                        } else {
                            // A demand session can close independently of this
                            // lock. Keep searching other causal children.
                            excluded.insert(id);
                        }
                    }
                    CausalChildProbe::Busy => break CausalChildSelection::Busy,
                    CausalChildProbe::None => break CausalChildSelection::None,
                }
            }
        };
        drop(mutation);
        if matches!(selection, CausalChildSelection::Claimed(_)) {
            self.notify_all(CoordinatorMutationKind::WorkClaim);
        }
        selection
    }

    pub(super) fn has_busy_causal_child(
        &self,
        target: Option<&EvaluationWaitToken>,
        caller_tasks: [Option<EvaluationTaskId>; 2],
    ) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        matches!(
            causal_child_probe_locked(&state, target, caller_tasks, &TrustedHashSet::default()),
            CausalChildProbe::Busy
        )
    }

    pub(super) fn work_for_wait(&self, wait: &EvaluationWaitToken) -> Option<EvaluationWorkId> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        work_for_wait_locked(&state, wait)
    }

    #[cfg(test)]
    pub(super) fn producer_for_wait(&self, wait: &EvaluationWaitToken) -> Option<EvaluationTaskId> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        work_for_wait_locked(&state, wait)
            .and_then(|id| state.work.get(&id))
            .and_then(task_for_record)
    }

    pub(super) fn task_origin_for_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<(EvaluationTaskId, EvaluationSessionId)> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let record = state.work.get(&work_for_wait_locked(&state, wait)?)?;
        Some((task_for_record(record)?, record.demand_session))
    }

    pub(super) fn register_task_promise(
        self: &Arc<Self>,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
        root: ManagedPromiseRoot,
    ) -> Result<Arc<PromiseProducerObligation>, Arc<str>> {
        self.register_task_promise_with_terminal(
            task,
            wait,
            root,
            TaskPromiseTerminalMapper::UnresolvedFailure,
        )
    }

    pub(super) fn register_task_promise_with_terminal(
        self: &Arc<Self>,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
        root: ManagedPromiseRoot,
        terminal: TaskPromiseTerminalMapper,
    ) -> Result<Arc<PromiseProducerObligation>, Arc<str>> {
        debug_assert_eq!(wait.runtime_id(), self.runtime);
        let promise = root.id();
        let mutation = self.admission.mutation_guard();
        let producer = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let work = state
                .reflection
                .by_task
                .get(&task)
                .or_else(|| state.deferred.by_task.get(&task))
                .copied()
                .ok_or_else(|| {
                    Arc::<str>::from(format!(
                        "task {} has no active work record for its promise",
                        task.get()
                    ))
                })?;
            let record = state
                .work
                .get_mut(&work)
                .expect("indexed promise producer work must remain registered");
            if record.control.close_reason.is_some() {
                return Err(Arc::from("evaluation demand session is closed"));
            }
            if !matches!(
                record.state,
                WorkState::Dormant | WorkState::Reserved | WorkState::Running
            ) {
                return Err(Arc::from(
                    "a promise cannot be added after its producer stopped running",
                ));
            }
            let producer = Arc::new(PromiseProducerObligation::coordinator_owned(
                task,
                record.demand_session,
                &wait,
                work,
                promise,
                self,
            ));
            record
                .obligations
                .add_owned_promise(TaskOwnedPromiseObligation {
                    promise,
                    root,
                    producer: producer.clone(),
                    wait: wait.clone(),
                    terminal,
                });
            assert!(
                state.promise_by_wait.insert(wait, work).is_none(),
                "evaluation wait tokens must be unique"
            );
            state.advance_work_generation(
                CoordinatorMutationKind::TaskPromiseIndexAdmission,
                RouteHazard::None,
            );
            producer
        };
        drop(mutation);
        self.notify_all(CoordinatorMutationKind::TaskPromiseIndexAdmission);
        Ok(producer)
    }

    pub(super) fn complete_task_promise_guarded(
        &self,
        _mutation: &dyn RuntimeMutationAuthority,
        work: EvaluationWorkId,
        wait: &EvaluationWaitToken,
        promise: PromiseId,
    ) -> Option<ManagedPromiseRoot> {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        if state.promise_by_wait.get(wait).copied() != Some(work) {
            return None;
        }
        let record = state
            .work
            .get_mut(&work)
            .expect("indexed promise producer work must remain registered");
        let obligation = record
            .obligations
            .take_owned_promise(wait, promise)
            .expect("promise wait index must agree with its producer obligation");
        debug_assert_eq!(obligation.promise, promise);
        assert_eq!(state.promise_by_wait.remove(wait), Some(work));
        state.advance_work_generation(
            CoordinatorMutationKind::TaskPromiseIndexRetirement,
            RouteHazard::All,
        );
        drop(state);
        Some(obligation.root)
    }

    /// Consumes one terminalizing work record's producer obligation and
    /// publishes its failure-ledger decision, wait terminal, and protected
    /// status query under one runtime mutation admission before the record may
    /// retire.
    pub(super) fn settle_terminal_work(
        self: &Arc<Self>,
        work: EvaluationWorkId,
        terminal: EvaluationWaitTerminal,
        promise_failure: Arc<EvaluationFailure>,
    ) -> EvaluationWaitTerminal {
        debug_assert!(
            !matches!(terminal, EvaluationWaitTerminal::Panicked(_)),
            "panicked work settles through settle_panicked_work"
        );
        self.settle_work(work, terminal, Some(promise_failure))
    }

    /// Settles work interrupted by a panic.
    ///
    /// The wait and task status become `Panicked`. Owned promises stay
    /// unassigned and record the panic on their producer obligation, because
    /// a panic is never a semantic result.
    pub(super) fn settle_panicked_work(
        self: &Arc<Self>,
        work: EvaluationWorkId,
        report: EvaluationPanic,
    ) -> EvaluationWaitTerminal {
        self.settle_work(work, EvaluationWaitTerminal::Panicked(report), None)
    }

    fn settle_work(
        self: &Arc<Self>,
        work: EvaluationWorkId,
        terminal: EvaluationWaitTerminal,
        promise_failure: Option<Arc<EvaluationFailure>>,
    ) -> EvaluationWaitTerminal {
        let mutation = self.admission.mutation_guard();
        let (producer, status_update, promises) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let (producer, failure, status_update, promises) = {
                let record = state
                    .work
                    .get_mut(&work)
                    .expect("terminalizing work must remain registered");
                assert!(matches!(record.state, WorkState::Terminalizing));
                let failure = match (&record.kind, &terminal) {
                    (WorkKind::Reflection(reflection), EvaluationWaitTerminal::Failed(error))
                        if !reflection.failure_reporting.acknowledged =>
                    {
                        Some((
                            reflection.failure_reporting.owner_session,
                            reflection.task,
                            error.clone(),
                        ))
                    }
                    _ => None,
                };
                let mut producer = record
                    .obligations
                    .take_producer()
                    .expect("work producer obligations must be consumed exactly once");
                let status_update = match &mut producer {
                    ProducerSettlementObligation::ReflectionTask(publisher) => {
                        publisher.update_status(terminal_task_status(&terminal), true)
                    }
                    ProducerSettlementObligation::DeferredClaim { .. } => Vec::new(),
                };
                let promises = record.obligations.owned_promises.clone();
                (producer, failure, status_update, promises)
            };
            if let Some((owner, task, failure)) = failure {
                insert_task_failure(&mut state.failures, owner, task, failure.clone());
                insert_task_failure(&mut state.pending_failure_reports, owner, task, failure);
                state.advance_work_generation(
                    CoordinatorMutationKind::FailureLedger,
                    RouteHazard::None,
                );
            }
            (producer, status_update, promises)
        };
        let wait = match &producer {
            ProducerSettlementObligation::ReflectionTask(publisher) => &publisher.wait,
            ProducerSettlementObligation::DeferredClaim { wait, producer } => {
                let _producer = producer.id();
                wait
            }
        };
        let (terminal, wake) = wait.publish_terminal_guarded(self, &mutation, terminal);
        let mut completion_wakes = vec![wake];
        let mut status_wakes = Vec::with_capacity(status_update.len());
        let mut status_publishers = Vec::with_capacity(status_update.len());
        for update in status_update {
            debug_assert_eq!(update.status, terminal_task_status(&terminal));
            let publisher = update.publisher.clone();
            status_wakes.push(publisher.publish_update_guarded(&mutation, update));
            status_publishers.push(publisher);
        }
        let mut promise_publications = Vec::with_capacity(promises.len());
        for obligation in promises {
            let (producer, completion) = match (&terminal, &promise_failure) {
                (EvaluationWaitTerminal::Panicked(report), _) => {
                    obligation.publish_panicked_guarded(self, &mutation, report)
                }
                (_, Some(failure)) => {
                    obligation.publish_terminal_guarded(self, &mutation, &terminal, failure)
                }
                (_, None) => unreachable!("only panicked settlement omits its promise failure"),
            };
            promise_publications.push(producer);
            completion_wakes.push(completion);
        }
        {
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get(&work)
                .expect("settled work must remain registered for reporting cleanup");
            assert!(
                record.obligations.is_empty(),
                "terminal settlement must consume every work obligation"
            );
        }

        #[cfg(test)]
        self.run_terminal_publication_probe();
        drop(mutation);

        // The deferred producer clone, exact wakes, status publishers, and any
        // values they release are disposed only after coordinator/component
        // locks and mutation admission have been released.
        drop(producer);
        // Deliver lifecycle/status callbacks before waking parked completion
        // subscribers. The terminal cells are already authoritative, so a
        // direct poller may observe them before either notification; callback
        // completion is not part of the semantic terminal state.
        for status_wake in status_wakes {
            status_wake.notify();
        }
        for wake in completion_wakes {
            wake.notify();
        }
        for publication in promise_publications {
            publication.notify();
        }
        drop(status_publishers);
        terminal
    }

    pub(super) fn task_dependency(&self, task: EvaluationTaskId) -> Option<WorkDependency> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let id = state
            .reflection
            .by_task
            .get(&task)
            .or_else(|| state.deferred.by_task.get(&task))?;
        let record = state.work.get(id)?;
        match &record.kind {
            WorkKind::Reflection(work) => work.block.as_ref(),
            WorkKind::Deferred(work) => work.block.as_ref(),
            WorkKind::LazyRoute(work) => work.block.as_ref(),
            WorkKind::Spark(_) => None,
        }
        .and_then(|block| block.dependency.clone())
    }

    pub(super) fn work_dependency_by_id(&self, id: EvaluationWorkId) -> Option<WorkDependency> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state.work.get(&id).and_then(work_dependency).cloned()
    }

    pub(super) fn work_observed_epoch(
        &self,
        id: EvaluationWorkId,
    ) -> Option<RuntimeObservationEpoch> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state.work.get(&id).and_then(task_observation_epoch)
    }

    #[cfg(test)]
    pub(super) fn work_is_busy(&self, id: EvaluationWorkId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state.work.get(&id).is_some_and(|record| {
            matches!(
                record.state,
                WorkState::Reserved
                    | WorkState::Running
                    | WorkState::InlineForced
                    | WorkState::Terminalizing
            )
        })
    }

    #[cfg(test)]
    pub(super) fn task_is_claimable(&self, task: EvaluationTaskId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let id = state
            .reflection
            .by_task
            .get(&task)
            .or_else(|| state.deferred.by_task.get(&task));
        id.and_then(|id| state.work.get(id))
            .is_some_and(|record| matches!(record.state, WorkState::Dormant | WorkState::Queued))
    }

    pub(super) fn dependency_observes_runtime(&self, target: &EvaluationWaitToken) -> bool {
        let mut seen = std::collections::HashSet::new();
        let mut wait = target.clone();
        while seen.insert(wait.get()) {
            let Some(work) = self.work_for_wait(&wait) else {
                return false;
            };
            if self.work_observed_epoch(work).is_some() {
                return true;
            }
            let Some(dependency) = self.work_dependency_by_id(work) else {
                return false;
            };
            let Some(dependency_wait) = dependency.producer_wait() else {
                return false;
            };
            wait = dependency_wait.clone();
        }
        false
    }

    /// Reports only progress which a session drain is authorized to await:
    /// its reflection roots and the exact producer chains beneath them.
    pub(super) fn session_background_is_busy(&self, session: EvaluationSessionId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state.background_roots.iter().any(|root| {
            let Some(root_record) = state.work.get(root) else {
                return false;
            };
            if root_record.demand_session != session
                || !matches!(root_record.kind, WorkKind::Reflection(_))
            {
                return false;
            }
            matches!(
                causal_background_probe_locked(&state, *root),
                CausalBackgroundProbe::Busy(_)
            )
        })
    }

    #[cfg(test)]
    pub(super) fn task_promise_count(&self, session: EvaluationSessionId) -> usize {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state
            .promise_by_wait
            .values()
            .filter(|work| {
                state
                    .work
                    .get(work)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count()
    }

    #[cfg(test)]
    pub(super) fn client_demand_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .client_demands
            .len()
    }

    /// Waits on the coordinator's observed generation, optionally bounded by
    /// an idle timeout. The predicate is checked under the same mutex as
    /// publication, so a change preceding this call cannot become a lost
    /// wake. The timeout never interrupts an active machine poll.
    #[cfg(test)]
    pub(super) fn wait_for_change_with_timeout(
        &self,
        observed_generation: u64,
        timeout: Option<Duration>,
    ) -> bool {
        let changed = self.wait_for_change_for(
            observed_generation,
            timeout,
            CoordinatorWaiterClass::TaskObserver,
        );
        if changed {
            self.record_waiter_outcome(
                CoordinatorWaiterClass::TaskObserver,
                CoordinatorWaiterOutcome::Relevant,
            );
        }
        changed
    }

    pub(super) fn wait_for_change_for(
        &self,
        observed_generation: u64,
        timeout: Option<Duration>,
        class: CoordinatorWaiterClass,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        #[cfg(test)]
        if state.work_generation == observed_generation
            && let Some(probe) = self.work_wait_probe.get()
        {
            let _ = probe.send(());
        }
        if let Some(timeout) = timeout {
            let (current, _) = self
                .work_available
                .wait_timeout_while(state, timeout, |state| {
                    state.work_generation == observed_generation
                })
                .expect("evaluation work coordinator was poisoned");
            let changed = current.work_generation != observed_generation;
            drop(current);
            if changed {
                self.record_waiter_release(class);
            }
            return changed;
        }
        while state.work_generation == observed_generation {
            state = self
                .work_available
                .wait(state)
                .expect("evaluation work coordinator was poisoned");
        }
        drop(state);
        self.record_waiter_release(class);
        true
    }

    #[cfg(test)]
    pub(super) fn set_work_wait_probe(&self, probe: std::sync::mpsc::Sender<()>) {
        self.work_wait_probe
            .set(probe)
            .unwrap_or_else(|_| panic!("work wait probe can only be installed once"));
    }

    /// Queues every blocked task whose retained retry checkpoint predates a
    /// newly published semantic-state epoch. The caller retains shared
    /// runtime mutation admission across epoch publication and this pass.
    pub(crate) fn publish_runtime_observation_guarded(
        &self,
        _mutation: &dyn RuntimeMutationAuthority,
        epoch: RuntimeObservationEpoch,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let registrations = state
            .observation_waiters
            .values()
            .copied()
            .filter(|registration| registration.observed_epoch < epoch)
            .collect::<Vec<_>>();
        let mut changed = false;
        for registration in registrations {
            changed |= queue_current_observation(&mut state, registration, epoch);
        }
        if changed {
            state.advance_work_generation(
                CoordinatorMutationKind::ObservationWake,
                RouteHazard::All,
            );
        }
        changed
    }

    pub(crate) fn notify_runtime_observation(&self, changed: bool) {
        if changed {
            self.notify_all(CoordinatorMutationKind::ObservationWake);
        }
    }

    /// Completes subscribe-and-recheck after a task publishes a blocked
    /// observation registration. Runtime mutation admission remains held by
    /// the caller, so a publisher either precedes this recheck or observes the
    /// installed registration itself.
    fn recheck_observation_wait(&self, id: EvaluationWorkId) -> bool {
        let current_epoch = self.observations.current();
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(registration) = state.observation_waiters.get(&id).copied() else {
            return false;
        };
        let changed = queue_current_observation(&mut state, registration, current_epoch);
        if changed {
            state.advance_work_generation(
                CoordinatorMutationKind::ObservationWake,
                RouteHazard::All,
            );
        }
        changed
    }

    fn publish_transition(
        &self,
        kind: CoordinatorMutationKind,
        transition: impl FnOnce(&mut WorkCoordinatorState),
    ) {
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            transition(&mut state);
            state.advance_work_generation(kind, RouteHazard::None);
        }
        drop(mutation);
        self.notify_all(kind);
    }

    pub(super) fn demand_session_is_open(&self, session: EvaluationSessionId) -> bool {
        let demand = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .demand_sessions
            .get(&session)
            .cloned();
        demand
            .and_then(|demand| demand.upgrade())
            .is_some_and(|demand| !demand.is_closed())
    }

    #[cfg(test)]
    pub(crate) fn registered_session_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .demand_sessions
            .len()
    }

    #[cfg(test)]
    pub(super) fn reflection_work_for_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<EvaluationWorkId> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state
            .reflection
            .by_wait
            .get(wait)
            .or_else(|| state.promise_by_wait.get(wait))
            .copied()
    }

    #[cfg(test)]
    pub(super) fn reflection_counts(&self, session: EvaluationSessionId) -> (usize, usize) {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let active = state
            .work_by_session
            .get(&session)
            .into_iter()
            .flatten()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| matches!(record.kind, WorkKind::Reflection(_)))
            })
            .count();
        let indexed = state
            .reflection
            .by_task
            .values()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count();
        (active, indexed)
    }

    #[cfg(test)]
    pub(crate) fn ready_task_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .ready_task_set
            .len()
    }

    #[cfg(test)]
    pub(crate) fn spark_work_counts(&self) -> (usize, usize, usize) {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let mut queued = 0;
        let mut running = 0;
        let mut blocked = 0;
        for record in state.work.values() {
            if !matches!(record.kind, WorkKind::Spark(_)) {
                continue;
            }
            match record.state {
                WorkState::Queued => queued += 1,
                WorkState::Running => running += 1,
                WorkState::Blocked => blocked += 1,
                WorkState::Dormant
                | WorkState::Reserved
                | WorkState::InlineForced
                | WorkState::ExitWaiting
                | WorkState::Terminalizing => {}
            }
        }
        (queued, running, blocked)
    }

    #[cfg(test)]
    pub(crate) fn retained_spark_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .work
            .values()
            .filter(|record| matches!(record.kind, WorkKind::Spark(_)))
            .count()
    }
}

fn task_for_record(record: &WorkRecord) -> Option<EvaluationTaskId> {
    match &record.kind {
        WorkKind::Reflection(work) => Some(work.task),
        WorkKind::Deferred(work) => Some(work.task),
        WorkKind::LazyRoute(_) => None,
        WorkKind::Spark(_) => None,
    }
}

fn task_block(record: &WorkRecord) -> Option<&EvaluationTaskBlock> {
    match &record.kind {
        WorkKind::Reflection(work) => work.block.as_ref(),
        WorkKind::Deferred(work) => work.block.as_ref(),
        WorkKind::LazyRoute(work) => work.block.as_ref(),
        WorkKind::Spark(_) => None,
    }
}

fn task_observation_epoch(record: &WorkRecord) -> Option<RuntimeObservationEpoch> {
    match (&record.state, &record.kind) {
        (WorkState::Blocked, _) => task_block(record).and_then(|block| block.observed_epoch),
        (WorkState::ExitWaiting, WorkKind::Reflection(work)) => {
            work.exit.as_ref().and_then(|exit| exit.observed_epoch)
        }
        _ => None,
    }
}

fn work_dependency(record: &WorkRecord) -> Option<&WorkDependency> {
    match &record.kind {
        WorkKind::Spark(work) => work.dependency.as_ref(),
        WorkKind::Reflection(work) => work.block.as_ref()?.dependency.as_ref(),
        WorkKind::Deferred(work) => work.block.as_ref()?.dependency.as_ref(),
        WorkKind::LazyRoute(work) => work.block.as_ref()?.dependency.as_ref(),
    }
}

fn exact_route_disposition_locked(
    state: &WorkCoordinatorState,
    work: EvaluationWorkId,
) -> ExactRouteDisposition {
    let Some(record) = state.work.get(&work) else {
        return ExactRouteDisposition::Terminal;
    };
    match record.state {
        WorkState::Queued | WorkState::Dormant => ExactRouteDisposition::Runnable,
        WorkState::Reserved | WorkState::Running | WorkState::InlineForced => {
            ExactRouteDisposition::Busy
        }
        WorkState::Blocked => {
            let Some(dependency) = work_dependency(record) else {
                return ExactRouteDisposition::Parked;
            };
            let producer = dependency
                .producer_wait()
                .as_ref()
                .and_then(|wait| work_for_wait_locked(state, wait));
            ExactRouteDisposition::Blocked {
                subscription_epoch: record.subscription_epoch,
                dependency: dependency.key(),
                producer,
            }
        }
        WorkState::ExitWaiting => ExactRouteDisposition::Parked,
        WorkState::Terminalizing => ExactRouteDisposition::Terminal,
    }
}

fn work_for_wait_locked(
    state: &WorkCoordinatorState,
    wait: &EvaluationWaitToken,
) -> Option<EvaluationWorkId> {
    state
        .promise_by_wait
        .get(wait)
        .or_else(|| state.deferred.by_wait.get(wait))
        .or_else(|| state.reflection.by_wait.get(wait))
        .copied()
}

/// Finds the first claimable item on one background root's exact producer
/// chain. A queued root runs before its old block is followed; a blocked root
/// can reach a dormant deferred producer without promoting unrelated work.
/// Worker and runtime-pump selectors use this same traversal policy.
fn causal_background_probe_locked(
    state: &WorkCoordinatorState,
    root: EvaluationWorkId,
) -> CausalBackgroundProbe {
    let Some(root_record) = state.work.get(&root) else {
        return CausalBackgroundProbe::None;
    };
    if !matches!(
        root_record.kind,
        WorkKind::Reflection(_) | WorkKind::Spark(_)
    ) {
        return CausalBackgroundProbe::None;
    }
    exact_producer_probe_locked(state, root).selection
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExactProducerProbe {
    selection: CausalBackgroundProbe,
    depth: usize,
}

#[cfg(test)]
fn exact_target_probe_locked(
    state: &WorkCoordinatorState,
    target: &EvaluationWaitToken,
) -> ExactProducerProbe {
    let Some(root) = work_for_wait_locked(state, target) else {
        return ExactProducerProbe {
            selection: CausalBackgroundProbe::None,
            depth: 0,
        };
    };
    exact_producer_probe_locked(state, root)
}

fn exact_route_probe_locked(
    state: &WorkCoordinatorState,
    target: &EvaluationWaitToken,
    route: &mut ExactDemandRoute,
) -> (ExactProducerProbe, usize, Option<ExactRouteFallbackReason>) {
    let route_hazard_revision = route.hazard_revision;
    let current_hazard_revision = state.exact_route_hazard_revision;
    if route_hazard_revision == Some(current_hazard_revision) && route.current.is_some() {
        route.generation = Some(state.work_generation);
        let (probe, handoffs) = continue_exact_route_locked(state, route);
        return (probe, handoffs, None);
    }
    if route_hazard_revision.is_some() {
        return match validate_exact_route_locked(state, target, route, true) {
            Ok(validation_handoffs) => {
                route.generation = Some(state.work_generation);
                route.hazard_revision = Some(current_hazard_revision);
                let (probe, handoffs) = continue_exact_route_locked(state, route);
                (probe, validation_handoffs + handoffs, None)
            }
            Err(reason) => {
                route.reset_to_target(target);
                (
                    rebuild_exact_route_locked(state, target, route),
                    0,
                    Some(reason),
                )
            }
        };
    }

    let fallback = route.invalidation.take();
    route.reset_to_target(target);
    let probe = rebuild_exact_route_locked(state, target, route);
    route.generation = Some(state.work_generation);
    route.hazard_revision = Some(current_hazard_revision);
    (probe, 0, fallback)
}

fn rebuild_exact_route_locked(
    state: &WorkCoordinatorState,
    target: &EvaluationWaitToken,
    route: &mut ExactDemandRoute,
) -> ExactProducerProbe {
    let Some(root) = work_for_wait_locked(state, target) else {
        return ExactProducerProbe {
            selection: CausalBackgroundProbe::None,
            depth: 0,
        };
    };
    route.current = Some(root);
    route.members.insert(root, 0);
    let mut current = root;
    let mut depth = 0;
    loop {
        depth += 1;
        route.current = Some(current);
        let Some(record) = state.work.get(&current) else {
            route.current = None;
            route.parents.clear();
            return ExactProducerProbe {
                selection: CausalBackgroundProbe::None,
                depth,
            };
        };
        match record.state {
            WorkState::Queued => {
                return ExactProducerProbe {
                    selection: CausalBackgroundProbe::Ready(current),
                    depth,
                };
            }
            WorkState::Dormant
                if matches!(record.kind, WorkKind::Deferred(_) | WorkKind::LazyRoute(_)) =>
            {
                return ExactProducerProbe {
                    selection: CausalBackgroundProbe::Ready(current),
                    depth,
                };
            }
            WorkState::Blocked => {
                let Some(dependency) = work_dependency(record) else {
                    return ExactProducerProbe {
                        selection: CausalBackgroundProbe::None,
                        depth,
                    };
                };
                let Some(wait) = dependency.producer_wait() else {
                    return ExactProducerProbe {
                        selection: CausalBackgroundProbe::None,
                        depth,
                    };
                };
                let Some(producer) = work_for_wait_locked(state, &wait) else {
                    return ExactProducerProbe {
                        selection: CausalBackgroundProbe::None,
                        depth,
                    };
                };
                if !route.admit_producer(producer) {
                    return ExactProducerProbe {
                        selection: CausalBackgroundProbe::None,
                        depth,
                    };
                }
                route.parents.push(ExactDemandRouteFrame {
                    work: current,
                    subscription_epoch: record.subscription_epoch,
                    dependency: dependency.key(),
                });
                current = producer;
            }
            WorkState::Reserved
            | WorkState::Running
            | WorkState::InlineForced
            | WorkState::Terminalizing => {
                return ExactProducerProbe {
                    selection: CausalBackgroundProbe::Busy(current),
                    depth,
                };
            }
            WorkState::Dormant | WorkState::ExitWaiting => {
                return ExactProducerProbe {
                    selection: CausalBackgroundProbe::None,
                    depth,
                };
            }
        }
    }
}

fn validate_exact_route_locked(
    state: &WorkCoordinatorState,
    target: &EvaluationWaitToken,
    route: &mut ExactDemandRoute,
    recover_missing_tip: bool,
) -> Result<usize, ExactRouteFallbackReason> {
    let Some(current) = route.current else {
        return Err(ExactRouteFallbackReason::RetiredWork);
    };
    if !state.work.contains_key(&current) {
        if recover_missing_tip && route.parents.len() == 1 {
            let root = route.parents[0].work;
            if work_for_wait_locked(state, target) == Some(root) && state.work.contains_key(&root) {
                route.members.remove(&current);
                route.parents.clear();
                route.current = Some(root);
                return Ok(1);
            }
        }
        return Err(ExactRouteFallbackReason::RetiredWork);
    }
    let root = route.parents.first().map_or(current, |frame| frame.work);
    match work_for_wait_locked(state, target) {
        None => return Err(ExactRouteFallbackReason::RetiredWork),
        Some(actual) if actual != root => {
            return Err(ExactRouteFallbackReason::BranchedWork);
        }
        Some(_) => {}
    }
    // Frames above the lowest touched member are unchanged since the route
    // was last validated. A touched member at depth `d` can change its own
    // frame and the link into it from frame `d - 1`.
    let start = match route.hazard_revision {
        Some(validated) => match state.exact_route_hazard_start(route, validated) {
            Some(depth) => depth.saturating_sub(1),
            None => return Ok(0),
        },
        None => 0,
    };
    #[cfg(any(test, feature = "glam-prof"))]
    state.exact_route_validated_frames.fetch_add(
        route.parents.len().saturating_sub(start) as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    for (index, frame) in route.parents.iter().enumerate().skip(start) {
        let Some(record) = state.work.get(&frame.work) else {
            return Err(ExactRouteFallbackReason::RetiredWork);
        };
        if !matches!(record.state, WorkState::Blocked)
            || record.subscription_epoch != frame.subscription_epoch
        {
            return Err(ExactRouteFallbackReason::ChangedDependency);
        }
        let Some(dependency) = work_dependency(record) else {
            return Err(ExactRouteFallbackReason::ChangedDependency);
        };
        if dependency.key() != frame.dependency {
            return Err(ExactRouteFallbackReason::ChangedDependency);
        }
        let expected = route
            .parents
            .get(index + 1)
            .map_or(current, |next| next.work);
        let Some(producer) = dependency
            .producer_wait()
            .as_ref()
            .and_then(|wait| work_for_wait_locked(state, wait))
        else {
            return Err(ExactRouteFallbackReason::RetiredWork);
        };
        if producer != expected {
            return Err(ExactRouteFallbackReason::BranchedWork);
        }
    }
    Ok(0)
}

/// Advances a route already known to describe the coordinator's current
/// generation. Each loop iteration is one real scheduler edge transition,
/// rather than a repeated walk from the root.
fn continue_exact_route_locked(
    state: &WorkCoordinatorState,
    route: &mut ExactDemandRoute,
) -> (ExactProducerProbe, usize) {
    let mut handoffs = 0;
    loop {
        let Some(current) = route.current else {
            return (
                ExactProducerProbe {
                    selection: CausalBackgroundProbe::None,
                    depth: 0,
                },
                handoffs,
            );
        };
        let Some(record) = state.work.get(&current) else {
            route.members.remove(&current);
            let Some(parent) = route.parents.pop() else {
                route.current = None;
                return (
                    ExactProducerProbe {
                        selection: CausalBackgroundProbe::None,
                        depth: 0,
                    },
                    handoffs,
                );
            };
            route.current = Some(parent.work);
            handoffs += 1;
            continue;
        };
        match record.state {
            WorkState::Queued => {
                return (
                    ExactProducerProbe {
                        selection: CausalBackgroundProbe::Ready(current),
                        depth: 0,
                    },
                    handoffs,
                );
            }
            WorkState::Dormant
                if matches!(record.kind, WorkKind::Deferred(_) | WorkKind::LazyRoute(_)) =>
            {
                return (
                    ExactProducerProbe {
                        selection: CausalBackgroundProbe::Ready(current),
                        depth: 0,
                    },
                    handoffs,
                );
            }
            WorkState::Blocked => {
                let Some(dependency) = work_dependency(record) else {
                    return (
                        ExactProducerProbe {
                            selection: CausalBackgroundProbe::None,
                            depth: 0,
                        },
                        handoffs,
                    );
                };
                let Some(wait) = dependency.producer_wait() else {
                    return (
                        ExactProducerProbe {
                            selection: CausalBackgroundProbe::None,
                            depth: 0,
                        },
                        handoffs,
                    );
                };
                let Some(producer) = work_for_wait_locked(state, &wait) else {
                    return (
                        ExactProducerProbe {
                            selection: CausalBackgroundProbe::None,
                            depth: 0,
                        },
                        handoffs,
                    );
                };
                if !route.admit_producer(producer) {
                    return (
                        ExactProducerProbe {
                            selection: CausalBackgroundProbe::None,
                            depth: 0,
                        },
                        handoffs,
                    );
                }
                route.parents.push(ExactDemandRouteFrame {
                    work: current,
                    subscription_epoch: record.subscription_epoch,
                    dependency: dependency.key(),
                });
                route.current = Some(producer);
                handoffs += 1;
            }
            WorkState::Reserved
            | WorkState::Running
            | WorkState::InlineForced
            | WorkState::Terminalizing => {
                return (
                    ExactProducerProbe {
                        selection: CausalBackgroundProbe::Busy(current),
                        depth: 0,
                    },
                    handoffs,
                );
            }
            WorkState::Dormant | WorkState::ExitWaiting => {
                return (
                    ExactProducerProbe {
                        selection: CausalBackgroundProbe::None,
                        depth: 0,
                    },
                    handoffs,
                );
            }
        }
    }
}

/// Walks one exact producer route using each record's current scheduling
/// state. Prior blocks are meaningful only while their record is blocked.
fn exact_producer_probe_locked(
    state: &WorkCoordinatorState,
    root: EvaluationWorkId,
) -> ExactProducerProbe {
    let mut current = root;
    let mut seen = TrustedWorkIdSet::default();
    while seen.insert(current) {
        let Some(record) = state.work.get(&current) else {
            return ExactProducerProbe {
                selection: CausalBackgroundProbe::None,
                depth: seen.len(),
            };
        };
        match record.state {
            WorkState::Queued => {
                return ExactProducerProbe {
                    selection: CausalBackgroundProbe::Ready(current),
                    depth: seen.len(),
                };
            }
            WorkState::Dormant
                if matches!(record.kind, WorkKind::Deferred(_) | WorkKind::LazyRoute(_)) =>
            {
                return ExactProducerProbe {
                    selection: CausalBackgroundProbe::Ready(current),
                    depth: seen.len(),
                };
            }
            WorkState::Blocked => {
                let Some(wait) = work_dependency(record).and_then(WorkDependency::producer_wait)
                else {
                    return ExactProducerProbe {
                        selection: CausalBackgroundProbe::None,
                        depth: seen.len(),
                    };
                };
                let Some(producer) = work_for_wait_locked(state, &wait) else {
                    return ExactProducerProbe {
                        selection: CausalBackgroundProbe::None,
                        depth: seen.len(),
                    };
                };
                current = producer;
            }
            WorkState::Reserved
            | WorkState::Running
            | WorkState::InlineForced
            | WorkState::Terminalizing => {
                return ExactProducerProbe {
                    selection: CausalBackgroundProbe::Busy(current),
                    depth: seen.len(),
                };
            }
            WorkState::Dormant | WorkState::ExitWaiting => {
                return ExactProducerProbe {
                    selection: CausalBackgroundProbe::None,
                    depth: seen.len(),
                };
            }
        }
    }
    ExactProducerProbe {
        selection: CausalBackgroundProbe::None,
        depth: seen.len(),
    }
}

fn causal_background_candidate_locked(
    state: &WorkCoordinatorState,
    root: EvaluationWorkId,
) -> Option<EvaluationWorkId> {
    match causal_background_probe_locked(state, root) {
        CausalBackgroundProbe::Ready(candidate) => Some(candidate),
        CausalBackgroundProbe::Busy(_) | CausalBackgroundProbe::None => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CausalBackgroundProbe {
    Ready(EvaluationWorkId),
    Busy(EvaluationWorkId),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CausalChildProbe {
    Ready(EvaluationWorkId),
    Busy,
    None,
}

/// Walks task-launch edges from the caller and target's exact producer chain.
/// Each launched child may itself block through an exact producer or launch
/// further children. A claimed route remains visible as Busy; it must not be
/// mistaken for stable absence just because its machine was detached.
fn causal_child_probe_locked(
    state: &WorkCoordinatorState,
    target: Option<&EvaluationWaitToken>,
    caller_tasks: [Option<EvaluationTaskId>; 2],
    excluded: &TrustedHashSet<EvaluationWorkId>,
) -> CausalChildProbe {
    let mut tasks = VecDeque::new();
    let mut seen_tasks = TrustedHashSet::default();
    for task in caller_tasks.into_iter().flatten() {
        if seen_tasks.insert(task) {
            tasks.push_back(task);
        }
    }

    let mut seen_exact = TrustedHashSet::default();
    let mut wait = target.cloned();
    while let Some(work) = wait
        .as_ref()
        .and_then(|wait| work_for_wait_locked(state, wait))
    {
        if !seen_exact.insert(work) {
            break;
        }
        let Some(record) = state.work.get(&work) else {
            break;
        };
        if let Some(task) = task_for_record(record)
            && seen_tasks.insert(task)
        {
            tasks.push_back(task);
        }
        let Some(next) = work_dependency(record).and_then(WorkDependency::producer_wait) else {
            break;
        };
        wait = Some(next.clone());
    }

    let mut seen_child_work = TrustedHashSet::default();
    let mut busy = false;
    while let Some(parent) = tasks.pop_front() {
        let Some(children) = state.reflection.children_by_parent.get(&parent) else {
            continue;
        };
        for root in children {
            let mut current = *root;
            while seen_child_work.insert(current) {
                let Some(record) = state.work.get(&current) else {
                    break;
                };
                if let Some(task) = task_for_record(record)
                    && seen_tasks.insert(task)
                {
                    tasks.push_back(task);
                }
                match record.state {
                    WorkState::Queued
                        if !excluded.contains(&current)
                            && !demand_session_is_closed(state, record.demand_session) =>
                    {
                        return CausalChildProbe::Ready(current);
                    }
                    WorkState::Dormant
                        if matches!(
                            record.kind,
                            WorkKind::Deferred(_) | WorkKind::LazyRoute(_)
                        ) && !excluded.contains(&current)
                            && !demand_session_is_closed(state, record.demand_session) =>
                    {
                        return CausalChildProbe::Ready(current);
                    }
                    WorkState::Reserved
                    | WorkState::Running
                    | WorkState::InlineForced
                    | WorkState::Terminalizing => {
                        busy = true;
                        break;
                    }
                    WorkState::Blocked => {
                        let Some(next) = work_dependency(record)
                            .and_then(WorkDependency::producer_wait)
                            .and_then(|wait| work_for_wait_locked(state, &wait))
                        else {
                            break;
                        };
                        current = next;
                    }
                    WorkState::Dormant | WorkState::Queued | WorkState::ExitWaiting => break,
                }
            }
        }
    }
    if busy {
        CausalChildProbe::Busy
    } else {
        CausalChildProbe::None
    }
}

fn claim_causal_background(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    include_sparks: bool,
) -> CoordinatorSelection {
    let preferred_kinds = if state.prefer_spark {
        [true, false]
    } else {
        [false, true]
    };
    for spark_root in preferred_kinds {
        if spark_root && !include_sparks {
            continue;
        }
        for index in 0..state.background_roots.len() {
            let root = state.background_roots[index];
            let Some(root_record) = state.work.get(&root) else {
                continue;
            };
            if matches!(root_record.kind, WorkKind::Spark(_)) != spark_root {
                continue;
            }
            let Some(candidate) = causal_background_candidate_locked(state, root) else {
                continue;
            };
            let Some(record) = state.work.get(&candidate) else {
                continue;
            };
            let claimed = match record.kind {
                WorkKind::Spark(_) => {
                    claim_spark(state, runtime, candidate).map(CoordinatorSelection::Spark)
                }
                _ => claim_task_work_locked(state, runtime, candidate, false)
                    .map(CoordinatorSelection::Task),
            };
            if let Some(claimed) = claimed {
                let rotated = state
                    .background_roots
                    .remove(index)
                    .expect("selected background root must remain registered");
                state.background_roots.push_back(rotated);
                state.prefer_spark = !spark_root;
                return claimed;
            }
        }
    }
    CoordinatorSelection::None
}

/// Selects from reflection roots owned by one demand session while allowing
/// an exact producer chain to cross session boundaries. This deliberately
/// does not scan the session's ready queue: queue co-location is not causal
/// authority.
fn claim_causal_session_background(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    session: EvaluationSessionId,
) -> CoordinatorSelection {
    for index in 0..state.background_roots.len() {
        let root = state.background_roots[index];
        let Some(root_record) = state.work.get(&root) else {
            continue;
        };
        if root_record.demand_session != session
            || !matches!(root_record.kind, WorkKind::Reflection(_))
        {
            continue;
        }
        let Some(candidate) = causal_background_candidate_locked(state, root) else {
            continue;
        };
        let claimed = claim_task_work_locked(state, runtime, candidate, false)
            .map(CoordinatorSelection::Task);
        if let Some(claimed) = claimed {
            let rotated = state
                .background_roots
                .remove(index)
                .expect("selected session reflection root must remain registered");
            state.background_roots.push_back(rotated);
            return claimed;
        }
    }
    CoordinatorSelection::None
}

/// Reports progress already latent in one exact producer chain.
///
/// The caller holds runtime mutation admission and the coordinator-state
/// mutex, so a claimable tail, owned poll, terminal dependency, or stale
/// observation cannot disappear between this check and a client retirement.
fn dependency_has_causal_progress_locked(
    state: &WorkCoordinatorState,
    dependency: &WorkDependency,
    current_epoch: RuntimeObservationEpoch,
) -> bool {
    let mut dependency = Some(dependency.clone());
    let mut seen = TrustedHashSet::default();
    while let Some(current) = dependency {
        if current.is_terminal() {
            return true;
        }
        let Some(wait) = current.producer_wait() else {
            return false;
        };
        let Some(work) = work_for_wait_locked(state, &wait) else {
            return false;
        };
        if !seen.insert(work) {
            return false;
        }
        let Some(record) = state.work.get(&work) else {
            return false;
        };
        match record.state {
            WorkState::Dormant
            | WorkState::Reserved
            | WorkState::Queued
            | WorkState::Running
            | WorkState::InlineForced
            | WorkState::Terminalizing => return true,
            WorkState::Blocked | WorkState::ExitWaiting => {
                if task_observation_epoch(record).is_some_and(|epoch| epoch < current_epoch) {
                    return true;
                }
                dependency = work_dependency(record).cloned();
            }
        }
    }
    false
}

fn debug_assert_task_block_runtime(runtime: EvaluationRuntimeId, block: &EvaluationTaskBlock) {
    if let Some(dependency) = &block.dependency {
        debug_assert_eq!(
            dependency.runtime_id(),
            runtime,
            "published task block dependency must belong to its coordinator runtime"
        );
    }
}

fn publish_task_block_locked(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    id: EvaluationWorkId,
    block: EvaluationTaskBlock,
) -> Option<(WorkDependency, WakeRegistration)> {
    debug_assert_task_block_runtime(runtime, &block);
    assert!(
        block.dependency.is_some() || block.observed_epoch.is_some(),
        "blocked task work must publish an exact dependency or observed runtime epoch"
    );
    state.observation_waiters.remove(&id);
    let dependency = block.dependency.clone();
    let observed_epoch = block.observed_epoch;
    let record = state
        .work
        .get_mut(&id)
        .expect("blocked task work must remain registered");
    assert!(matches!(record.state, WorkState::Running));
    record.subscription_epoch = record
        .subscription_epoch
        .checked_add(1)
        .expect("evaluation work subscription epochs exhausted");
    let registration = WakeRegistration {
        work: id,
        subscription_epoch: record.subscription_epoch,
    };
    match &mut record.kind {
        WorkKind::Reflection(work) => work.block = Some(block),
        WorkKind::Deferred(work) => work.block = Some(block),
        WorkKind::LazyRoute(work) => work.block = Some(block),
        WorkKind::Spark(_) => panic!("spark work cannot publish a task block"),
    }
    record.state = WorkState::Blocked;
    if let Some(observed_epoch) = observed_epoch {
        state.observation_waiters.insert(
            id,
            ObservationRegistration {
                wake: registration,
                observed_epoch,
            },
        );
    }
    dependency.map(|dependency| (dependency, registration))
}

fn queue_current_observation(
    state: &mut WorkCoordinatorState,
    registration: ObservationRegistration,
    current_epoch: RuntimeObservationEpoch,
) -> bool {
    let id = registration.wake.work;
    let valid = state.work.get(&id).is_some_and(|record| {
        matches!(record.state, WorkState::Blocked | WorkState::ExitWaiting)
            && record.subscription_epoch == registration.wake.subscription_epoch
            && task_observation_epoch(record)
                .is_some_and(|observed| observed == registration.observed_epoch)
    });
    if !valid {
        if state.observation_waiters.get(&id) == Some(&registration) {
            state.observation_waiters.remove(&id);
        }
        return false;
    }
    if registration.observed_epoch >= current_epoch {
        return false;
    }
    state.observation_waiters.remove(&id);
    let record = state
        .work
        .get_mut(&id)
        .expect("validated observation work must remain registered");
    if matches!(record.state, WorkState::ExitWaiting) {
        let reflection = reflection_work_mut(record);
        assert!(
            reflection.machine.is_some(),
            "retryable exit work must retain its sanitized machine"
        );
        reflection.exit = None;
    }
    record.state = WorkState::Queued;
    queue_task(state, id);
    true
}

fn queue_task(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    if state.ready_task_set.insert(id) {
        state.ready_tasks.push_back(id);
    }
}

fn register_background_root(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    debug_assert!(!state.background_roots.contains(&id));
    state.background_roots.push_back(id);
}

fn unregister_background_root(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    if let Some(position) = state.background_roots.iter().position(|root| *root == id) {
        state.background_roots.remove(position);
    }
}

fn remove_ready_task(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    state.ready_task_set.remove(&id);
    state.ready_tasks.retain(|candidate| *candidate != id);
}

#[cfg(test)]
fn claim_ready_task(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    session: Option<EvaluationSessionId>,
) -> Option<ClaimedTaskWork> {
    loop {
        let position = match session {
            Some(session) => state
                .ready_tasks
                .iter()
                .position(|id| {
                    state.work.get(id).is_some_and(|record| {
                        record.demand_session == session
                            && matches!(record.kind, WorkKind::Reflection(_))
                    })
                })
                .or_else(|| {
                    state.ready_tasks.iter().position(|id| {
                        state
                            .work
                            .get(id)
                            .is_some_and(|record| record.demand_session == session)
                    })
                })?,
            None => 0,
        };
        let id = state.ready_tasks.remove(position)?;
        state.ready_task_set.remove(&id);
        let Some(record) = state.work.get(&id) else {
            continue;
        };
        let claimed = match &record.kind {
            WorkKind::Reflection(_) => claim_reflection_task(state, runtime, id),
            WorkKind::Deferred(_) => {
                claim_deferred(state, runtime, id, true).map(ClaimedTaskWork::Deferred)
            }
            WorkKind::LazyRoute(_) => {
                claim_lazy_route(state, runtime, id, true).map(ClaimedTaskWork::LazyRoute)
            }
            WorkKind::Spark(_) => None,
        };
        if let Some(claimed) = claimed {
            return Some(claimed);
        }
    }
}

fn claim_reflection_task(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    id: EvaluationWorkId,
) -> Option<ClaimedTaskWork> {
    claim_reflection(state, runtime, id).map(ClaimedTaskWork::Reflection)
}

fn claim_task_work_locked(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    id: EvaluationWorkId,
    requeue_on_yield: bool,
) -> Option<ClaimedTaskWork> {
    match state.work.get(&id)?.kind {
        WorkKind::Reflection(_) => claim_reflection_task(state, runtime, id),
        WorkKind::Deferred(_) => {
            claim_deferred(state, runtime, id, requeue_on_yield).map(ClaimedTaskWork::Deferred)
        }
        WorkKind::LazyRoute(_) => {
            claim_lazy_route(state, runtime, id, requeue_on_yield).map(ClaimedTaskWork::LazyRoute)
        }
        WorkKind::Spark(_) => None,
    }
}

fn demand_session_is_closed(state: &WorkCoordinatorState, session: EvaluationSessionId) -> bool {
    state
        .demand_sessions
        .get(&session)
        .and_then(Weak::upgrade)
        .is_none_or(|demand| demand.is_closed())
}

fn prune_closed_session_registration(
    state: &mut WorkCoordinatorState,
    session: EvaluationSessionId,
) -> bool {
    if demand_session_is_closed(state, session) {
        state.demand_sessions.remove(&session);
        true
    } else {
        false
    }
}

fn queue_current_registration(
    state: &mut WorkCoordinatorState,
    registration: WakeRegistration,
    source: Option<WorkDependencyKey>,
) -> bool {
    if let Some(record) = state.client_demands.get_mut(&registration.work) {
        if !matches!(record.state, WorkState::Blocked)
            || record.subscription_epoch != registration.subscription_epoch
            || source.is_some_and(|source| {
                record
                    .work
                    .subscription
                    .as_ref()
                    .is_none_or(|subscription| subscription.dependency.key() != source)
            })
        {
            return false;
        }
        record.state = WorkState::Queued;
        queue_client_demand(state, registration.work);
        return true;
    }

    let kind = {
        let Some(record) = state.work.get_mut(&registration.work) else {
            return false;
        };
        if !matches!(record.state, WorkState::Blocked)
            || record.subscription_epoch != registration.subscription_epoch
            || source.is_some_and(|source| {
                work_dependency(record).is_none_or(|dependency| dependency.key() != source)
            })
        {
            return false;
        }
        record.state = WorkState::Queued;
        !matches!(record.kind, WorkKind::Spark(_))
    };
    state.observation_waiters.remove(&registration.work);
    if kind {
        queue_task(state, registration.work);
    }
    true
}

#[cfg(test)]
mod tests;
