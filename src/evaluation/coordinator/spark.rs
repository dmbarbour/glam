//! Best-effort background evaluation demand.

use std::sync::{Arc, Weak};

use crate::core::Value;
use crate::runtime::RuntimeValueRoot;

use super::super::EvaluationDemandState;
use super::deferred::promote_deferred_wait_locked;
use super::{
    ClaimedDemandSession, EvaluationWorkCoordinator, EvaluationWorkId, SettlementObligations,
    WakeRegistration, WorkControl, WorkCoordinatorState, WorkDependency, WorkKind, WorkRecord,
    WorkState, demand_session_is_closed, prune_closed_session_registration,
};

pub(super) struct SparkDemand {
    session: Weak<EvaluationDemandState>,
    value: RuntimeValueRoot,
}

pub(crate) struct SparkWork {
    pub(super) demand: Option<SparkDemand>,
    pub(super) dependency: Option<WorkDependency>,
}

pub(crate) struct ClaimedSparkWork {
    pub(super) id: EvaluationWorkId,
    pub(super) session: ClaimedDemandSession,
    pub(super) demand: SparkDemand,
    pub(super) prior_dependency: Option<WorkDependency>,
}

impl ClaimedSparkWork {
    pub(in crate::evaluation) fn demand(&self) -> &ClaimedDemandSession {
        &self.session
    }

    pub(crate) fn demand_session(&self) -> Arc<EvaluationDemandState> {
        self.session.demand()
    }

    pub(crate) fn value(&self) -> &RuntimeValueRoot {
        &self.demand.value
    }

    pub(crate) fn assert_runtime(&self, runtime: crate::runtime::EvaluationRuntimeId) {
        self.session.assert_runtime(runtime);
        assert_eq!(
            self.value().runtime_id(),
            runtime,
            "spark value must match its claimed demand session runtime"
        );
    }
}

pub(crate) enum SparkWorkPoll {
    Complete,
    Blocked(WorkDependency),
}

pub(super) struct SparkRetirement {
    demand: SparkDemand,
    dependencies: Vec<WorkDependency>,
    _obligations: SettlementObligations,
}

impl SparkRetirement {
    pub(super) fn abandon(self) {
        for dependency in self.dependencies {
            dependency.abandon();
        }
        drop(self.demand);
    }
}

