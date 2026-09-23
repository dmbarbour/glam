//! Deferred producer claims and pure-lazy cycle handling.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::core::{DeferredValueId, ManagedLazyRoot, ManagedPromiseRoot};

use super::super::{EvaluationDemandState, EvaluationTaskBlock};
#[cfg(test)]
use super::EvaluationSessionId;
use super::task::LazyRouteDemandLease;
use super::{
    ClaimedDemandSession, EvaluationTaskId, EvaluationTaskMachine, EvaluationWaitToken,
    EvaluationWorkCoordinator, EvaluationWorkId, ExactRouteRelease, ExactRouteReleaseTracker,
    SettlementObligations, WorkCloseReason, WorkControl, WorkCoordinatorState, WorkDependency,
    WorkKind, WorkRecord, WorkState, demand_session_is_closed, prune_closed_session_registration,
    publish_task_block_locked, queue_task, remove_ready_task,
};

impl EvaluationWorkCoordinator {
    pub(in crate::evaluation) fn release_lazy_route(
        &self,
        claimed: ClaimedLazyRoute,
        poll: DeferredWorkPoll,
    ) -> DeferredWorkRelease {
        let had_exact_demand = claimed.wait.has_exact_subscriptions();
        let mutation = self.admission.mutation_guard();
        let (mut release, exact_subscription, mut route_tracker) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let mut route_tracker = ExactRouteReleaseTracker::new(&state, claimed.id);
            let demand_while_running = {
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("claimed lazy route must remain registered");
                assert!(matches!(record.state, WorkState::Running));
                let WorkKind::LazyRoute(route) = &mut record.kind else {
                    unreachable!("lazy route claim must match its record")
                };
                assert_eq!(route.wait, claimed.wait);
                std::mem::take(&mut route.demand_while_running)
            };
            let (state_after, block, made_progress, remains_blocked, terminal) = match poll {
                DeferredWorkPoll::Yielded
                    if (claimed.requeue_on_yield && had_exact_demand) || demand_while_running =>
                {
                    (WorkState::Queued, None, true, false, false)
                }
                DeferredWorkPoll::Yielded => (WorkState::Dormant, None, true, false, false),
                DeferredWorkPoll::Blocked(block) => {
                    let unchanged = claimed.prior_block.as_ref() == Some(&block);
                    (WorkState::Blocked, Some(block), !unchanged, true, false)
                }
                DeferredWorkPoll::Terminal => (WorkState::Terminalizing, None, true, false, true),
            };
            let mut exact_subscription = if let Some(block) = block {
                publish_task_block_locked(&mut state, self.runtime, claimed.id, block)
            } else {
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("claimed lazy route must remain registered");
                let WorkKind::LazyRoute(route) = &mut record.kind else {
                    unreachable!()
                };
                route.block = None;
                record.state = state_after;
                state.observation_waiters.remove(&claimed.id);
                None
            };
            if matches!(state_after, WorkState::Queued) {
                queue_deferred(&mut state, claimed.id);
            }
            let (cycle, cycle_error) = if matches!(state_after, WorkState::Blocked) {
                terminalize_lazy_cycle(&mut state, claimed.id)
            } else {
                (Vec::new(), None)
            };
            let cycle_terminal = !cycle.is_empty();
            if cycle_terminal {
                exact_subscription = None;
            }
            state.work_generation = state.work_generation.wrapping_add(1);
            route_tracker.changed(true);
            (
                DeferredWorkRelease {
                    made_progress: made_progress || cycle_terminal,
                    remains_blocked: remains_blocked && !cycle_terminal,
                    terminal: terminal || cycle_terminal,
                    abandoned: false,
                    cycle,
                    cycle_error,
                    machine: None,
                    route: None,
                },
                exact_subscription,
                route_tracker,
            )
        };
        let promoted_wait = exact_subscription
            .as_ref()
            .and_then(|(dependency, _)| dependency.producer_wait());
        let woke = release.remains_blocked
            && exact_subscription.is_some_and(|(dependency, registration)| {
                self.subscribe_dependency_guarded(&mutation, dependency, registration)
            });
        route_tracker.changed(woke);
        if woke {
            release.made_progress = true;
            release.remains_blocked = false;
        }
        if let Some(wait) = promoted_wait {
            let promoted = self.promote_deferred_wait_guarded(&mutation, &wait);
            route_tracker.changed(promoted);
        }
        let observation_woke = release.remains_blocked && self.recheck_observation_wait(claimed.id);
        route_tracker.changed(observation_woke);
        if observation_woke {
            release.made_progress = true;
            release.remains_blocked = false;
        }
        release.route = Some({
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            route_tracker.finish(&state)
        });
        drop(mutation);
        if !release.terminal {
            self.retire_unsubscribed_lazy_route(claimed.id, false);
        }
        self.work_available.notify_all();
        release
    }

    pub(in crate::evaluation) fn retire_lazy_route(&self, id: EvaluationWorkId) {
        let mutation = self.admission.mutation_guard();
        let retired = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let retired = detach_lazy_route(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
            retired
        };
        drop(mutation);
        drop(retired);
        self.work_available.notify_all();
    }

    pub(super) fn release_lazy_route_demand(&self, id: EvaluationWorkId) {
        self.retire_unsubscribed_lazy_route(id, true);
    }

    fn retire_unsubscribed_lazy_route(&self, id: EvaluationWorkId, release_demand: bool) {
        let mutation = self.admission.mutation_guard();
        let retired = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get_mut(&id) else {
                return;
            };
            let WorkKind::LazyRoute(route) = &mut record.kind else {
                return;
            };
            if release_demand {
                route.demand_count = route
                    .demand_count
                    .checked_sub(1)
                    .expect("lazy route demand released twice");
            }
            if route.demand_count != 0
                || matches!(record.state, WorkState::Running | WorkState::Terminalizing)
            {
                return;
            }
            let retired = detach_lazy_route(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
            Some(retired)
        };
        drop(mutation);
        drop(retired);
        self.work_available.notify_all();
    }

    /// Admits one runtime-owned route for a lazy. The background demand is an
    /// execution capability, not the route's owner or a synthetic task.
    pub(in crate::evaluation) fn reserve_lazy_route(
        self: &Arc<Self>,
        lazy: ManagedLazyRoot,
        wait: EvaluationWaitToken,
    ) -> Result<EvaluationWaitToken, Arc<str>> {
        debug_assert_eq!(wait.runtime_id(), self.runtime);
        let background = self
            .background_demand()
            .ok_or_else(|| Arc::from("evaluation runtime has no background demand domain"))?;
        let value = DeferredValueId::from(lazy.id());
        let mutation = self.admission.mutation_guard();
        let (id, canonical_wait, new, leased) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if let Some(id) = state.deferred.by_value.get(&value).copied() {
                let record = state
                    .work
                    .get_mut(&id)
                    .expect("indexed producer must remain registered");
                match &mut record.kind {
                    WorkKind::LazyRoute(route) => {
                        route.demand_count = route
                            .demand_count
                            .checked_add(1)
                            .expect("lazy route demand count exhausted");
                        (id, route.wait.clone(), false, true)
                    }
                    #[cfg(test)]
                    WorkKind::Deferred(work)
                        if matches!(work.producer, DeferredProducer::Lazy(_)) =>
                    {
                        // Test-only custom lazy machines keep their old task
                        // ownership while production lazies use routes.
                        (id, work.wait.clone(), false, false)
                    }
                    _ => panic!("lazy identity must index a lazy producer"),
                }
            } else {
                let id = EvaluationWorkId(self.ids.evaluation_work());
                let record = WorkRecord {
                    id,
                    // Only used to select a matching value-domain execution
                    // capability. Lazy routes are deliberately not inserted
                    // into work_by_session and cannot be closed by an observer.
                    demand_session: background.id,
                    subscription_epoch: 0,
                    control: WorkControl::default(),
                    obligations: SettlementObligations::deferred_claim(
                        wait.clone(),
                        DeferredProducer::Lazy(lazy.clone()),
                    ),
                    state: WorkState::Dormant,
                    kind: WorkKind::LazyRoute(LazyRouteWork {
                        wait: wait.clone(),
                        lazy,
                        block: None,
                        demand_while_running: false,
                        demand_count: 1,
                    }),
                };
                assert!(state.work.insert(id, record).is_none());
                assert!(state.deferred.by_wait.insert(wait.clone(), id).is_none());
                assert!(state.deferred.by_value.insert(value, id).is_none());
                state.work_generation = state.work_generation.wrapping_add(1);
                (id, wait.clone(), true, true)
            }
        };
        drop(mutation);
        if new {
            self.work_available.notify_all();
        }
        Ok(if leased {
            canonical_wait.with_lazy_route_lease(LazyRouteDemandLease::new(self, id))
        } else {
            canonical_wait
        })
    }

    pub(in crate::evaluation) fn reserve_deferred(
        &self,
        session: &EvaluationDemandState,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
        producer: DeferredProducer,
        machine: Box<dyn EvaluationTaskMachine>,
    ) -> Result<DeferredWorkReservation, Arc<str>> {
        debug_assert_eq!(session.values.runtime_id(), self.runtime);
        debug_assert_eq!(wait.runtime_id(), self.runtime);
        let deferred = producer.id();
        let mutation = self.admission.mutation_guard();
        let mut machine = Some(machine);
        let reservation = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if demand_session_is_closed(&state, session.id) {
                return Err(Arc::from("evaluation demand session is closed"));
            }
            if let Some(id) = state.deferred.by_value.get(&deferred).copied() {
                let wait = producer_wait(
                    state
                        .work
                        .get(&id)
                        .expect("indexed deferred work must remain registered"),
                );
                DeferredWorkReservation::Existing(wait)
            } else {
                let id = EvaluationWorkId(self.ids.evaluation_work());
                let record = WorkRecord {
                    id,
                    demand_session: session.id,
                    subscription_epoch: 0,
                    control: WorkControl::default(),
                    obligations: SettlementObligations::deferred_claim(
                        wait.clone(),
                        producer.clone(),
                    ),
                    state: WorkState::Dormant,
                    kind: WorkKind::Deferred(DeferredWork {
                        task,
                        wait: wait.clone(),
                        producer,
                        machine: machine.take(),
                        block: None,
                        demand_while_running: false,
                    }),
                };
                assert!(state.work.insert(id, record).is_none());
                assert!(state.deferred.by_task.insert(task, id).is_none());
                assert!(state.deferred.by_wait.insert(wait, id).is_none());
                assert!(state.deferred.by_value.insert(deferred, id).is_none());
                state
                    .work_by_session
                    .entry(session.id)
                    .or_default()
                    .insert(id);
                state.work_generation = state.work_generation.wrapping_add(1);
                DeferredWorkReservation::New
            }
        };
        drop(mutation);
        // A racing producer may have installed the canonical machine while we
        // were constructing this candidate. Dispose the unused candidate only
        // after releasing coordinator state and mutation admission.
        drop(machine);
        if matches!(reservation, DeferredWorkReservation::New) {
            self.work_available.notify_all();
        }
        Ok(reservation)
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn deferred_work_for_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<EvaluationWorkId> {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .deferred
            .by_wait
            .get(wait)
            .copied()
    }

    pub(in crate::evaluation) fn deferred_wait(
        &self,
        producer: DeferredValueId,
    ) -> Option<EvaluationWaitToken> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let work = state.deferred.by_value.get(&producer)?;
        Some(producer_wait(
            state
                .work
                .get(work)
                .expect("indexed producer work must remain registered"),
        ))
    }

    pub(in crate::evaluation) fn deferred_owner_for_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<super::EvaluationSessionId> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let id = state.deferred.by_wait.get(wait)?;
        let record = state.work.get(id)?;
        matches!(record.kind, WorkKind::Deferred(_)).then_some(record.demand_session)
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn promote_deferred_wait(&self, wait: &EvaluationWaitToken) -> bool {
        let mutation = self.admission.mutation_guard();
        let promoted = self.promote_deferred_wait_guarded(&mutation, wait);
        drop(mutation);
        if promoted {
            self.work_available.notify_all();
        }
        promoted
    }

    pub(super) fn promote_deferred_wait_guarded(
        &self,
        _mutation: &dyn crate::runtime::RuntimeMutationAuthority,
        wait: &EvaluationWaitToken,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let promoted = promote_deferred_wait_locked(&mut state, wait);
        if promoted {
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        promoted
    }
}

