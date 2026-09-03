use std::marker::PhantomData;
use std::sync::{Arc, Mutex, Weak};

use rpds::RedBlackTreeMapSync;

use super::diagnostics::DiagnosticIngressInner;
use super::{DiagnosticIngress, Error, ReasoningFailure, Value, Values};
use crate::core::CoreValueFactory;
use crate::evaluation::{
    EvaluationExecutor, EvaluationSession, EvaluationWorkCoordinator, ReflectionTaskProfile,
    RuntimeCoordinatorReadiness, RuntimeObservationEpoch, RuntimeObservationState,
    ValidatedRuntimeSettlementPlan,
};
use crate::reflection::{
    ConflictAnalysisStrategy, ExactConflictAnalysis, ReasoningSessionId, ReflectionQueryMutation,
    ReflectionQueryWriter, ReflectionStore, RuntimeInputEndpointId, VolumeId,
};
use crate::runtime::{
    EvaluationRuntimeId, RuntimeIds, RuntimeMutationAdmission, RuntimeMutationAuthority,
    RuntimeMutationGuard, RuntimeSettlementGuard, RuntimeValueRoot, allocate_evaluation_runtime_id,
};

mod events;
mod readiness;

pub use events::*;
use events::{
    RuntimeDeliveryState, RuntimeEventState, RuntimeInputBuffer, runtime_delivery_failure_snapshot,
};
pub(in crate::api) use events::{
    RuntimeDiagnosticRouteMode, RuntimePreparedInput, configure_runtime_diagnostic_fallback,
    register_runtime_diagnostic_route, route_runtime_diagnostic_guarded,
    set_runtime_diagnostic_route, set_runtime_diagnostic_route_guarded,
};
pub use readiness::*;
use readiness::{
    RuntimeSettlementSnapshot, reasoning_diagnostic, runtime_deadlock_work_from_snapshot,
    runtime_disposition_from_snapshot, runtime_killed_failure,
};

/// Opaque background execution resources shared by related evaluation
/// sessions, including the assembler, logger, and future IDE services.
#[derive(Clone)]
pub struct EvaluationRuntime {
    pub(super) state: Arc<RuntimeState>,
    pub(super) default_reflection_profile: Arc<ReflectionTaskProfile>,
}

pub(super) struct RuntimeState {
    pub(super) executor: Arc<EvaluationExecutor>,
    pub(super) work: Arc<EvaluationWorkCoordinator>,
    pub(super) shared_resources: Arc<RuntimeSharedResources>,
    pub(super) diagnostic_ingresses: Mutex<Vec<Arc<DiagnosticIngressInner>>>,
}

/// Acyclic runtime infrastructure needed by evaluation and reflection work.
///
/// The coordinator route is deliberately weak: retaining these resources must
/// not retain the runtime scheduler, executor, public runtime wrapper, or
/// default reflection profile.
pub(crate) struct RuntimeSharedResources {
    pub(super) id: EvaluationRuntimeId,
    pub(super) values: RuntimeValueFactory,
    pub(super) transactions: RuntimeTransactionState,
    pub(super) observations: Arc<RuntimeObservationState>,
    pub(super) ids: Arc<RuntimeIds>,
    pub(super) mutation_admission: Arc<RuntimeMutationAdmission>,
    pub(super) work: Weak<EvaluationWorkCoordinator>,
}

pub(super) struct RuntimeTransactionState {
    pub(super) state: Mutex<RuntimeTransactionData>,
}

pub(super) struct RuntimeTransactionData {
    pub(super) reflection: ReflectionStore,
    pub(super) events: RuntimeEventState,
}

#[derive(Clone)]
pub(super) struct RuntimeValueFactory {
    runtime: EvaluationRuntimeId,
    core: CoreValueFactory,
}

impl RuntimeValueFactory {
    pub(super) fn root(&self, value: Value) -> Result<RuntimeValueRoot, Error> {
        value.require_runtime(self.runtime)?;
        Ok(value.0)
    }

    pub(super) fn core(&self) -> &CoreValueFactory {
        &self.core
    }
}

impl RuntimeValueRoot {
    pub(super) fn value(&self, runtime: EvaluationRuntimeId) -> Value {
        debug_assert_eq!(self.runtime_id(), runtime);
        Value(self.clone())
    }
}

pub(super) struct RuntimeObservationNotification {
    observations: Arc<RuntimeObservationState>,
    work: Option<Arc<EvaluationWorkCoordinator>>,
    scheduler_changed: Option<bool>,
}