impl EvaluationWorkCoordinator {
    pub(in crate::evaluation) fn submit_spark(
        &self,
        session: Arc<EvaluationDemandState>,
        value: Value,
    ) {
        debug_assert_eq!(session.values.runtime_id(), self.runtime);
        let id = EvaluationWorkId(self.ids.evaluation_work());
        let demand = SparkDemand {
            session: Arc::downgrade(&session),
            value: RuntimeValueRoot::new(&session.values, value),
        };
        let mutation = self.admission.mutation_guard();
        let admitted = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if state.spark_workers == 0 || demand_session_is_closed(&state, session.id) {
                false
            } else {
                let session_id = session.id;
                let record = WorkRecord {
                    id,
                    demand_session: session_id,
                    subscription_epoch: 0,
                    control: WorkControl::default(),
                    obligations: SettlementObligations::default(),
                    state: WorkState::Queued,
                    kind: WorkKind::Spark(SparkWork {
                        demand: Some(demand),
                        dependency: None,
                    }),
                };
                assert!(state.work.insert(id, record).is_none());
                state
                    .work_by_session
                    .entry(session_id)
                    .or_default()
                    .insert(id);
                queue_spark(&mut state, id);
                state.work_generation = state.work_generation.wrapping_add(1);
                true
            }
        };
        drop(mutation);
        if admitted {
            self.work_available.notify_one();
        }
    }

    pub(crate) fn abandon_quiescent_sparks(&self) -> usize {
        let mutation = self.admission.mutation_guard();
        let retired = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let ids = state
                .work
                .iter()
                .filter_map(|(id, record)| {
                    (matches!(record.kind, WorkKind::Spark(_))
                        && matches!(record.state, WorkState::Queued | WorkState::Blocked))
                    .then_some(*id)
                })
                .collect::<Vec<_>>();
            let retired = ids
                .into_iter()
                .filter_map(|id| detach_spark(&mut state, id))
                .collect::<Vec<_>>();
            if !retired.is_empty() {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            retired
        };
        let count = retired.len();
        drop(mutation);
        if count != 0 {
            self.work_available.notify_all();
        }
        for record in retired {
            record.abandon();
        }
        count
    }

    pub(in crate::evaluation) fn release_spark(
        &self,
        claimed: ClaimedSparkWork,
        poll: SparkWorkPoll,
    ) {
        let mutation = self.admission.mutation_guard();
        let (retired, obsolete_dependency, exact_subscription) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get(&claimed.id) else {
                drop(state);
                drop(mutation);
                claimed
                    .prior_dependency
                    .into_iter()
                    .for_each(WorkDependency::abandon);
                drop(claimed.demand);
                return;
            };
            assert_eq!(record.id, claimed.id);
            assert_eq!(record.demand_session, claimed.session.id());
            assert!(matches!(record.state, WorkState::Running));
            let close_requested = record.control.close_reason.is_some();

            let mut obsolete_dependency = None;
            let mut exact_subscription = None;
            let dependency = match poll {
                SparkWorkPoll::Complete => None,
                SparkWorkPoll::Blocked(dependency) => Some(dependency),
            };
            let retired = if close_requested {
                let dependency = match (claimed.prior_dependency, dependency) {
                    (Some(prior), Some(current)) if prior.same_source(&current) => {
                        drop(current);
                        Some(prior)
                    }
                    (Some(prior), current) => {
                        obsolete_dependency = Some(prior);
                        current
                    }
                    (None, current) => current,
                };
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("running spark work must remain registered");
                let spark = spark_work_mut(record);
                spark.demand = Some(claimed.demand);
                spark.dependency = dependency;
                record.state = WorkState::Terminalizing;
                detach_spark(&mut state, claimed.id)
            } else if let Some(dependency) = dependency {
                if dependency.runtime_id() != self.runtime {
                    obsolete_dependency = claimed.prior_dependency;
                    let record = state
                        .work
                        .get_mut(&claimed.id)
                        .expect("running spark work must remain registered");
                    let spark = spark_work_mut(record);
                    spark.demand = Some(claimed.demand);
                    spark.dependency = Some(dependency);
                    record.state = WorkState::Terminalizing;
                    detach_spark(&mut state, claimed.id)
                } else {
                    let dependency = if claimed
                        .prior_dependency
                        .as_ref()
                        .is_some_and(|prior| prior.same_source(&dependency))
                    {
                        drop(dependency);
                        claimed.prior_dependency
                    } else {
                        obsolete_dependency = claimed.prior_dependency;
                        Some(dependency)
                    };
                    let record = state
                        .work
                        .get_mut(&claimed.id)
                        .expect("running spark work must remain registered");
                    let spark = spark_work_mut(record);
                    spark.demand = Some(claimed.demand);
                    spark.dependency = dependency;
                    record.subscription_epoch = record
                        .subscription_epoch
                        .checked_add(1)
                        .expect("evaluation work subscription epochs exhausted");
                    let registration = WakeRegistration {
                        work: claimed.id,
                        subscription_epoch: record.subscription_epoch,
                    };
                    record.state = WorkState::Blocked;
                    exact_subscription = Some((
                        spark_work(record)
                            .dependency
                            .as_ref()
                            .expect("blocked spark work must retain its dependency")
                            .clone(),
                        registration,
                    ));
                    None
                }
            } else {
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("running spark work must remain registered");
                let spark = spark_work_mut(record);
                spark.demand = Some(claimed.demand);
                spark.dependency = claimed.prior_dependency;
                record.state = WorkState::Terminalizing;
                detach_spark(&mut state, claimed.id)
            };
            if state
                .work
                .get(&claimed.id)
                .is_some_and(|record| matches!(record.state, WorkState::Blocked))
                && let Some(wait) = state
                    .work
                    .get(&claimed.id)
                    .and_then(|record| spark_work(record).dependency.as_ref())
                    .and_then(WorkDependency::producer_wait)
                    .cloned()
            {
                promote_deferred_wait_locked(&mut state, &wait);
            }
            state.work_generation = state.work_generation.wrapping_add(1);
            (retired, obsolete_dependency, exact_subscription)
        };

        if let Some((dependency, registration)) = exact_subscription {
            self.subscribe_dependency_guarded(&mutation, dependency, registration);
        }
        drop(mutation);
        self.work_available.notify_all();
        if let Some(dependency) = obsolete_dependency {
            dependency.abandon();
        }
        if let Some(record) = retired {
            record.abandon();
        }
    }
}