impl EvaluationWorkCoordinator {
    pub(in crate::evaluation) fn release_deferred(
        &self,
        mut claimed: ClaimedDeferredWork,
        poll: DeferredWorkPoll,
    ) -> DeferredWorkRelease {
        let had_exact_demand = claimed.wait.has_exact_subscriptions();
        let mut machine = Some(
            claimed
                .machine
                .take()
                .expect("released deferred claim must retain its detached machine"),
        );
        let mutation = self.admission.mutation_guard();
        let (mut release, exact_subscription, mut route_tracker) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let mut route_tracker = ExactRouteReleaseTracker::new(&state, claimed.id);
            let demand_while_running = {
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("claimed deferred work must remain registered");
                assert_eq!(record.demand_session, claimed.demand.id());
                assert!(matches!(record.state, WorkState::Running));
                let deferred = deferred_work_mut(record);
                assert_eq!(deferred.task, claimed.task);
                assert_eq!(deferred.producer.id(), claimed.producer);
                assert!(
                    deferred.machine.is_none(),
                    "running deferred work must have detached its machine"
                );
                deferred.machine = machine.take();
                std::mem::take(&mut deferred.demand_while_running)
            };

            let abandoned = state.work.get(&claimed.id).is_some_and(|record| {
                matches!(
                    record.control.close_reason,
                    Some(WorkCloseReason::DemandSessionClosed)
                )
            });
            let (state_after, block, made_progress, remains_blocked, terminal) = if abandoned {
                (WorkState::Terminalizing, None, true, false, true)
            } else {
                match poll {
                    DeferredWorkPoll::Yielded
                        if (claimed.requeue_on_yield && had_exact_demand)
                            || demand_while_running =>
                    {
                        (WorkState::Queued, None, true, false, false)
                    }
                    DeferredWorkPoll::Yielded => (WorkState::Dormant, None, true, false, false),
                    DeferredWorkPoll::Blocked(block) => {
                        let unchanged = claimed.prior_block.as_ref() == Some(&block);
                        (WorkState::Blocked, Some(block), !unchanged, true, false)
                    }
                    DeferredWorkPoll::Terminal => {
                        (WorkState::Terminalizing, None, true, false, true)
                    }
                }
            };
            let mut exact_subscription = if let Some(block) = block {
                assert!(matches!(state_after, WorkState::Blocked));
                publish_task_block_locked(&mut state, self.runtime, claimed.id, block)
            } else {
                let record = state
                    .work
                    .get_mut(&claimed.id)
                    .expect("claimed deferred work must remain registered");
                deferred_work_mut(record).block = None;
                record.state = state_after;
                state.observation_waiters.remove(&claimed.id);
                None
            };
            if matches!(state_after, WorkState::Queued) {
                queue_deferred(&mut state, claimed.id);
            }

            let (cycle, cycle_error) = if matches!(state_after, WorkState::Blocked) {
                terminalize_lazy_cycle(&mut state, claimed.id)
            } else {
                (Vec::new(), None)
            };
            let cycle_terminal = !cycle.is_empty();
            if cycle_terminal {
                exact_subscription = None;
            }
            let machine = if terminal && !cycle_terminal {
                deferred_work_mut(
                    state
                        .work
                        .get_mut(&claimed.id)
                        .expect("terminal deferred work must remain registered"),
                )
                .machine
                .take()
            } else {
                None
            };
            state.work_generation = state.work_generation.wrapping_add(1);
            route_tracker.changed(true);
            (
                DeferredWorkRelease {
                    made_progress: made_progress || cycle_terminal,
                    remains_blocked: remains_blocked && !cycle_terminal,
                    terminal: terminal || cycle_terminal,
                    abandoned,
                    cycle,
                    cycle_error,
                    machine,
                    route: None,
                },
                exact_subscription,
                route_tracker,
            )
        };
        let promoted_wait = exact_subscription
            .as_ref()
            .and_then(|(dependency, _)| dependency.producer_wait());
        let woke = release.remains_blocked
            && exact_subscription.is_some_and(|(dependency, registration)| {
                self.subscribe_dependency_guarded(&mutation, dependency, registration)
            });
        route_tracker.changed(woke);
        if woke {
            release.made_progress = true;
            release.remains_blocked = false;
        }
        if let Some(wait) = promoted_wait {
            let promoted = self.promote_deferred_wait_guarded(&mutation, &wait);
            route_tracker.changed(promoted);
        }
        let observation_woke = release.remains_blocked && self.recheck_observation_wait(claimed.id);
        route_tracker.changed(observation_woke);
        if observation_woke {
            release.made_progress = true;
            release.remains_blocked = false;
        }
        release.route = Some({
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            route_tracker.finish(&state)
        });
        drop(mutation);
        self.work_available.notify_all();
        release
    }

    pub(in crate::evaluation) fn retire_deferred(&self, id: EvaluationWorkId) {
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get(&id)
                .expect("terminal deferred work must remain registered");
            assert!(matches!(record.state, WorkState::Terminalizing));
            detach_deferred(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_all();
    }

    pub(in crate::evaluation) fn abandon_deferred_wait(
        &self,
        wait: &EvaluationWaitToken,
    ) -> Option<AbandonedDeferredWork> {
        let mutation = self.admission.mutation_guard();
        let abandoned = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let id = state.deferred.by_wait.get(wait).copied()?;
            if state.work.get(&id).is_some_and(|record| {
                matches!(record.state, WorkState::Running | WorkState::Terminalizing)
            }) {
                // A running claim still owns the machine. A terminalizing
                // release already took it and owns settlement/retirement.
                return None;
            }
            let abandoned = begin_deferred_abandonment(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
            abandoned
        };
        drop(mutation);
        self.work_available.notify_all();
        Some(abandoned)
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn deferred_counts(
        &self,
        session: EvaluationSessionId,
    ) -> (usize, usize, usize) {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let active = state
            .deferred
            .by_value
            .values()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count();
        let waits = state
            .deferred
            .by_wait
            .values()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count();
        let tasks = state
            .deferred
            .by_task
            .values()
            .filter(|id| {
                state
                    .work
                    .get(id)
                    .is_some_and(|record| record.demand_session == session)
            })
            .count();
        (active, waits, tasks)
    }

    /// Forces the exact ordering in which an upstream client observes a
    /// blocked chain after its causal tail cooperatively parks.
    #[cfg(test)]
    pub(in crate::evaluation) fn park_deferred_wait_for_test(
        &self,
        wait: &EvaluationWaitToken,
    ) -> bool {
        let mutation = self.admission.mutation_guard();
        let parked = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(id) = state.deferred.by_wait.get(wait).copied() else {
                return false;
            };
            let Some(record) = state.work.get_mut(&id) else {
                return false;
            };
            if !matches!(record.state, WorkState::Queued) {
                return false;
            }
            record.state = WorkState::Dormant;
            remove_ready_deferred(&mut state, id);
            state.work_generation = state.work_generation.wrapping_add(1);
            true
        };
        drop(mutation);
        if parked {
            self.work_available.notify_all();
        }
        parked
    }
}