impl RuntimeObservationNotification {
    pub(super) fn notify(self) {
        self.observations.notify_all();
        if let (Some(work), Some(changed)) = (self.work, self.scheduler_changed) {
            work.notify_runtime_observation(changed);
        }
    }
}

pub(super) fn prepare_runtime_observation(
    resources: &RuntimeSharedResources,
    mutation: &dyn RuntimeMutationAuthority,
) -> RuntimeObservationNotification {
    let epoch = resources.observations.advance();
    let work = resources.work.upgrade();
    let scheduler_changed = work
        .as_ref()
        .map(|work| work.publish_runtime_observation_guarded(mutation, epoch));
    RuntimeObservationNotification {
        observations: resources.observations.clone(),
        work,
        scheduler_changed,
    }
}

pub(in crate::api) fn publish_runtime_observation(
    resources: &RuntimeSharedResources,
    mutation: RuntimeMutationGuard<'_>,
) {
    let notification = prepare_runtime_observation(resources, &mutation);
    drop(mutation);
    notification.notify();
}

impl RuntimeSharedResources {
    fn id(&self) -> EvaluationRuntimeId {
        self.id
    }

    pub(super) fn values(&self) -> Values {
        Values {
            runtime: self.id,
            core: self.values.core().clone(),
        }
    }

    pub(super) fn root_value(&self, value: Value) -> Result<RuntimeValueRoot, Error> {
        self.values.root(value)
    }

    pub(super) fn allocate_reasoning_session_id(&self) -> ReasoningSessionId {
        ReasoningSessionId::from_u64(self.ids.reasoning_session().get())
            .expect("reasoning session IDs start at one")
    }

    pub(in crate::api) fn mutation_guard(&self) -> RuntimeMutationGuard<'_> {
        self.mutation_admission.mutation_guard()
    }

    fn publish_observation(&self, mutation: RuntimeMutationGuard<'_>) {
        publish_runtime_observation(self, mutation);
    }

    pub(super) fn reflection_snapshot(&self) -> (u64, crate::reflection::StoreSnapshot) {
        let generation = self.observations.current().get();
        let store = self
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .reflection
            .snapshot();
        (generation, store)
    }

    pub(in crate::api) fn transaction_snapshot(
        &self,
    ) -> (u64, crate::reflection::StoreSnapshot, RuntimeEventSnapshot) {
        // Reading the epoch first prevents a waiter from retaining a new epoch
        // beside stale transactional state.
        let generation = self.observations.current().get();
        let state = self
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned");
        (
            generation,
            state.reflection.snapshot(),
            state.events.snapshot(self.id),
        )
    }

    pub(in crate::api) fn try_commit_transaction(
        &self,
        store: &crate::reflection::StoreJournal,
        events: &RuntimeEventJournal,
    ) -> crate::reflection::StoreCommitResult {
        if events.runtime != self.id {
            return crate::reflection::StoreCommitResult::Conflict;
        }
        let mutation = self.mutation_guard();
        let (result, changed) = {
            let mut state = self
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            let result = state.reflection.validate(store);
            if !matches!(result, crate::reflection::StoreCommitResult::Committed) {
                return result;
            }
            if !state.events.validate(events) {
                return crate::reflection::StoreCommitResult::Conflict;
            }
            let reflection_changed = state.reflection.commit_validated(store);
            let event_changed = state.events.commit_validated(events);
            (
                crate::reflection::StoreCommitResult::Committed,
                reflection_changed || event_changed,
            )
        };
        if changed {
            self.publish_observation(mutation);
        }
        result
    }

    pub(super) fn commit_reflection(
        &self,
        journal: &crate::reflection::StoreJournal,
    ) -> crate::reflection::StoreCommitResult {
        let mutation = self.mutation_guard();
        let (result, changed) = {
            self.transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned")
                .reflection
                .try_commit_with_change(journal)
        };
        if changed {
            self.publish_observation(mutation);
        }
        result
    }

    pub(super) fn create_volume(&self, initial: Value) -> Result<VolumeId, Error> {
        initial.require_runtime(self.id)?;
        let _mutation = self.mutation_guard();
        self.transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .reflection
            .create_volume(initial)
            .map_err(|error| Error::new(error.as_ref()))
    }

    pub(super) fn revoke_volume(&self, volume: VolumeId) -> Result<Value, Error> {
        let mutation = self.mutation_guard();
        let value = {
            self.transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned")
                .reflection
                .revoke_volume(volume)
                .ok_or_else(|| {
                    Error::new(format!(
                        "reflection volume {} has already been revoked",
                        volume.get()
                    ))
                })?
        };
        self.publish_observation(mutation);
        Ok(value)
    }

    #[cfg(test)]
    pub(in crate::api) fn update_query(
        &self,
        handle: &Arc<crate::reflection::EvaluationQueryHandle>,
        result: Value,
    ) -> Result<(), Error> {
        result.require_runtime(self.id)?;
        let mutation = self.mutation_guard();
        let updated = {
            self.transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned")
                .reflection
                .update_query(handle, result)
        };
        if updated {
            self.publish_observation(mutation);
        }
        Ok(())
    }

    pub(super) fn wait_for_change(&self, observed_generation: u64) -> bool {
        self.observations
            .wait_for_change(RuntimeObservationEpoch::from_raw(observed_generation));
        true
    }

    #[cfg(test)]
    pub(in crate::api) fn reflection_root(&self) -> Value {
        self.transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .reflection
            .root()
            .clone()
    }

    fn has_running_delivery(&self) -> bool {
        self.transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .events
            .outputs
            .records
            .values()
            .any(|record| record.state == RuntimeDeliveryState::Running)
    }
}

