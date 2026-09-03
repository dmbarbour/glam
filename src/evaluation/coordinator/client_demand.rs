//! Runtime-owned pure client demand lifecycle.

use std::sync::{Arc, Condvar, Mutex, Weak};

#[cfg(test)]
use crate::core::EvaluationFailure;
use crate::runtime::{EvaluationRuntimeId, RuntimeFailureRoot, RuntimeValueRoot};

use super::super::EvaluationDemandState;
use super::deferred::promote_deferred_wait_locked;
use super::{
    ClaimedDemandSession, EvaluationWorkCoordinator, EvaluationWorkId, SettlementObligations,
    WakeRegistration, WorkCloseReason, WorkControl, WorkCoordinatorState, WorkDependency, WorkKind,
    WorkRecord, WorkState, demand_session_is_closed, prune_closed_session_registration,
    queue_current_registration,
};

/// One sealed pure operation retained by runtime-owned client demand.
#[derive(Debug)]
pub(crate) struct ClientDemandOperation(pub(in crate::evaluation) RuntimeValueRoot);

impl ClientDemandOperation {
    pub(crate) fn new(value: RuntimeValueRoot) -> Self {
        Self(value)
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.0.runtime_id()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientDemandResult {
    Complete(RuntimeValueRoot),
    Failed(RuntimeFailureRoot),
    Abandoned,
    Killed(RuntimeFailureRoot),
}

pub(crate) struct ClientDemandResultCell {
    result: Mutex<Option<ClientDemandResult>>,
    changed: Condvar,
    #[cfg(test)]
    publish_probe: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ClientDemandResultCell {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(None),
            changed: Condvar::new(),
            #[cfg(test)]
            publish_probe: Mutex::new(None),
        })
    }

    fn publish(&self, result: ClientDemandResult) -> bool {
        let mut current = self
            .result
            .lock()
            .expect("client demand result cell was poisoned");
        if current.is_some() {
            return false;
        }
        *current = Some(result);
        drop(current);
        #[cfg(test)]
        if let Some(probe) = self
            .publish_probe
            .lock()
            .expect("client demand publish probe was poisoned")
            .take()
        {
            probe();
        }
        self.changed.notify_all();
        true
    }

    #[cfg(test)]
    fn set_publish_probe(&self, probe: impl FnOnce() + Send + 'static) {
        *self
            .publish_probe
            .lock()
            .expect("client demand publish probe was poisoned") = Some(Box::new(probe));
    }

    fn poll(&self) -> Option<ClientDemandResult> {
        self.result
            .lock()
            .expect("client demand result cell was poisoned")
            .clone()
    }

    fn wait(&self) -> ClientDemandResult {
        let mut result = self
            .result
            .lock()
            .expect("client demand result cell was poisoned");
        loop {
            if let Some(result) = result.clone() {
                return result;
            }
            result = self
                .changed
                .wait(result)
                .expect("client demand result cell was poisoned");
        }
    }
}

#[derive(Clone)]
pub(crate) struct ClientDemandSink {
    cell: Arc<ClientDemandResultCell>,
}

impl ClientDemandSink {
    pub(crate) fn pair() -> (Self, Arc<ClientDemandResultCell>) {
        let cell = ClientDemandResultCell::new();
        (Self { cell: cell.clone() }, cell)
    }

    pub(super) fn publish(&self, result: ClientDemandResult) -> bool {
        self.cell.publish(result)
    }
}