#[derive(Clone)]
pub(in crate::evaluation) enum DeferredProducer {
    Lazy(ManagedLazyRoot),
    Promise(ManagedPromiseRoot),
}

impl DeferredProducer {
    pub(in crate::evaluation) fn id(&self) -> DeferredValueId {
        match self {
            Self::Lazy(lazy) => lazy.id().into(),
            Self::Promise(promise) => promise.id().into(),
        }
    }
}

pub(super) struct DeferredWork {
    pub(super) task: EvaluationTaskId,
    pub(super) wait: EvaluationWaitToken,
    pub(super) producer: DeferredProducer,
    pub(super) machine: Option<Box<dyn EvaluationTaskMachine>>,
    pub(super) block: Option<EvaluationTaskBlock>,
    /// A dependency was published while this producer's machine was detached.
    /// Release consumes the latch so a cooperative yield cannot lose that
    /// already-authoritative demand.
    pub(super) demand_while_running: bool,
}

/// Scheduler state only. The lazy's managed checkpoint is the sole semantic
/// computation state; this record owns no persistent machine.
pub(super) struct LazyRouteWork {
    pub(super) wait: EvaluationWaitToken,
    pub(super) lazy: ManagedLazyRoot,
    pub(super) block: Option<EvaluationTaskBlock>,
    pub(super) demand_while_running: bool,
    pub(super) demand_count: usize,
}