pub(super) fn spark_work(record: &WorkRecord) -> &SparkWork {
    match &record.kind {
        WorkKind::Spark(work) => work,
        WorkKind::Reflection(_) | WorkKind::Deferred(_) | WorkKind::ClientDemand(_) => {
            panic!("spark operation addressed non-spark work")
        }
    }
}

pub(super) fn spark_work_mut(record: &mut WorkRecord) -> &mut SparkWork {
    match &mut record.kind {
        WorkKind::Spark(work) => work,
        WorkKind::Reflection(_) | WorkKind::Deferred(_) | WorkKind::ClientDemand(_) => {
            panic!("spark operation addressed non-spark work")
        }
    }
}

pub(super) fn queue_spark(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    if state.ready_spark_set.insert(id) {
        state.ready_sparks.push_back(id);
    }
}

pub(super) fn claim_ready_spark(
    state: &mut WorkCoordinatorState,
    runtime: crate::runtime::EvaluationRuntimeId,
) -> Option<ClaimedSparkWork> {
    while let Some(id) = state.ready_sparks.pop_front() {
        state.ready_spark_set.remove(&id);
        let Some(record) = state.work.get(&id) else {
            continue;
        };
        if !matches!(record.state, WorkState::Queued) {
            continue;
        }
        let demand_session = record.demand_session;
        let session = ClaimedDemandSession::registered(state, demand_session, runtime)?;
        if !spark_work(record)
            .demand
            .as_ref()
            .is_some_and(|demand| Weak::ptr_eq(&demand.session, &Arc::downgrade(&session.demand())))
        {
            return None;
        }
        let record = state
            .work
            .get_mut(&id)
            .expect("claimable spark work must remain registered");
        record.state = WorkState::Running;
        let spark = spark_work_mut(record);
        let demand = spark
            .demand
            .take()
            .expect("queued spark work must retain its demand");
        let prior_dependency = spark.dependency.take();
        return Some(ClaimedSparkWork {
            id,
            session,
            demand,
            prior_dependency,
        });
    }
    None
}

pub(super) fn detach_spark(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
) -> Option<SparkRetirement> {
    let record = state.work.get_mut(&id)?;
    assert!(
        !matches!(record.state, WorkState::Running),
        "worker-owned spark work cannot be detached"
    );
    record.state = WorkState::Terminalizing;
    let record = state
        .work
        .remove(&id)
        .expect("terminalizing spark work must remain registered");
    state.ready_spark_set.remove(&id);
    state.ready_sparks.retain(|candidate| *candidate != id);
    if let Some(session_work) = state.work_by_session.get_mut(&record.demand_session) {
        session_work.remove(&id);
        if session_work.is_empty() {
            state.work_by_session.remove(&record.demand_session);
        }
    }
    prune_closed_session_registration(state, record.demand_session);
    let WorkKind::Spark(mut spark) = record.kind else {
        unreachable!("spark retirement must contain spark work")
    };
    assert!(
        record.obligations.is_empty(),
        "spark work must not acquire producer settlement obligations"
    );
    Some(SparkRetirement {
        demand: spark
            .demand
            .take()
            .expect("non-running spark work must retain its demand"),
        dependencies: spark.dependency.take().into_iter().collect(),
        _obligations: record.obligations,
    })
}