/// Runtime-scoped capabilities needed by an external effect-task host.
///
/// Construction remains under [`EvaluationRuntime`] control. The capability
/// deliberately omits raw reflection-store access, volume lifecycle, runtime
/// identity allocation, and mutation-admission internals.
///
/// Raw runtime resources are not part of the embedding API:
///
/// ```compile_fail
/// # let runtime = glam::EvaluationRuntime::new(0).unwrap();
/// let _resources = runtime.shared_resources();
/// ```
#[derive(Clone)]
pub struct RuntimeTaskCapability {
    resources: Arc<RuntimeSharedResources>,
}

impl RuntimeTaskCapability {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.resources.id()
    }

    pub fn values(&self) -> Values {
        self.resources.values()
    }

    pub fn transaction_snapshot(
        &self,
    ) -> (u64, crate::reflection::StoreSnapshot, RuntimeEventSnapshot) {
        self.resources.transaction_snapshot()
    }

    pub fn try_commit_transaction(
        &self,
        store: &crate::reflection::StoreJournal,
        events: &RuntimeEventJournal,
    ) -> crate::reflection::StoreCommitResult {
        self.resources.try_commit_transaction(store, events)
    }

    #[doc(hidden)]
    pub fn wait_for_change(&self, observed_generation: u64) -> bool {
        self.resources.wait_for_change(observed_generation)
    }
}

impl ReflectionQueryWriter for RuntimeTaskCapability {
    fn update_query_guarded(
        &self,
        mutation: ReflectionQueryMutation<'_>,
        handle: &Arc<crate::reflection::EvaluationQueryHandle>,
        result: Value,
    ) -> Box<dyn FnOnce() + Send> {
        <RuntimeSharedResources as ReflectionQueryWriter>::update_query_guarded(
            self.resources.as_ref(),
            mutation,
            handle,
            result,
        )
    }
}

impl EvaluationRuntime {
    pub fn new(worker_threads: usize) -> Result<Self, Error> {
        Self::with_conflict_analysis(worker_threads, Arc::new(ExactConflictAnalysis))
    }

    /// Constructs a runtime with its immutable reflection conflict policy.
    /// Assemblers attached later observe this policy and cannot replace it.
    pub fn with_conflict_analysis(
        worker_threads: usize,
        conflict_analysis: Arc<dyn ConflictAnalysisStrategy>,
    ) -> Result<Self, Error> {
        let id = allocate_evaluation_runtime_id();
        let ids = RuntimeIds::new();
        let values = RuntimeValueFactory {
            runtime: id,
            core: CoreValueFactory::new(id, ids.clone()),
        };
        let mutation_admission = RuntimeMutationAdmission::new();
        let observations = RuntimeObservationState::new();
        let work = EvaluationWorkCoordinator::new(
            id,
            ids.clone(),
            mutation_admission.clone(),
            observations.clone(),
        );
        values.core().attach_work_coordinator(&work);
        let executor = EvaluationExecutor::new(worker_threads, &work)
            .map_err(|error| Error::new(error.as_ref()))?;
        let shared_resources = Arc::new(RuntimeSharedResources {
            id,
            values: values.clone(),
            transactions: RuntimeTransactionState {
                state: Mutex::new(RuntimeTransactionData {
                    reflection: ReflectionStore::new(values.core().clone(), conflict_analysis),
                    events: RuntimeEventState::new(),
                }),
            },
            observations,
            ids,
            mutation_admission,
            work: Arc::downgrade(&work),
        });
        Ok(Self {
            state: Arc::new(RuntimeState {
                executor,
                work,
                shared_resources,
                diagnostic_ingresses: Mutex::new(Vec::new()),
            }),
            default_reflection_profile: Arc::new(ReflectionTaskProfile::unsealed()),
        })
    }