#[derive(Default)]
pub(super) struct DeferredIndexes {
    pub(super) by_task: BTreeMap<EvaluationTaskId, EvaluationWorkId>,
    pub(super) by_wait: HashMap<EvaluationWaitToken, EvaluationWorkId>,
    pub(super) by_value: HashMap<DeferredValueId, EvaluationWorkId>,
}

pub(in crate::evaluation) struct ClaimedDeferredWork {
    pub(super) id: EvaluationWorkId,
    pub(super) task: EvaluationTaskId,
    pub(super) wait: EvaluationWaitToken,
    pub(super) demand: ClaimedDemandSession,
    pub(super) producer: DeferredValueId,
    pub(super) prior_block: Option<EvaluationTaskBlock>,
    pub(super) requeue_on_yield: bool,
    pub(super) machine: Option<Box<dyn EvaluationTaskMachine>>,
}

pub(in crate::evaluation) struct ClaimedLazyRoute {
    pub(super) id: EvaluationWorkId,
    pub(super) wait: EvaluationWaitToken,
    pub(super) lazy: ManagedLazyRoot,
    pub(super) demand: ClaimedDemandSession,
    pub(super) prior_block: Option<EvaluationTaskBlock>,
    pub(super) requeue_on_yield: bool,
}

impl ClaimedLazyRoute {
    pub(in crate::evaluation) fn id(&self) -> EvaluationWorkId {
        self.id
    }