/// Rust-side ownership of one asynchronous pure evaluator demand.
pub(crate) struct ClientDemandHandle {
    runtime: EvaluationRuntimeId,
    pub(in crate::evaluation) work: EvaluationWorkId,
    coordinator: Weak<EvaluationWorkCoordinator>,
    cell: Arc<ClientDemandResultCell>,
    active: bool,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "retained async handle controls remain internal until a public runtime-client API is selected"
    )
)]
impl ClientDemandHandle {
    pub(crate) fn new(
        runtime: EvaluationRuntimeId,
        work: EvaluationWorkId,
        coordinator: &Arc<EvaluationWorkCoordinator>,
        cell: Arc<ClientDemandResultCell>,
    ) -> Self {
        Self {
            runtime,
            work,
            coordinator: Arc::downgrade(coordinator),
            cell,
            active: true,
        }
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub(crate) fn poll(&self) -> Option<ClientDemandResult> {
        self.cell.poll()
    }

    pub(in crate::evaluation) fn wait(&self) -> ClientDemandResult {
        self.cell.wait()
    }

    pub(crate) fn abandon(mut self) {
        self.abandon_inner();
    }

    fn abandon_inner(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let retired = self
            .coordinator
            .upgrade()
            .is_some_and(|coordinator| coordinator.abandon_client_demand(self.work));
        if !retired {
            let _ = self.cell.publish(ClientDemandResult::Abandoned);
        }
    }

    pub(in crate::evaluation) fn abandon_if_stably_blocked(
        &mut self,
        subscription_epoch: u64,
    ) -> Option<WorkDependency> {
        if !self.active {
            return None;
        }
        let dependency = self
            .coordinator
            .upgrade()?
            .abandon_blocked_client_demand(self.work, subscription_epoch)?;
        self.active = false;
        Some(dependency)
    }

    #[cfg(test)]
    pub(crate) fn result_cell(&self) -> Weak<ClientDemandResultCell> {
        Arc::downgrade(&self.cell)
    }

    #[cfg(test)]
    pub(crate) fn work(&self) -> EvaluationWorkId {
        self.work
    }

    #[cfg(test)]
    pub(crate) fn set_publish_probe(&self, probe: impl FnOnce() + Send + 'static) {
        self.cell.set_publish_probe(probe);
    }
}

impl Drop for ClientDemandHandle {
    fn drop(&mut self) {
        self.abandon_inner();
    }
}

pub(super) struct ClientDemandSubscription {
    pub(super) dependency: WorkDependency,
    pub(super) registration: WakeRegistration,
}

impl ClientDemandSubscription {
    pub(super) fn unsubscribe(self) {
        let _ = self.dependency.unsubscribe_work(self.registration);
    }
}

pub(crate) struct ClientDemandWork {
    pub(super) demand: Weak<EvaluationDemandState>,
    pub(super) operation: Option<ClientDemandOperation>,
    pub(super) subscription: Option<ClientDemandSubscription>,
}

pub(crate) struct ClaimedClientDemand {
    pub(in crate::evaluation) id: EvaluationWorkId,
    pub(in crate::evaluation) demand: ClaimedDemandSession,
    pub(in crate::evaluation) operation: Option<ClientDemandOperation>,
    pub(super) prior_subscription: Option<ClientDemandSubscription>,
}

pub(crate) enum ClientDemandPoll {
    Complete(RuntimeValueRoot),
    Failed(RuntimeFailureRoot),
    Blocked(WorkDependency),
}

pub(crate) enum ClientDemandSnapshot {
    Queued,
    Running,
    Blocked {
        dependency: WorkDependency,
        subscription_epoch: u64,
    },
}

pub(super) struct ClientDemandRetirement {
    pub(super) sink: ClientDemandSink,
    pub(super) operation: ClientDemandOperation,
    pub(super) subscription: Option<ClientDemandSubscription>,
    pub(super) result: ClientDemandResult,
}

impl ClientDemandRetirement {
    pub(super) fn finish(self) {
        if let Some(subscription) = self.subscription {
            subscription.unsubscribe();
        }
        let _ = self.sink.publish(self.result);
        drop(self.operation);
    }
}

impl EvaluationWorkCoordinator {
    pub(in crate::evaluation) fn admit_client_demand(
        &self,
        demand: Arc<EvaluationDemandState>,
        operation: ClientDemandOperation,
        sink: ClientDemandSink,
    ) -> Result<EvaluationWorkId, Arc<str>> {
        debug_assert_eq!(demand.values.runtime_id(), self.runtime);
        if operation.runtime_id() != self.runtime {
            return Err(Arc::from(
                "client demand operation belongs to another evaluation runtime",
            ));
        }
        let id = EvaluationWorkId(self.ids.evaluation_work());
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if demand_session_is_closed(&state, demand.id) {
                return Err(Arc::from("evaluation demand session is closed"));
            }
            let session = demand.id;
            let record = WorkRecord {
                id,
                demand_session: session,
                subscription_epoch: 0,
                control: WorkControl::default(),
                obligations: SettlementObligations::client_demand(sink),
                state: WorkState::Queued,
                kind: WorkKind::ClientDemand(ClientDemandWork {
                    demand: Arc::downgrade(&demand),
                    operation: Some(operation),
                    subscription: None,
                }),
            };
            assert!(state.work.insert(id, record).is_none());
            state.work_by_session.entry(session).or_default().insert(id);
            queue_client_demand(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_one();
        Ok(id)
    }

    pub(in crate::evaluation) fn requeue_unpolled_client_demand(
        &self,
        mut claimed: ClaimedClientDemand,
    ) {
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get_mut(&claimed.id)
                .expect("unpolled client demand must remain registered");
            assert!(matches!(record.state, WorkState::Running));
            let client = client_demand_work_mut(record);
            assert!(
                client.operation.is_none(),
                "running client demand must have detached its operation"
            );
            client.operation = claimed.operation.take();
            client.subscription = claimed.prior_subscription.take();
            record.state = WorkState::Queued;
            queue_client_demand(&mut state, claimed.id);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_all();
    }

    pub(in crate::evaluation) fn claim_client_demand(
        &self,
        id: EvaluationWorkId,
    ) -> Option<ClaimedClientDemand> {
        let mutation = self.admission.mutation_guard();
        let claimed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let claimed = claim_client_demand(&mut state, self.runtime, id);
            if claimed.is_some() {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            claimed
        };
        drop(mutation);
        if claimed.is_some() {
            self.work_available.notify_all();
        }
        claimed
    }

    pub(in crate::evaluation) fn client_demand_snapshot(
        &self,
        id: EvaluationWorkId,
    ) -> Option<ClientDemandSnapshot> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let record = state.work.get(&id)?;
        let WorkKind::ClientDemand(client) = &record.kind else {
            return None;
        };
        match record.state {
            WorkState::Queued => Some(ClientDemandSnapshot::Queued),
            WorkState::Running => Some(ClientDemandSnapshot::Running),
            WorkState::Blocked => {
                let subscription = client
                    .subscription
                    .as_ref()
                    .expect("blocked client demand must retain its exact subscription");
                Some(ClientDemandSnapshot::Blocked {
                    dependency: subscription.dependency.clone(),
                    subscription_epoch: subscription.registration.subscription_epoch,
                })
            }
            WorkState::Dormant
            | WorkState::Reserved
            | WorkState::ExitWaiting
            | WorkState::Terminalizing => {
                unreachable!("client demand entered an unsupported work state")
            }
        }
    }