    pub fn id(&self) -> EvaluationRuntimeId {
        self.state.shared_resources.id()
    }

    /// Publishes activation of a diagnostic consumer and its already prepared
    /// coordinator root as one settlement-excluded runtime transition.
    pub(crate) fn activate_diagnostic_consumer(
        &self,
        ingress: &DiagnosticIngress,
        activate: impl FnOnce(&dyn RuntimeMutationAuthority) -> bool,
    ) -> Result<(), Error> {
        let resources = &self.state.shared_resources;
        if ingress.inner.sender.runtime != self.id() {
            return Err(Error::new(format!(
                "diagnostic ingress belongs to evaluation runtime {}, not {}",
                ingress.inner.sender.runtime.get(),
                self.id().get()
            )));
        }
        let owner = ingress.inner.sender.owner.upgrade().ok_or_else(|| {
            Error::new(format!(
                "evaluation runtime {} for diagnostic ingress has been dropped",
                self.id().get()
            ))
        })?;
        if !Arc::ptr_eq(resources, &owner) {
            return Err(Error::new(
                "diagnostic ingress does not belong to this evaluation runtime",
            ));
        }

        let settlement = resources.mutation_admission.settlement_guard();
        let (_transferred, notification) = set_runtime_diagnostic_route_guarded(
            resources,
            ingress.inner.sender.endpoint,
            RuntimeDiagnosticRouteMode::Active,
            &settlement,
        )?;
        assert!(
            activate(&settlement),
            "fresh diagnostic consumer reservation must activate"
        );
        drop(settlement);
        resources.mutation_admission.notify_settlement();
        if let Some(notification) = notification {
            notification.notify();
        }
        Ok(())
    }

    pub(crate) fn shared_resources(&self) -> Arc<RuntimeSharedResources> {
        self.state.shared_resources.clone()
    }

    /// Constructs the narrow runtime capability used by a custom effect-task
    /// host. Clones retain runtime-local values and transactional endpoint
    /// state, but not the runtime coordinator or executor lifecycle.
    pub fn task_capability(&self) -> Arc<RuntimeTaskCapability> {
        Arc::new(RuntimeTaskCapability {
            resources: self.state.shared_resources.clone(),
        })
    }

    pub fn worker_threads(&self) -> usize {
        self.state.executor.worker_count()
    }

    /// Returns this runtime's explicit value-construction service.
    pub fn values(&self) -> Values {
        self.state.shared_resources.values()
    }

    /// Registers a runtime-local FIFO input boundary.
    ///
    /// The converter is host policy: it runs before mutation admission and
    /// leaves the runtime untouched when it fails. The returned sender and
    /// reader retain only weak links to this runtime.
    pub fn input_endpoint<T, F>(&self, convert: F) -> Result<RuntimeInputEndpoint<T>, Error>
    where
        F: Fn(T) -> Result<Value, Error> + Send + Sync + 'static,
    {
        let id = self
            .state
            .shared_resources
            .ids
            .input_endpoint()
            .map_err(Error::new)?;
        let endpoint = RuntimeInputEndpointId::from_u64(id.get())
            .expect("runtime input endpoint IDs start at one");
        let _mutation = self.mutation_guard();
        self.state
            .shared_resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .events
            .inputs
            .insert_mut(endpoint, Arc::new(RuntimeInputBuffer::default()));
        let owner = Arc::downgrade(&self.state.shared_resources);
        Ok(RuntimeInputEndpoint {
            sender: RuntimeInputSender {
                runtime: self.id(),
                owner: owner.clone(),
                endpoint,
                convert: Arc::new(convert),
                marker: PhantomData,
            },
            reader: RuntimeInputReader {
                runtime: self.id(),
                owner,
                endpoint,
            },
        })
    }