    pub(in crate::evaluation) fn demand(&self) -> &ClaimedDemandSession {
        &self.demand
    }

    pub(in crate::evaluation) fn poll(
        &mut self,
        context: &super::super::EvaluationPollContext,
        step_budget: &mut super::super::EvaluationStepBudget,
    ) -> super::EvaluationMachinePoll {
        crate::eval::poll_lazy_route(context, self.demand.demand(), &self.lazy, step_budget)
    }
}

impl ClaimedDeferredWork {
    pub(in crate::evaluation) fn demand(&self) -> &ClaimedDemandSession {
        &self.demand
    }

    pub(in crate::evaluation) fn id(&self) -> EvaluationWorkId {
        self.id
    }

    pub(in crate::evaluation) fn poll(
        &mut self,
        context: &super::super::EvaluationPollContext,
        step_budget: &mut super::super::EvaluationStepBudget,
    ) -> super::EvaluationMachinePoll {
        self.machine
            .as_mut()
            .expect("claimed deferred work must retain its detached machine")
            .poll(context, step_budget)
    }
}

pub(in crate::evaluation) enum DeferredWorkPoll {
    Yielded,
    Blocked(EvaluationTaskBlock),
    Terminal,
}

pub(in crate::evaluation) struct DeferredLazyCycleMember {
    pub(in crate::evaluation) work: EvaluationWorkId,
    pub(in crate::evaluation) wait: EvaluationWaitToken,
    pub(in crate::evaluation) lazy: ManagedLazyRoot,
    pub(in crate::evaluation) machine: Option<Box<dyn EvaluationTaskMachine>>,
    pub(in crate::evaluation) route: bool,
    pub(in crate::evaluation) retired_block: Option<EvaluationTaskBlock>,
}