    pub(in crate::evaluation) fn release_client_demand(
        &self,
        mut claimed: ClaimedClientDemand,
        poll: ClientDemandPoll,
    ) {
        let mutation = self.admission.mutation_guard();
        let (retirement, obsolete_subscription, exact_subscription) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let close_requested = {
                let record = state
                    .work
                    .get(&claimed.id)
                    .expect("claimed client demand must remain registered");
                assert_eq!(record.demand_session, claimed.demand.id());
                assert!(matches!(record.state, WorkState::Running));
                assert!(matches!(record.kind, WorkKind::ClientDemand(_)));
                record.control.close_reason
            };
            let mut obsolete_subscription = None;
            let mut exact_subscription = None;
            let retirement = if close_requested.is_some() {
                debug_assert!(matches!(
                    close_requested,
                    Some(
                        WorkCloseReason::ClientDemandAbandoned
                            | WorkCloseReason::DemandSessionClosed
                    )
                ));
                Some(detach_client_demand(
                    &mut state,
                    claimed.id,
                    claimed.operation.take(),
                    claimed.prior_subscription.take(),
                    ClientDemandResult::Abandoned,
                ))
            } else {
                match poll {
                    ClientDemandPoll::Complete(value) => {
                        debug_assert_eq!(value.runtime_id(), self.runtime);
                        Some(detach_client_demand(
                            &mut state,
                            claimed.id,
                            claimed.operation.take(),
                            claimed.prior_subscription.take(),
                            ClientDemandResult::Complete(value),
                        ))
                    }
                    ClientDemandPoll::Failed(failure) => Some(detach_client_demand(
                        &mut state,
                        claimed.id,
                        claimed.operation.take(),
                        claimed.prior_subscription.take(),
                        ClientDemandResult::Failed(failure),
                    )),
                    ClientDemandPoll::Blocked(dependency)
                        if dependency.runtime_id() != self.runtime =>
                    {
                        Some(detach_client_demand(
                            &mut state,
                            claimed.id,
                            claimed.operation.take(),
                            claimed.prior_subscription.take(),
                            ClientDemandResult::Failed(RuntimeFailureRoot::from_observer(
                                &self.values,
                                Arc::new(crate::core::EvaluationFailure::message(
                                    "client demand blocked on another evaluation runtime",
                                )),
                            )),
                        ))
                    }
                    ClientDemandPoll::Blocked(dependency) => {
                        obsolete_subscription = claimed.prior_subscription.take();
                        let record = state
                            .work
                            .get_mut(&claimed.id)
                            .expect("blocked client demand must remain registered");
                        record.subscription_epoch = record
                            .subscription_epoch
                            .checked_add(1)
                            .expect("evaluation work subscription epochs exhausted");
                        let registration = WakeRegistration {
                            work: claimed.id,
                            subscription_epoch: record.subscription_epoch,
                        };
                        let client = client_demand_work_mut(record);
                        assert!(client.operation.is_none());
                        client.operation = claimed.operation.take();
                        client.subscription = Some(ClientDemandSubscription {
                            dependency: dependency.clone(),
                            registration,
                        });
                        record.state = WorkState::Blocked;
                        exact_subscription = Some((dependency.clone(), registration));
                        if let Some(wait) = dependency.producer_wait() {
                            promote_deferred_wait_locked(&mut state, wait);
                        }
                        None
                    }
                }
            };
            state.work_generation = state.work_generation.wrapping_add(1);
            (retirement, obsolete_subscription, exact_subscription)
        };
        if let Some(subscription) = obsolete_subscription {
            subscription.unsubscribe();
        }
        let woke = exact_subscription.is_some_and(|(dependency, registration)| {
            self.subscribe_dependency_guarded(&mutation, dependency, registration)
        });
        drop(mutation);
        if let Some(retirement) = retirement {
            retirement.finish();
        }
        self.work_available.notify_all();
        self.notify_dependency_wake(woke);
    }