    /// Registers a buffered output endpoint with separate typed decoding and
    /// external delivery policy.
    pub fn output_endpoint<T, D, A>(
        &self,
        decode: D,
        adapter: A,
    ) -> Result<RuntimeOutputEndpoint<T>, Error>
    where
        D: Fn(Value) -> Result<T, Error> + Send + Sync + 'static,
        A: Fn(T) -> Result<(), Error> + Send + Sync + 'static,
    {
        let id = self
            .state
            .shared_resources
            .ids
            .output_endpoint()
            .map_err(Error::new)?;
        let endpoint = RuntimeOutputEndpointId::from_u64(id.get())
            .expect("runtime output endpoint IDs start at one");
        let _mutation = self.mutation_guard();
        self.state
            .shared_resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .events
            .outputs
            .ready_by_endpoint
            .insert(endpoint, std::collections::VecDeque::new());
        let owner = Arc::downgrade(&self.state.shared_resources);
        Ok(RuntimeOutputEndpoint {
            writer: RuntimeOutputWriter {
                runtime: self.id(),
                owner: owner.clone(),
                endpoint,
            },
            delivery: RuntimeOutputDelivery {
                runtime: self.id(),
                owner,
                endpoint,
                decode: Arc::new(decode),
                adapter: Arc::new(adapter),
            },
        })
    }

    /// Captures every currently retained external delivery failure.
    pub fn delivery_failure_snapshot(&self) -> RuntimeDeliveryFailureSnapshot {
        runtime_delivery_failure_snapshot(&self.state.shared_resources, None)
    }

    /// Acknowledges one retained delivery failure. This changes reporting
    /// state only and therefore does not advance the semantic observation
    /// epoch.
    pub fn acknowledge_delivery_failure(&self, delivery: RuntimeDeliveryId) -> bool {
        let mutation = self.mutation_guard();
        let removed = {
            let mut state = self
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            let removed = state.events.outputs.failures.get(&delivery).cloned();
            if removed.is_some() {
                state.events.outputs.failures.remove_mut(&delivery);
                state
                    .events
                    .outputs
                    .pending_failure_reports
                    .remove_mut(&delivery);
            }
            removed
        };
        drop(mutation);
        let acknowledged = removed.is_some();
        drop(removed);
        acknowledged
    }

    /// Pumps useful lifecycle work across every evaluation session until the
    /// runtime reaches a stable instant.
    ///
    /// This operation does not construct or commit a readiness report. It
    /// waits for work currently owned by a worker or delivery callback,
    /// abandons only unclaimed best-effort sparks, and leaves queued external
    /// output for its host adapter.
    pub fn pump_until_stable(&self) {
        let admission = &self.state.shared_resources.mutation_admission;
        let activity = admission.activity();
        loop {
            if self.state.work.poll_runtime_work() {
                continue;
            }

            if self.state.work.abandon_quiescent_sparks() != 0 {
                // Releasing a spark's lazy claim may make lifecycle work
                // runnable, so always begin another ordinary pump pass.
                continue;
            }

            let observed_activity = activity.current();
            let Some(settlement) = admission.try_settlement_guard() else {
                activity.wait_for_change(observed_activity);
                continue;
            };
            let work = self.state.work.runtime_pump_snapshot();
            let running_delivery = self.state.shared_resources.has_running_delivery();
            drop(settlement);

            if work.useful_ready || work.abandonable_sparks {
                continue;
            }
            if work.progress_owned || running_delivery {
                activity.wait_for_change(observed_activity);
                continue;
            }
            return;
        }
    }