pub(in crate::evaluation) struct DeferredWorkRelease {
    pub(in crate::evaluation) made_progress: bool,
    pub(in crate::evaluation) remains_blocked: bool,
    pub(in crate::evaluation) terminal: bool,
    pub(in crate::evaluation) abandoned: bool,
    pub(in crate::evaluation) cycle: Vec<DeferredLazyCycleMember>,
    pub(in crate::evaluation) cycle_error: Option<String>,
    pub(in crate::evaluation) machine: Option<Box<dyn EvaluationTaskMachine>>,
    pub(in crate::evaluation) route: Option<ExactRouteRelease>,
}

pub(in crate::evaluation) enum DeferredWorkReservation {
    New,
    Existing(EvaluationWaitToken),
}

pub(in crate::evaluation) struct AbandonedDeferredWork {
    pub(in crate::evaluation) id: EvaluationWorkId,
    pub(in crate::evaluation) task: EvaluationTaskId,
    pub(in crate::evaluation) dependency: Option<EvaluationWaitToken>,
    pub(in crate::evaluation) machine: Box<dyn EvaluationTaskMachine>,
}

pub(super) fn deferred_work_mut(record: &mut WorkRecord) -> &mut DeferredWork {
    match &mut record.kind {
        WorkKind::Deferred(work) => work,
        WorkKind::LazyRoute(_) => panic!("lazy route is not a task-owned deferred producer"),
        WorkKind::Spark(_) => panic!("spark work cannot be used as a deferred producer"),
        WorkKind::Reflection(_) => {
            panic!("reflection work cannot be used as a deferred producer")
        }
    }
}

pub(super) fn producer_wait(record: &WorkRecord) -> EvaluationWaitToken {
    match &record.kind {
        WorkKind::Deferred(work) => work.wait.clone(),
        WorkKind::LazyRoute(work) => work.wait.clone(),
        WorkKind::Spark(_) | WorkKind::Reflection(_) => {
            panic!("indexed deferred producer must remain a deferred producer")
        }
    }
}

pub(super) fn queue_deferred(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    assert!(matches!(
        state
            .work
            .get(&id)
            .expect("queued deferred work must remain registered")
            .kind,
        WorkKind::Deferred(_) | WorkKind::LazyRoute(_)
    ));
    queue_task(state, id);
}

pub(super) fn detach_lazy_route(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
) -> WorkRecord {
    state.observation_waiters.remove(&id);
    remove_ready_deferred(state, id);
    let record = state
        .work
        .remove(&id)
        .expect("retired lazy route must remain registered");
    let WorkKind::LazyRoute(route) = &record.kind else {
        panic!("lazy route retirement must contain lazy route work")
    };
    assert_eq!(state.deferred.by_wait.remove(&route.wait), Some(id));
    assert_eq!(
        state.deferred.by_value.remove(&route.lazy.id().into()),
        Some(id)
    );
    record
}

pub(super) fn remove_ready_deferred(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    remove_ready_task(state, id);
}

pub(super) fn claim_deferred(
    state: &mut WorkCoordinatorState,
    runtime: crate::runtime::EvaluationRuntimeId,
    id: EvaluationWorkId,
    requeue_on_yield: bool,
) -> Option<ClaimedDeferredWork> {
    let demand_session = state.work.get(&id)?.demand_session;
    let demand = ClaimedDemandSession::registered(state, demand_session, runtime)?;
    let (task, wait, producer, prior_block, machine, requeue_on_yield) = {
        let record = state.work.get_mut(&id)?;
        if !matches!(record.kind, WorkKind::Deferred(_))
            || !matches!(record.state, WorkState::Dormant | WorkState::Queued)
        {
            return None;
        }
        let was_queued = matches!(record.state, WorkState::Queued);
        record.state = WorkState::Running;
        let deferred = deferred_work_mut(record);
        let demand_while_running = std::mem::take(&mut deferred.demand_while_running);
        (
            deferred.task,
            deferred.wait.clone(),
            deferred.producer.id(),
            deferred.block.take(),
            deferred
                .machine
                .take()
                .expect("claimable deferred work must retain its machine"),
            requeue_on_yield || was_queued || demand_while_running,
        )
    };
    state.observation_waiters.remove(&id);
    remove_ready_deferred(state, id);
    Some(ClaimedDeferredWork {
        id,
        task,
        wait,
        demand,
        producer,
        prior_block,
        requeue_on_yield,
        machine: Some(machine),
    })
}