    pub(super) fn abandon_client_demand(&self, id: EvaluationWorkId) -> bool {
        let mutation = self.admission.mutation_guard();
        let (accepted, retirement) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get_mut(&id) else {
                return false;
            };
            if !matches!(record.kind, WorkKind::ClientDemand(_)) {
                return false;
            }
            if matches!(record.state, WorkState::Terminalizing) {
                return true;
            }
            if matches!(record.state, WorkState::Running) {
                record
                    .control
                    .close_reason
                    .get_or_insert(WorkCloseReason::ClientDemandAbandoned);
                state.work_generation = state.work_generation.wrapping_add(1);
                (true, None)
            } else {
                let retirement =
                    detach_client_demand(&mut state, id, None, None, ClientDemandResult::Abandoned);
                state.work_generation = state.work_generation.wrapping_add(1);
                (true, Some(retirement))
            }
        };
        drop(mutation);
        if let Some(retirement) = retirement {
            retirement.finish();
        }
        self.work_available.notify_all();
        accepted
    }

    pub(super) fn abandon_blocked_client_demand(
        &self,
        id: EvaluationWorkId,
        subscription_epoch: u64,
    ) -> Option<WorkDependency> {
        let mutation = self.admission.mutation_guard();
        let (dependency, registration) = {
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state.work.get(&id)?;
            if !matches!(record.state, WorkState::Blocked) {
                return None;
            }
            let WorkKind::ClientDemand(client) = &record.kind else {
                return None;
            };
            let subscription = client
                .subscription
                .as_ref()
                .expect("blocked client demand must retain its exact subscription");
            if subscription.registration.subscription_epoch != subscription_epoch {
                return None;
            }
            (subscription.dependency.clone(), subscription.registration)
        };

        if dependency.is_terminal() {
            let queued = {
                let mut state = self
                    .state
                    .lock()
                    .expect("evaluation work coordinator was poisoned");
                let queued =
                    queue_current_registration(&mut state, registration, Some(dependency.key()));
                if queued {
                    state.work_generation = state.work_generation.wrapping_add(1);
                }
                queued
            };
            drop(mutation);
            if queued {
                self.work_available.notify_all();
            }
            return None;
        }

        let (dependency, retirement) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state.work.get(&id)?;
            if !matches!(record.state, WorkState::Blocked) {
                return None;
            }
            let WorkKind::ClientDemand(client) = &record.kind else {
                return None;
            };
            let subscription = client
                .subscription
                .as_ref()
                .expect("blocked client demand must retain its exact subscription");
            if subscription.registration.subscription_epoch != subscription_epoch {
                return None;
            }
            let retirement =
                detach_client_demand(&mut state, id, None, None, ClientDemandResult::Abandoned);
            state.work_generation = state.work_generation.wrapping_add(1);
            (dependency, retirement)
        };
        drop(mutation);
        retirement.finish();
        self.work_available.notify_all();
        Some(dependency)
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn kill_client_demand(
        &self,
        id: EvaluationWorkId,
        failure: Arc<EvaluationFailure>,
    ) -> bool {
        let mutation = self.admission.mutation_guard();
        let retirement = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get(&id) else {
                return false;
            };
            if !matches!(record.kind, WorkKind::ClientDemand(_))
                || matches!(record.state, WorkState::Running | WorkState::Terminalizing)
            {
                return false;
            }
            let retirement = detach_client_demand(
                &mut state,
                id,
                None,
                None,
                ClientDemandResult::Killed(RuntimeFailureRoot::from_observer(
                    &self.values,
                    failure,
                )),
            );
            state.work_generation = state.work_generation.wrapping_add(1);
            retirement
        };
        drop(mutation);
        retirement.finish();
        self.work_available.notify_all();
        true
    }
}