    /// Observes one stable runtime instant without pumping, abandoning, or
    /// terminalizing any work.
    ///
    /// Call [`Self::pump_until_stable`] first when the client wants queued work
    /// and best-effort spark normalization to run before classification.
    pub fn readiness(&self) -> RuntimeReadiness {
        let Some(settlement) = self.try_settlement_guard() else {
            return RuntimeReadiness::Busy;
        };
        let coordinator = self.state.work.runtime_readiness_snapshot();
        if matches!(coordinator, RuntimeCoordinatorReadiness::Busy) {
            drop(settlement);
            return RuntimeReadiness::Busy;
        }

        let reflection = {
            let state = self
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            if !state.events.outputs.records.is_empty() {
                drop(state);
                drop(settlement);
                return RuntimeReadiness::Busy;
            }
            state.reflection.snapshot()
        };
        let observation_epoch = self.state.shared_resources.observations.current().get();
        drop(settlement);

        match coordinator {
            RuntimeCoordinatorReadiness::Busy => unreachable!("handled above"),
            RuntimeCoordinatorReadiness::Ready {
                work_generation,
                exits,
            } => {
                let dispositions = exits
                    .iter()
                    .cloned()
                    .map(runtime_disposition_from_snapshot)
                    .collect();
                RuntimeReadiness::Ready(QuiescenceSnapshot {
                    runtime: self.clone(),
                    stamp: RuntimeReadinessStamp {
                        work_generation,
                        observation_epoch,
                    },
                    dispositions,
                    reflection,
                    settlement: RuntimeSettlementSnapshot::Ready { exits },
                    killed_work: Vec::new(),
                })
            }
            RuntimeCoordinatorReadiness::Deadlocked {
                work_generation,
                exits,
                unfinished,
            } => RuntimeReadiness::Deadlocked(DeadlockSnapshot {
                runtime: self.clone(),
                stamp: RuntimeReadinessStamp {
                    work_generation,
                    observation_epoch,
                },
                dispositions: exits
                    .iter()
                    .cloned()
                    .map(runtime_disposition_from_snapshot)
                    .collect(),
                unfinished: unfinished
                    .iter()
                    .cloned()
                    .map(|work| runtime_deadlock_work_from_snapshot(self.values().core(), work))
                    .collect(),
                reflection,
                settlement_exits: exits,
                settlement_unfinished: unfinished,
            }),
        }
    }

    #[cfg(test)]
    fn validate_quiescence_snapshot(
        &self,
        snapshot: &QuiescenceSnapshot,
    ) -> Result<ValidatedRuntimeSettlementPlan, RuntimeSettlementError> {
        let settlement = self.settlement_guard();
        let validated = self.validate_quiescence_guarded(snapshot);
        drop(settlement);
        validated.ok_or(RuntimeSettlementError::RuntimeChanged)
    }

    fn validate_quiescence_guarded(
        &self,
        snapshot: &QuiescenceSnapshot,
    ) -> Option<ValidatedRuntimeSettlementPlan> {
        if snapshot.runtime.id() != self.id() {
            return None;
        }
        let state_matches = {
            let state = self
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            state.events.outputs.records.is_empty()
        };
        if !state_matches
            || self.state.shared_resources.observations.current().get()
                != snapshot.stamp.observation_epoch
        {
            return None;
        }
        self.state.work.validate_runtime_settlement(
            snapshot.stamp.work_generation,
            snapshot.settlement.exits(),
            snapshot.settlement.kills(),
        )
    }