pub(super) fn claim_lazy_route(
    state: &mut WorkCoordinatorState,
    runtime: crate::runtime::EvaluationRuntimeId,
    id: EvaluationWorkId,
    requeue_on_yield: bool,
) -> Option<ClaimedLazyRoute> {
    let execution_session = state.work.get(&id)?.demand_session;
    let demand = ClaimedDemandSession::registered(state, execution_session, runtime)?;
    let (wait, lazy, prior_block, requeue_on_yield) = {
        let record = state.work.get_mut(&id)?;
        let WorkKind::LazyRoute(route) = &mut record.kind else {
            return None;
        };
        if !matches!(record.state, WorkState::Dormant | WorkState::Queued) {
            return None;
        }
        let was_queued = matches!(record.state, WorkState::Queued);
        record.state = WorkState::Running;
        (
            route.wait.clone(),
            route.lazy.clone(),
            route.block.take(),
            requeue_on_yield || was_queued || std::mem::take(&mut route.demand_while_running),
        )
    };
    state.observation_waiters.remove(&id);
    remove_ready_deferred(state, id);
    Some(ClaimedLazyRoute {
        id,
        wait,
        lazy,
        demand,
        prior_block,
        requeue_on_yield,
    })
}

pub(super) fn promote_deferred_wait_locked(
    state: &mut WorkCoordinatorState,
    wait: &EvaluationWaitToken,
) -> bool {
    let Some(id) = state.deferred.by_wait.get(wait).copied() else {
        return false;
    };
    match state.work.get(&id).map(|record| record.state) {
        Some(WorkState::Dormant) => {
            state
                .work
                .get_mut(&id)
                .expect("dormant deferred work must remain registered")
                .state = WorkState::Queued;
            queue_deferred(state, id);
            true
        }
        Some(WorkState::Running) => {
            match &mut state
                .work
                .get_mut(&id)
                .expect("running deferred work must remain registered")
                .kind
            {
                WorkKind::Deferred(work) => work.demand_while_running = true,
                WorkKind::LazyRoute(work) => work.demand_while_running = true,
                WorkKind::Spark(_) | WorkKind::Reflection(_) => unreachable!(),
            }
            true
        }
        _ => false,
    }
}

fn deferred_dependency_cycle(
    state: &WorkCoordinatorState,
    start: EvaluationWorkId,
) -> Option<DeferredDependencyCycle> {
    let mut path: Vec<EvaluationWorkId> = Vec::new();
    let mut positions = HashMap::new();
    let mut current = start;
    loop {
        if let Some(first) = positions.insert(current, path.len()) {
            let mut cycle = path.split_off(first);
            let canonical = cycle
                .iter()
                .enumerate()
                .min_by_key(|(_, work)| work.get())
                .map(|(position, _)| position)
                .expect("a repeated successor must produce a non-empty cycle");
            cycle.rotate_left(canonical);
            return Some(DeferredDependencyCycle { members: cycle });
        }
        path.push(current);
        let record = state.work.get(&current)?;
        let dependency = match &record.kind {
            WorkKind::Deferred(work) => work.block.as_ref()?.dependency.as_ref()?,
            WorkKind::LazyRoute(work) => work.block.as_ref()?.dependency.as_ref()?,
            WorkKind::Reflection(work) => work.block.as_ref()?.dependency.as_ref()?,
            _ => return None,
        };
        let wait = dependency.producer_wait()?;
        current = super::work_for_wait_locked(state, &wait)?;
    }
}

struct DeferredDependencyCycle {
    members: Vec<EvaluationWorkId>,
}