pub(super) fn queue_client_demand(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    if state.ready_client_demand_set.insert(id) {
        state.ready_client_demands.push_back(id);
    }
}

pub(super) fn claim_ready_client_demand(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
) -> Option<ClaimedClientDemand> {
    while let Some(id) = state.ready_client_demands.pop_front() {
        state.ready_client_demand_set.remove(&id);
        if let Some(claimed) = claim_client_demand(state, runtime, id) {
            return Some(claimed);
        }
    }
    None
}

fn claim_client_demand(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    id: EvaluationWorkId,
) -> Option<ClaimedClientDemand> {
    let demand_session = state.work.get(&id)?.demand_session;
    let demand = ClaimedDemandSession::registered(state, demand_session, runtime)?;
    let record = state.work.get(&id)?;
    if !matches!(record.state, WorkState::Queued)
        || !matches!(record.kind, WorkKind::ClientDemand(_))
    {
        return None;
    }
    let WorkKind::ClientDemand(client) = &record.kind else {
        unreachable!("validated client demand must preserve its work kind")
    };
    if !Weak::ptr_eq(&client.demand, &Arc::downgrade(&demand.demand())) {
        return None;
    }
    state.ready_client_demand_set.remove(&id);
    state
        .ready_client_demands
        .retain(|candidate| *candidate != id);
    let record = state
        .work
        .get_mut(&id)
        .expect("claimable client demand must remain registered");
    record.state = WorkState::Running;
    let client = client_demand_work_mut(record);
    let operation = client
        .operation
        .take()
        .expect("queued client demand must retain its operation");
    let prior_subscription = client.subscription.take();
    Some(ClaimedClientDemand {
        id,
        demand,
        operation: Some(operation),
        prior_subscription,
    })
}

fn client_demand_work_mut(record: &mut WorkRecord) -> &mut ClientDemandWork {
    match &mut record.kind {
        WorkKind::ClientDemand(work) => work,
        WorkKind::Spark(_) | WorkKind::Reflection(_) | WorkKind::Deferred(_) => {
            panic!("client-demand operation addressed non-client work")
        }
    }
}

pub(super) fn detach_client_demand(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
    claimed_operation: Option<ClientDemandOperation>,
    claimed_subscription: Option<ClientDemandSubscription>,
    result: ClientDemandResult,
) -> ClientDemandRetirement {
    state.ready_client_demand_set.remove(&id);
    state
        .ready_client_demands
        .retain(|candidate| *candidate != id);
    state.observation_waiters.remove(&id);
    let mut record = state
        .work
        .remove(&id)
        .expect("retired client demand must remain registered");
    assert!(
        !matches!(record.state, WorkState::Running) || claimed_operation.is_some(),
        "worker-owned client demand requires its claimed operation at retirement"
    );
    let WorkKind::ClientDemand(mut client) = record.kind else {
        panic!("client-demand retirement must contain client work")
    };
    let operation = match (claimed_operation, client.operation.take()) {
        (Some(operation), None) | (None, Some(operation)) => operation,
        (Some(_), Some(_)) => panic!("client demand operation cannot have two owners"),
        (None, None) => panic!("client demand retirement must retain its operation"),
    };
    let subscription = match (claimed_subscription, client.subscription.take()) {
        (Some(subscription), None) | (None, Some(subscription)) => Some(subscription),
        (None, None) => None,
        (Some(_), Some(_)) => panic!("client demand subscription cannot have two owners"),
    };
    let sink = record
        .obligations
        .take_client_sink()
        .expect("client demand must retain its result sink until retirement");
    assert!(
        record.obligations.is_empty(),
        "client demand retirement must consume every settlement obligation"
    );
    if let Some(session_work) = state.work_by_session.get_mut(&record.demand_session) {
        session_work.remove(&id);
        if session_work.is_empty() {
            state.work_by_session.remove(&record.demand_session);
        }
    }
    prune_closed_session_registration(state, record.demand_session);
    ClientDemandRetirement {
        sink,
        operation,
        subscription,
        result,
    }
}