    fn settle_quiescence_snapshot(
        &self,
        snapshot: &QuiescenceSnapshot,
        previously_validated: Option<&ValidatedRuntimeSettlementPlan>,
    ) -> Result<QuiescenceReport, RuntimeSettlementError> {
        let settlement = self.settlement_guard();
        let Some(current) = self.validate_quiescence_guarded(snapshot) else {
            drop(settlement);
            return Err(RuntimeSettlementError::RuntimeChanged);
        };
        if previously_validated.is_some_and(|validated| validated != &current) {
            drop(settlement);
            return Err(RuntimeSettlementError::RuntimeChanged);
        }
        let kill_failure = snapshot
            .settlement
            .kill_reason()
            .map(|reason| runtime_killed_failure(self.values().core(), reason));
        let Some(release) =
            self.state
                .work
                .publish_runtime_settlement(&settlement, &current, kill_failure)
        else {
            drop(settlement);
            return Err(RuntimeSettlementError::RuntimeChanged);
        };

        let (work_generation, remaining_exits) = match self.state.work.runtime_readiness_snapshot()
        {
            RuntimeCoordinatorReadiness::Ready {
                work_generation,
                exits,
            } => (work_generation, exits),
            RuntimeCoordinatorReadiness::Busy | RuntimeCoordinatorReadiness::Deadlocked { .. } => {
                unreachable!("exclusive exit settlement must leave no unfinished work")
            }
        };
        assert!(
            remaining_exits.is_empty(),
            "settlement must consume every validated exit disposition"
        );
        let (task_ledger, pending_task_reports) = self.state.work.failure_ledgers_for_settlement();
        let (reflection, delivery_failures, pending_delivery_reports) = {
            let mut state = self
                .state
                .shared_resources
                .transactions
                .state
                .lock()
                .expect("runtime transaction mutex should not be poisoned");
            let pending_delivery_reports = std::mem::replace(
                &mut state.events.outputs.pending_failure_reports,
                RedBlackTreeMapSync::new_sync(),
            );
            (
                state.reflection.snapshot(),
                RuntimeDeliveryFailureSnapshot {
                    runtime: self.id(),
                    failures: state.events.outputs.failures.clone(),
                },
                RuntimeDeliveryFailureSnapshot {
                    runtime: self.id(),
                    failures: pending_delivery_reports,
                },
            )
        };
        let observation_epoch = self.state.shared_resources.observations.current().get();
        drop(settlement);
        release.finish();

        let task_failures = task_ledger
            .iter()
            .flat_map(|(session, failures)| {
                failures
                    .iter()
                    .map(move |(task, failure)| ReasoningFailure {
                        runtime: self.id(),
                        task: *task,
                        diagnostic: reasoning_diagnostic(
                            self.values().core(),
                            failure.as_failure(),
                        ),
                        session: *session,
                    })
            })
            .collect();
        let pending_task_failure_reports = pending_task_reports
            .iter()
            .flat_map(|(session, failures)| {
                failures
                    .iter()
                    .map(move |(task, failure)| ReasoningFailure {
                        runtime: self.id(),
                        task: *task,
                        diagnostic: reasoning_diagnostic(
                            self.values().core(),
                            failure.as_failure(),
                        ),
                        session: *session,
                    })
            })
            .collect();
        let pending_exit_error_reports = snapshot
            .dispositions
            .iter()
            .filter(|disposition| {
                matches!(disposition.kind(), RuntimeDispositionKind::ExitError(_))
            })
            .cloned()
            .collect();
        Ok(QuiescenceReport {
            runtime: self.clone(),
            stamp: RuntimeReadinessStamp {
                work_generation,
                observation_epoch,
            },
            dispositions: snapshot.dispositions.clone(),
            task_failures,
            delivery_failures,
            reflection,
            killed_work: snapshot.killed_work.clone(),
            pending_task_failure_reports,
            pending_delivery_failure_reports: pending_delivery_reports,
            pending_exit_error_reports,
            pending_killed_work_reports: snapshot.killed_work.clone(),
        })
    }

    /// Internal activity inspection for the scheduler pump. Retained failures
    /// and buffered input are reporting/state, not active delivery work.
    #[doc(hidden)]
    pub fn has_delivery_activity(&self) -> bool {
        !self
            .state
            .shared_resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .events
            .outputs
            .records
            .is_empty()
    }

    /// Starts this runtime's worker pool exactly once. A runtime constructed
    /// with zero workers remains dormant until this method is called.
    pub fn activate_workers(&self, worker_threads: usize) -> Result<(), Error> {
        self.state
            .executor
            .activate_workers(worker_threads)
            .map_err(|error| Error::new(error.as_ref()))
    }

    pub(crate) fn new_evaluation_session(&self) -> Result<Arc<EvaluationSession>, Error> {
        if !self.has_default_reflection_profile() {
            return Err(Error::new(
                "evaluation runtime default reflection task profile must be sealed before creating a session",
            ));
        }
        Ok(EvaluationSession::shared_with_default_profile(
            &self.state.work,
            self.state.shared_resources.values.core().clone(),
            self.default_reflection_profile.clone(),
        ))
    }

    pub(crate) fn new_evaluation_session_with_profile(
        &self,
        profile: Arc<ReflectionTaskProfile>,
    ) -> Result<Arc<EvaluationSession>, Error> {
        if !profile.is_sealed() {
            return Err(Error::new(
                "evaluation session reflection task profile must be sealed before use",
            ));
        }
        Ok(EvaluationSession::shared_with_default_profile(
            &self.state.work,
            self.state.shared_resources.values.core().clone(),
            profile,
        ))
    }

    pub(super) fn seal_default_reflection_profile(
        &self,
        launcher: Arc<dyn crate::evaluation::ReflectionTaskLauncher>,
    ) -> Result<(), Error> {
        self.default_reflection_profile
            .seal(launcher)
            .map_err(Error::new)
    }

    pub(super) fn has_default_reflection_profile(&self) -> bool {
        self.default_reflection_profile.is_sealed()
    }