pub(super) fn terminalize_lazy_cycle(
    state: &mut WorkCoordinatorState,
    start: EvaluationWorkId,
) -> (Vec<DeferredLazyCycleMember>, Option<String>) {
    let Some(cycle) = deferred_dependency_cycle(state, start) else {
        return (Vec::new(), None);
    };
    let all_blocked = cycle.members.iter().all(|id| {
        state.work.get(id).is_some_and(|record| {
            matches!(record.state, WorkState::Blocked)
                && matches!(
                    &record.kind,
                    WorkKind::LazyRoute(_)
                        | WorkKind::Deferred(DeferredWork {
                            producer: DeferredProducer::Lazy(_),
                            ..
                        })
                        | WorkKind::Reflection(_)
                )
        })
    });
    if !all_blocked {
        return (Vec::new(), None);
    }

    // Reflection tasks remain blocked; poisoning the lazy members wakes them
    // through their exact subscriptions. Only the lazy cells are terminalized
    // here, so no reflection machine or producer obligation is stolen.
    let cycle_error = cycle.members.iter().find_map(|id| {
        let record = state.work.get(id)?;
        let dependency = match &record.kind {
            WorkKind::LazyRoute(work) => work.block.as_ref()?.dependency.as_ref()?,
            WorkKind::Deferred(work) => work.block.as_ref()?.dependency.as_ref()?,
            WorkKind::Reflection(work) => work.block.as_ref()?.dependency.as_ref()?,
            WorkKind::Spark(_) => return None,
        };
        let WorkDependency::Promise(promise) = dependency else {
            return None;
        };
        let producer = promise.producer()?;
        Some(format!(
            "reflection promise {} recursively observed itself in task {}",
            promise.id().get(),
            producer.owner().get()
        ))
    });
    if cycle_error.is_none()
        && cycle.members.iter().any(|id| {
            state
                .work
                .get(id)
                .is_some_and(|record| matches!(record.kind, WorkKind::Reflection(_)))
        })
    {
        // A reflection task can still be cancelled or otherwise disturbed.
        // Only a promise owned by a member of this exact cycle makes its
        // recursive observation an evaluation error.
        return (Vec::new(), None);
    }

    let lazy_ids = cycle
        .members
        .into_iter()
        .filter(|id| {
            state.work.get(id).is_some_and(|record| {
                matches!(
                    &record.kind,
                    WorkKind::LazyRoute(_)
                        | WorkKind::Deferred(DeferredWork {
                            producer: DeferredProducer::Lazy(_),
                            ..
                        })
                )
            })
        })
        .collect::<Vec<_>>();
    if lazy_ids.is_empty() {
        return (Vec::new(), None);
    }

    let mut members = Vec::with_capacity(lazy_ids.len());
    for id in lazy_ids {
        let record = state
            .work
            .get_mut(&id)
            .expect("cycle member must remain registered");
        let member = match &mut record.kind {
            WorkKind::LazyRoute(route) => DeferredLazyCycleMember {
                work: id,
                wait: route.wait.clone(),
                lazy: route.lazy.clone(),
                machine: None,
                route: true,
                retired_block: route.block.take(),
            },
            WorkKind::Deferred(deferred) => {
                let DeferredProducer::Lazy(lazy) = &deferred.producer else {
                    unreachable!("pure lazy cycle cannot contain a promise")
                };
                DeferredLazyCycleMember {
                    work: id,
                    wait: deferred.wait.clone(),
                    lazy: lazy.clone(),
                    machine: Some(
                        deferred
                            .machine
                            .take()
                            .expect("blocked legacy lazy cycle member must retain its machine"),
                    ),
                    route: false,
                    retired_block: deferred.block.take(),
                }
            }
            _ => unreachable!("pure lazy cycle contains other work"),
        };
        record.state = WorkState::Terminalizing;
        state.observation_waiters.remove(&id);
        remove_ready_deferred(state, id);
        members.push(member);
    }
    (members, cycle_error)
}

pub(super) fn begin_deferred_abandonment(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
) -> AbandonedDeferredWork {
    let record = state
        .work
        .get_mut(&id)
        .expect("abandoned deferred work must remain registered");
    assert!(matches!(record.kind, WorkKind::Deferred(_)));
    assert!(!matches!(record.state, WorkState::Running));
    let deferred = deferred_work_mut(record);
    let abandoned = AbandonedDeferredWork {
        id,
        task: deferred.task,
        dependency: deferred
            .block
            .take()
            .and_then(|block| block.dependency)
            .and_then(WorkDependency::into_wait),
        machine: deferred
            .machine
            .take()
            .expect("abandoned deferred work must retain its machine"),
    };
    record.state = WorkState::Terminalizing;
    state.observation_waiters.remove(&id);
    remove_ready_deferred(state, id);
    abandoned
}

pub(super) fn detach_deferred(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    state.observation_waiters.remove(&id);
    remove_ready_deferred(state, id);
    let record = state
        .work
        .remove(&id)
        .expect("retired deferred work must remain registered");
    assert!(
        record.obligations.is_empty(),
        "deferred work cannot retire before terminal settlement"
    );
    let WorkKind::Deferred(deferred) = record.kind else {
        panic!("deferred retirement must contain deferred work")
    };
    assert!(
        deferred.machine.is_none(),
        "deferred work cannot retire before detaching its machine"
    );
    assert_eq!(state.deferred.by_task.remove(&deferred.task), Some(id));
    assert_eq!(state.deferred.by_wait.remove(&deferred.wait), Some(id));
    assert_eq!(
        state.deferred.by_value.remove(&deferred.producer.id()),
        Some(id)
    );
    if let Some(session_work) = state.work_by_session.get_mut(&record.demand_session) {
        session_work.remove(&id);
        if session_work.is_empty() {
            state.work_by_session.remove(&record.demand_session);
        }
    }
    prune_closed_session_registration(state, record.demand_session);
}