    pub(in crate::api) fn mutation_guard(&self) -> RuntimeMutationGuard<'_> {
        self.state
            .shared_resources
            .mutation_admission
            .mutation_guard()
    }

    fn try_settlement_guard(&self) -> Option<RuntimeSettlementGuard<'_>> {
        self.state
            .shared_resources
            .mutation_admission
            .try_settlement_guard()
    }

    fn settlement_guard(&self) -> RuntimeSettlementGuard<'_> {
        self.state
            .shared_resources
            .mutation_admission
            .settlement_guard()
    }

    #[cfg(test)]
    pub(crate) fn exclusive_admission_available(&self) -> bool {
        self.try_settlement_guard().is_some()
    }

    #[cfg(test)]
    pub(in crate::api) fn reflection_snapshot(&self) -> (u64, crate::reflection::StoreSnapshot) {
        self.state.shared_resources.reflection_snapshot()
    }

    /// Captures the reflection store and admitted-input state under the same
    /// transactional-state lock.
    #[cfg(test)]
    pub(in crate::api) fn transaction_snapshot(
        &self,
    ) -> (u64, crate::reflection::StoreSnapshot, RuntimeEventSnapshot) {
        self.state.shared_resources.transaction_snapshot()
    }

    /// Atomically validates and applies one reflection-store journal and its
    /// admitted-input claims. Neither side is applied if either conflicts.
    #[cfg(test)]
    pub(in crate::api) fn try_commit_transaction(
        &self,
        store: &crate::reflection::StoreJournal,
        events: &RuntimeEventJournal,
    ) -> crate::reflection::StoreCommitResult {
        self.state
            .shared_resources
            .try_commit_transaction(store, events)
    }

    #[cfg(test)]
    pub(in crate::api) fn commit_reflection(
        &self,
        journal: &crate::reflection::StoreJournal,
    ) -> crate::reflection::StoreCommitResult {
        self.state.shared_resources.commit_reflection(journal)
    }

    #[cfg(test)]
    pub(in crate::api) fn update_query(
        &self,
        handle: &Arc<crate::reflection::EvaluationQueryHandle>,
        result: Value,
    ) -> Result<(), Error> {
        self.state.shared_resources.update_query(handle, result)
    }

    #[cfg(test)]
    pub(in crate::api) fn wait_for_change(&self, observed_generation: u64) -> bool {
        self.state
            .shared_resources
            .wait_for_change(observed_generation)
    }

    #[cfg(test)]
    pub(in crate::api) fn reflection_root(&self) -> Value {
        self.state.shared_resources.reflection_root()
    }

    pub(super) fn conflict_analysis(&self) -> Arc<dyn ConflictAnalysisStrategy> {
        self.state
            .shared_resources
            .transactions
            .state
            .lock()
            .expect("runtime transaction mutex should not be poisoned")
            .reflection
            .strategy()
    }
}

#[cfg(test)]
pub(crate) fn compiler_test_runtime() -> EvaluationRuntime {
    static RUNTIME: std::sync::LazyLock<EvaluationRuntime> = std::sync::LazyLock::new(|| {
        let core = crate::compiler::test_value_factory();
        let id = core.runtime_id();
        let ids = core.ids().clone();
        let work = core.work_coordinator().unwrap_or_else(|| {
            let candidate = EvaluationWorkCoordinator::new(
                id,
                ids.clone(),
                RuntimeMutationAdmission::new(),
                RuntimeObservationState::new(),
            );
            core.work_coordinator_or_attach(candidate)
        });
        let mutation_admission = work.shared_mutation_admission();
        let observations = work.shared_observations();
        let values = RuntimeValueFactory {
            runtime: id,
            core: core.clone(),
        };
        let executor = EvaluationExecutor::new(0, &work)
            .expect("compiler test executor should be constructible");
        let shared_resources = Arc::new(RuntimeSharedResources {
            id,
            values: values.clone(),
            transactions: RuntimeTransactionState {
                state: Mutex::new(RuntimeTransactionData {
                    reflection: ReflectionStore::new(core, Arc::new(ExactConflictAnalysis)),
                    events: RuntimeEventState::new(),
                }),
            },
            observations,
            ids,
            mutation_admission,
            work: Arc::downgrade(&work),
        });
        EvaluationRuntime {
            state: Arc::new(RuntimeState {
                executor,
                work,
                shared_resources,
                diagnostic_ingresses: Mutex::new(Vec::new()),
            }),
            default_reflection_profile: Arc::new(ReflectionTaskProfile::unsealed()),
        }
    });
    RUNTIME.clone()
}
