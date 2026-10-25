//! Reflection-task payloads, indexes, and lifecycle-local transitions.

use std::collections::BTreeMap;

use crate::trusted_hash::TrustedHashMap;
use std::sync::Arc;

use crate::core::{EvaluationPanic, RuntimeValueMutator};
use crate::runtime::{EvaluationRuntimeId, RuntimeMutationAuthority};

use super::super::{EvaluationDemandState, EvaluationTaskBlock};
use super::deferred::terminalize_lazy_cycle;
use super::{
    ClaimedDemandSession, CoordinatorMutationKind, EvaluationSessionId, EvaluationTaskId,
    EvaluationTaskMachine, EvaluationTaskStatus, EvaluationWaitToken, EvaluationWorkCoordinator,
    EvaluationWorkId, ExactRouteRelease, ExactRouteReleaseTracker, ProducerSettlementObligation,
    RouteHazard, SettlementObligations, TaskStatusPublisher, TaskStatusUpdate, TaskStatusWake,
    WorkCloseReason, WorkControl, WorkCoordinatorState, WorkKind, WorkRecord, WorkState,
    demand_session_is_closed, prune_closed_session_registration, publish_task_block_locked,
    queue_task, register_background_root, remove_ready_task, unregister_background_root,
};

impl EvaluationWorkCoordinator {
    #[cfg(test)]
    pub(in crate::evaluation) fn task_has_status_publisher(&self, task: EvaluationTaskId) -> bool {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(work) = state.reflection.by_task.get(&task) else {
            return false;
        };
        state.work.get(work).is_some_and(|record| {
            record
                .obligations
                .producer
                .as_ref()
                .and_then(|producer| match producer {
                    ProducerSettlementObligation::ReflectionTask(publisher) => {
                        publisher.protected_status.as_ref()
                    }
                    ProducerSettlementObligation::DeferredClaim { .. } => None,
                })
                .is_some()
        })
    }

    /// Makes the failure of the reflection work at `work`, a task with no
    /// machine of its own, flag its anonymous root, which settlement reports
    /// (`reflection-tasks-are-machine-tasks`). Annotation tasks set it as
    /// they activate; a child task's failure flags its machine instead, and a
    /// Rust-scheduled root's goes to its caller.
    pub(in crate::evaluation) fn flag_root_on_failure(&self, work: EvaluationWorkId) {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        if let Some(record) = state.work.get_mut(&work) {
            reflection_work_mut(record).flags_root = true;
        }
    }

    /// Attaches a test's status observer, which records each status the
    /// task publishes.
    #[cfg(test)]
    pub(in crate::evaluation) fn attach_reflection_status_publisher(
        &self,
        work: EvaluationWorkId,
        publisher: TaskStatusPublisher,
    ) -> bool {
        let _mutation = self.admission.mutation_guard();
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(record) = state.work.get_mut(&work) else {
            return false;
        };
        record
            .obligations
            .task_publisher_mut()
            .expect("status publishers belong only to reflection tasks")
            .attach_status(publisher);
        true
    }

    /// Attaches one host-owned lifecycle publisher, beside a test's status
    /// observer.
    pub(in crate::evaluation) fn attach_reflection_lifecycle_publisher(
        &self,
        work: EvaluationWorkId,
        publisher: TaskStatusPublisher,
    ) -> bool {
        let _mutation = self.admission.mutation_guard();
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let Some(record) = state.work.get_mut(&work) else {
            return false;
        };
        record
            .obligations
            .task_publisher_mut()
            .expect("lifecycle publishers belong only to reflection tasks")
            .attach_lifecycle(publisher);
        true
    }

    pub(in crate::evaluation) fn reserve_reflection(
        &self,
        session: &EvaluationDemandState,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
    ) -> Result<EvaluationWorkId, Arc<str>> {
        self.insert_reflection(session, task, wait, WorkState::Reserved)
    }

    pub(in crate::evaluation) fn register_dormant_reflection(
        &self,
        session: &EvaluationDemandState,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
    ) -> Result<EvaluationWorkId, Arc<str>> {
        self.insert_reflection(session, task, wait, WorkState::Dormant)
    }

    fn insert_reflection(
        &self,
        session: &EvaluationDemandState,
        task: EvaluationTaskId,
        wait: EvaluationWaitToken,
        initial: WorkState,
    ) -> Result<EvaluationWorkId, Arc<str>> {
        debug_assert_eq!(session.values.runtime_id(), self.runtime);
        debug_assert!(matches!(initial, WorkState::Dormant | WorkState::Reserved));
        let id = EvaluationWorkId(self.ids.evaluation_work());
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if demand_session_is_closed(&state, session.id) {
                return Err(Arc::from("evaluation demand session is closed"));
            }
            let record = WorkRecord {
                id,
                demand_session: session.id,
                subscription_epoch: 0,
                control: WorkControl::default(),
                obligations: SettlementObligations::reflection_task(wait.clone()),
                state: initial,
                kind: WorkKind::Reflection(ReflectionWork {
                    task,
                    launch_parent: None,
                    flags_root: false,
                    wait: wait.clone(),
                    machine: None,
                    block: None,
                }),
            };
            assert!(state.work.insert(id, record).is_none());
            assert!(state.reflection.by_task.insert(task, id).is_none());
            assert!(state.reflection.by_wait.insert(wait, id).is_none());
            state
                .work_by_session
                .entry(session.id)
                .or_default()
                .insert(id);
            state.advance_work_generation(
                CoordinatorMutationKind::FreshWorkAdmission,
                RouteHazard::None,
            );
        }
        drop(mutation);
        self.notify_all(CoordinatorMutationKind::FreshWorkAdmission);
        Ok(id)
    }

    /// Installs a fully constructed machine into its reserved reflection work
    /// record. A concurrent cancellation or owner close may already have
    /// terminalized the reservation; in that case the unused machine is
    /// returned for destruction after runtime locks are released.
    pub(in crate::evaluation) fn install_reflection_machine(
        &self,
        id: EvaluationWorkId,
        machine: Box<dyn EvaluationTaskMachine>,
    ) -> Result<(), Box<dyn EvaluationTaskMachine>> {
        let mutation = self.admission.mutation_guard();
        let mut machine = Some(machine);
        let installed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if let Some(record) = state
                .work
                .get_mut(&id)
                .filter(|record| matches!(record.state, WorkState::Reserved))
            {
                let reflection = reflection_work_mut(record);
                assert!(
                    reflection.machine.is_none(),
                    "reserved reflection work cannot own two machines"
                );
                reflection.machine = machine.take();
                true
            } else {
                false
            }
        };
        drop(mutation);
        if installed {
            Ok(())
        } else {
            Err(machine.expect("uninstalled reflection machine must remain owned"))
        }
    }

    pub(in crate::evaluation) fn activate_reflection(&self, id: EvaluationWorkId) -> bool {
        self.activate_reflection_with_parent(id, None)
    }

    /// Publishes an effect-launched child and its causal parent together.
    /// The parent is provenance for work discovery, not a completion wait.
    pub(in crate::evaluation) fn activate_reflection_with_parent(
        &self,
        id: EvaluationWorkId,
        parent: Option<EvaluationTaskId>,
    ) -> bool {
        let mutation = self.admission.mutation_guard();
        let activated = self.activate_reflection_guarded_with_parent(id, parent, &mutation);
        drop(mutation);
        self.notify_reflection_activation(activated);
        activated
    }

    fn activate_reflection_guarded_with_parent(
        &self,
        id: EvaluationWorkId,
        parent: Option<EvaluationTaskId>,
        _mutation: &dyn RuntimeMutationAuthority,
    ) -> bool {
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get_mut(&id) else {
                return false;
            };
            if !matches!(record.kind, WorkKind::Reflection(_))
                || !matches!(record.state, WorkState::Reserved)
                || reflection_work(record).machine.is_none()
            {
                return false;
            }
            reflection_work_mut(record).launch_parent = parent;
            record.state = WorkState::Queued;
            if let Some(parent) = parent {
                state
                    .reflection
                    .children_by_parent
                    .entry(parent)
                    .or_default()
                    .push(id);
                state.reflection.helping_parent_by_child.insert(id, parent);
            }
            register_background_root(&mut state, id);
            queue_reflection(&mut state, id);
            state.advance_work_generation(
                CoordinatorMutationKind::WorkActivation,
                RouteHazard::None,
            );
            true
        }
    }

    pub(in crate::evaluation) fn notify_reflection_activation(&self, activated: bool) {
        if activated {
            self.notify_all(CoordinatorMutationKind::WorkActivation);
        }
    }

    pub(in crate::evaluation) fn terminalize_reserved_reflection(
        &self,
        id: EvaluationWorkId,
    ) -> bool {
        self.begin_reflection_terminalization(id, Some(WorkState::Reserved))
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn terminalize_reflection(&self, id: EvaluationWorkId) -> bool {
        self.begin_reflection_terminalization(id, None)
    }

    pub(in crate::evaluation) fn discard_reserved_reflection(&self, id: EvaluationWorkId) -> bool {
        let mutation = self.admission.mutation_guard();
        let discarded = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let reserved = state.work.get(&id).is_some_and(|record| {
                matches!(record.kind, WorkKind::Reflection(_))
                    && matches!(record.state, WorkState::Reserved)
            });
            if !reserved {
                false
            } else {
                let (machine, block) = detach_reflection(&mut state, id, false);
                assert!(
                    machine.is_none() && block.is_none(),
                    "an uncommitted reflection reservation cannot own a machine or a block"
                );
                state.advance_work_generation(
                    CoordinatorMutationKind::WorkRetirement,
                    RouteHazard::All,
                );
                true
            }
        };
        drop(mutation);
        if discarded {
            self.notify_all(CoordinatorMutationKind::WorkRetirement);
        }
        discarded
    }

    fn begin_reflection_terminalization(
        &self,
        id: EvaluationWorkId,
        required: Option<WorkState>,
    ) -> bool {
        let mutation = self.admission.mutation_guard();
        let terminalizing = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get_mut(&id) else {
                return false;
            };
            if !matches!(record.kind, WorkKind::Reflection(_))
                || required
                    .as_ref()
                    .is_some_and(|required| record.state != *required)
            {
                return false;
            }
            assert!(
                !matches!(record.state, WorkState::Running),
                "running reflection work requires a release transition"
            );
            record.state = WorkState::Terminalizing;
            state.observation_waiters.remove(&id);
            remove_ready_reflection(&mut state, id);
            state.advance_work_generation(
                CoordinatorMutationKind::TerminalSettlement,
                RouteHazard::All,
            );
            true
        };
        drop(mutation);
        if terminalizing {
            self.notify_all(CoordinatorMutationKind::TerminalSettlement);
        }
        terminalizing
    }

    pub(in crate::evaluation) fn request_reflection_cancellation(
        &self,
        id: EvaluationWorkId,
    ) -> ReflectionCancellation {
        let mutation = self.admission.mutation_guard();
        let outcome = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let Some(record) = state.work.get_mut(&id) else {
                return ReflectionCancellation::Late;
            };
            if !matches!(record.kind, WorkKind::Reflection(_)) {
                return ReflectionCancellation::Late;
            }
            let outcome = match record.state {
                WorkState::Running => {
                    record
                        .control
                        .close_reason
                        .get_or_insert(WorkCloseReason::ExplicitCancellation);
                    ReflectionCancellation::Requested
                }
                WorkState::Dormant
                | WorkState::Reserved
                | WorkState::Queued
                | WorkState::Blocked => {
                    record
                        .control
                        .close_reason
                        .get_or_insert(WorkCloseReason::ExplicitCancellation);
                    record.state = WorkState::Terminalizing;
                    state.observation_waiters.remove(&id);
                    remove_ready_reflection(&mut state, id);
                    ReflectionCancellation::Terminalize
                }
                WorkState::Terminalizing => ReflectionCancellation::Late,
                WorkState::InlineForced => unreachable!("only a lazy route is forced inline"),
            };
            if !matches!(outcome, ReflectionCancellation::Late) {
                state.advance_work_generation(
                    CoordinatorMutationKind::Cancellation,
                    RouteHazard::All,
                );
            }
            outcome
        };
        drop(mutation);
        if !matches!(outcome, ReflectionCancellation::Late) {
            self.notify_all(CoordinatorMutationKind::Cancellation);
        }
        outcome
    }

    /// Releases claimed reflection work without a mutator, opening a
    /// boundary for status publication; coordinator tests drive it directly.
    #[cfg(test)]
    pub(in crate::evaluation) fn release_reflection(
        &self,
        claimed: ClaimedReflectionWork,
        poll: ReflectionWorkPoll,
    ) -> ReflectionWorkRelease {
        self.release_reflection_with(None, claimed, poll)
    }

    /// As [`Self::release_reflection`], under a quantum boundary's `mutator`.
    ///
    /// A status publisher builds its value and runs client code, so a
    /// release expected to change a published status runs suspended, under
    /// one gate with its transition: a runtime observed stable has published
    /// every blocked state. Any other change it makes publishes after the
    /// release's gate, suspended under a gate of its own; the advisory
    /// update's order check drops it if a newer status was published
    /// meanwhile.
    pub(in crate::evaluation) fn release_reflection_under(
        &self,
        mutator: &mut RuntimeValueMutator<'_, '_>,
        claimed: ClaimedReflectionWork,
        poll: ReflectionWorkPoll,
    ) -> ReflectionWorkRelease {
        if self.release_changes_published_status(claimed.id, &poll) {
            return mutator.suspend(|| self.release_reflection_with(None, claimed, poll));
        }
        self.release_reflection_with(Some(mutator), claimed, poll)
    }

    /// Whether releasing `work` with `poll` is expected to change a status
    /// its observers have seen.
    fn release_changes_published_status(
        &self,
        work: EvaluationWorkId,
        poll: &ReflectionWorkPoll,
    ) -> bool {
        let status = match poll {
            ReflectionWorkPoll::Runnable => EvaluationTaskStatus::Launched,
            ReflectionWorkPoll::Blocked(_) => EvaluationTaskStatus::Blocked,
            ReflectionWorkPoll::Terminal => return false,
        };
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state
            .work
            .get_mut(&work)
            .and_then(|record| record.obligations.task_publisher_mut())
            .is_some_and(|publisher| publisher.would_publish(&status))
    }

    fn release_reflection_with(
        &self,
        mutator: Option<&mut RuntimeValueMutator<'_, '_>>,
        claimed: ClaimedReflectionWork,
        poll: ReflectionWorkPoll,
    ) -> ReflectionWorkRelease {
        let ClaimedReflectionWork {
            id,
            task,
            demand,
            prior_block: _,
            mut machine,
        } = claimed;
        let demand_session = demand.id();
        let mutation = self.admission.mutation_guard();
        let (mut release, exact_subscription, mut route_tracker) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let mut route_tracker = ExactRouteReleaseTracker::new(&state, id);
            let (cancel, abandoned) = {
                let record = state
                    .work
                    .get(&id)
                    .expect("claimed reflection work must remain registered");
                assert_eq!(record.demand_session, demand_session);
                assert_eq!(reflection_work(record).task, task);
                assert!(matches!(record.state, WorkState::Running));
                assert!(
                    reflection_work(record).machine.is_none(),
                    "running reflection work must not retain its claimed machine"
                );
                (
                    matches!(
                        record.control.close_reason,
                        Some(WorkCloseReason::ExplicitCancellation)
                    ),
                    matches!(
                        record.control.close_reason,
                        Some(WorkCloseReason::DemandSessionClosed)
                    ),
                )
            };
            let (state_after, block, remains_blocked, terminal) = if cancel || abandoned {
                (WorkState::Terminalizing, None, false, true)
            } else {
                match poll {
                    ReflectionWorkPoll::Runnable => (WorkState::Queued, None, false, false),
                    ReflectionWorkPoll::Blocked(block) => {
                        (WorkState::Blocked, Some(block), true, false)
                    }
                    ReflectionWorkPoll::Terminal => (WorkState::Terminalizing, None, false, true),
                }
            };
            if !terminal {
                let record = state
                    .work
                    .get_mut(&id)
                    .expect("released reflection work must remain registered");
                let reflection = reflection_work_mut(record);
                assert!(reflection.machine.is_none());
                reflection.machine = machine.take();
            }
            let exact_subscription = if let Some(block) = block {
                assert!(matches!(state_after, WorkState::Blocked));
                publish_task_block_locked(&mut state, self.runtime, id, block)
            } else {
                let record = state
                    .work
                    .get_mut(&id)
                    .expect("claimed reflection work must remain registered");
                let reflection = reflection_work_mut(record);
                reflection.block = None;
                record.state = state_after;
                state.observation_waiters.remove(&id);
                None
            };
            if matches!(state_after, WorkState::Queued) {
                queue_reflection(&mut state, id);
            }
            let (cycle, cycle_error) = if matches!(state_after, WorkState::Blocked) {
                terminalize_lazy_cycle(&mut state, id)
            } else {
                (Vec::new(), None)
            };
            // The release changes only this work and any lazy cycle it
            // terminalized.
            let touched = std::iter::once(id)
                .chain(cycle.iter().map(|member| member.work))
                .collect::<Vec<_>>();
            state.advance_work_generation(
                CoordinatorMutationKind::WorkRelease,
                RouteHazard::Works(&touched),
            );
            route_tracker.changed(true);
            (
                ReflectionWorkRelease {
                    remains_blocked,
                    terminal,
                    cancel,
                    abandoned,
                    machine: None,
                    cycle,
                    cycle_error,
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
            release.remains_blocked = false;
        }
        #[cfg(test)]
        self.run_reflection_release_status_probe();
        if let Some(wait) = &promoted_wait {
            let promoted = self.promote_deferred_wait_guarded(&mutation, wait);
            route_tracker.changed(promoted);
        }
        let observation_woke = release.remains_blocked && self.recheck_observation_wait(id);
        route_tracker.changed(observation_woke);
        if observation_woke {
            release.remains_blocked = false;
        }
        let status_updates = if !release.terminal {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            state
                .work
                .get_mut(&id)
                .and_then(|record| {
                    // Releasing makes queued work claimable before this
                    // advisory status tail. A later quantum may therefore
                    // have advanced or retired the same stable record.
                    // Publish its current state when it remains live; a
                    // terminal update owns the final status otherwise.
                    let status = match record.state {
                        WorkState::Blocked => Some(EvaluationTaskStatus::Blocked),
                        WorkState::Dormant
                        | WorkState::Reserved
                        | WorkState::Queued
                        | WorkState::Running => Some(EvaluationTaskStatus::Launched),
                        WorkState::Terminalizing => None,
                        WorkState::InlineForced => {
                            unreachable!("only a lazy route is forced inline")
                        }
                    }?;
                    record
                        .obligations
                        .task_publisher_mut()
                        .map(|publisher| publisher.update_status(status, false))
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let (status_wakes, status_updates) = if mutator.is_some() {
            (Vec::new(), status_updates)
        } else {
            (
                Self::publish_status_updates(&mutation, status_updates),
                Vec::new(),
            )
        };
        release.route = Some({
            let state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            route_tracker.finish(&state)
        });
        drop(mutation);
        #[cfg(test)]
        self.run_reflection_release_end_probe();
        // A route's last lease releases the route, which takes the gate.
        drop(promoted_wait);
        self.notify_all(CoordinatorMutationKind::WorkRelease);
        for wake in status_wakes {
            wake.notify();
        }
        if let Some(mutator) = mutator
            && !status_updates.is_empty()
        {
            mutator.suspend(|| {
                let mutation = self.admission.mutation_guard();
                let wakes = Self::publish_status_updates(&mutation, status_updates);
                drop(mutation);
                for wake in wakes {
                    wake.notify();
                }
            });
        }
        release.machine = machine;
        release
    }

    fn publish_status_updates(
        mutation: &dyn RuntimeMutationAuthority,
        updates: Vec<TaskStatusUpdate>,
    ) -> Vec<TaskStatusWake> {
        updates
            .into_iter()
            .map(|update| {
                let publisher = update.publisher.clone();
                publisher.publish_update_guarded(mutation, update)
            })
            .collect()
    }

    pub(in crate::evaluation) fn retire_reflection(
        &self,
        id: EvaluationWorkId,
    ) -> Option<Box<dyn EvaluationTaskMachine>> {
        let mutation = self.admission.mutation_guard();
        let (machine, block) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let record = state
                .work
                .get(&id)
                .expect("terminal reflection work must remain registered");
            assert!(matches!(record.state, WorkState::Terminalizing));
            let detached = detach_reflection(&mut state, id, true);
            state.advance_work_generation(
                CoordinatorMutationKind::WorkRetirement,
                RouteHazard::Works(std::slice::from_ref(&id)),
            );
            detached
        };
        drop(mutation);
        drop(block);
        self.notify_all(CoordinatorMutationKind::WorkRetirement);
        machine
    }

    pub(in crate::evaluation) fn reflection_snapshots(
        &self,
        session: EvaluationSessionId,
    ) -> Vec<ReflectionWorkSnapshot> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        state
            .reflection
            .by_task
            .iter()
            .filter_map(|(task, id)| {
                let record = state.work.get(id)?;
                (record.demand_session == session).then(|| ReflectionWorkSnapshot {
                    task: *task,
                    state: reflection_state(record),
                })
            })
            .collect()
    }
}

pub(super) struct ReflectionWork {
    pub(super) task: EvaluationTaskId,
    pub(super) launch_parent: Option<EvaluationTaskId>,
    /// Whether the task's failure flags its anonymous root.
    pub(super) flags_root: bool,
    pub(super) wait: EvaluationWaitToken,
    pub(super) machine: Option<Box<dyn EvaluationTaskMachine>>,
    pub(super) block: Option<EvaluationTaskBlock>,
}

#[derive(Default)]
pub(super) struct ReflectionIndexes {
    pub(super) by_task: BTreeMap<EvaluationTaskId, EvaluationWorkId>,
    pub(super) by_wait: TrustedHashMap<EvaluationWaitToken, EvaluationWorkId>,
    /// Activated children only. Retiring an intermediate task promotes its
    /// still-live descendants to its own helping parent. This is a causal
    /// route, never an implicit join or owner.
    pub(super) children_by_parent: TrustedHashMap<EvaluationTaskId, Vec<EvaluationWorkId>>,
    /// Effective helping parent; may differ from immutable launch provenance
    /// after an intermediate task retires.
    pub(super) helping_parent_by_child: TrustedHashMap<EvaluationWorkId, EvaluationTaskId>,
}

pub(in crate::evaluation) struct ClaimedReflectionWork {
    pub(super) id: EvaluationWorkId,
    pub(super) task: EvaluationTaskId,
    pub(super) demand: ClaimedDemandSession,
    pub(super) prior_block: Option<EvaluationTaskBlock>,
    pub(super) machine: Option<Box<dyn EvaluationTaskMachine>>,
}

impl ClaimedReflectionWork {
    pub(in crate::evaluation) fn demand(&self) -> &ClaimedDemandSession {
        &self.demand
    }

    /// The panic that interrupted the dependency this work last blocked on.
    pub(in crate::evaluation) fn prior_dependency_panic(&self) -> Option<EvaluationPanic> {
        self.prior_block
            .as_ref()?
            .dependency
            .as_ref()?
            .panic_report()
    }

    pub(in crate::evaluation) fn id(&self) -> EvaluationWorkId {
        self.id
    }

    #[cfg(test)]
    pub(in crate::evaluation) fn task(&self) -> EvaluationTaskId {
        self.task
    }

    pub(in crate::evaluation) fn poll(
        &mut self,
        context: &mut super::super::EvaluationPollContext,
        step_budget: &mut super::super::EvaluationStepBudget,
    ) -> super::EvaluationMachinePoll {
        self.machine
            .as_mut()
            .expect("claimed reflection work must retain its detached machine")
            .poll(context, step_budget)
    }
}

pub(in crate::evaluation) enum ReflectionWorkPoll {
    /// The work runs again: it spent its budget, or another thread stopped it.
    Runnable,
    Blocked(EvaluationTaskBlock),
    Terminal,
}

pub(in crate::evaluation) struct ReflectionWorkRelease {
    pub(in crate::evaluation) remains_blocked: bool,
    pub(in crate::evaluation) terminal: bool,
    pub(in crate::evaluation) cancel: bool,
    pub(in crate::evaluation) abandoned: bool,
    pub(in crate::evaluation) machine: Option<Box<dyn EvaluationTaskMachine>>,
    pub(in crate::evaluation) cycle: Vec<super::DeferredLazyCycleMember>,
    pub(in crate::evaluation) cycle_error: Option<String>,
    pub(in crate::evaluation) route: Option<ExactRouteRelease>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::evaluation) enum ReflectionWorkState {
    Dormant,
    Reserved,
    Queued,
    Running,
    Blocked(EvaluationTaskBlock),
    Terminalizing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::evaluation) struct ReflectionWorkSnapshot {
    pub(in crate::evaluation) task: EvaluationTaskId,
    pub(in crate::evaluation) state: ReflectionWorkState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::evaluation) enum ReflectionCancellation {
    Requested,
    Terminalize,
    Late,
}

pub(in crate::evaluation) struct AbandonedReflectionWork {
    pub(in crate::evaluation) id: EvaluationWorkId,
    pub(in crate::evaluation) task: EvaluationTaskId,
    pub(in crate::evaluation) cancel: bool,
}

pub(super) fn reflection_work(record: &WorkRecord) -> &ReflectionWork {
    match &record.kind {
        WorkKind::Reflection(work) => work,
        WorkKind::Spark(_) => panic!("spark work cannot be used as a reflection task"),
        WorkKind::Deferred(_) => panic!("deferred work cannot be used as a reflection task"),
        WorkKind::LazyRoute(_) => panic!("lazy route cannot be used as a reflection task"),
    }
}

pub(super) fn reflection_work_mut(record: &mut WorkRecord) -> &mut ReflectionWork {
    match &mut record.kind {
        WorkKind::Reflection(work) => work,
        WorkKind::Spark(_) => panic!("spark work cannot be used as a reflection task"),
        WorkKind::Deferred(_) => panic!("deferred work cannot be used as a reflection task"),
        WorkKind::LazyRoute(_) => panic!("lazy route cannot be used as a reflection task"),
    }
}

pub(super) fn queue_reflection(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    assert!(matches!(
        state
            .work
            .get(&id)
            .expect("queued reflection work must remain registered")
            .kind,
        WorkKind::Reflection(_)
    ));
    queue_task(state, id);
}

pub(super) fn remove_ready_reflection(state: &mut WorkCoordinatorState, id: EvaluationWorkId) {
    remove_ready_task(state, id);
}

pub(super) fn claim_reflection(
    state: &mut WorkCoordinatorState,
    runtime: EvaluationRuntimeId,
    id: EvaluationWorkId,
) -> Option<ClaimedReflectionWork> {
    let demand_session = state.work.get(&id)?.demand_session;
    let demand = ClaimedDemandSession::registered(state, demand_session, runtime)?;
    let (task, prior_block, machine) = {
        let record = state.work.get_mut(&id)?;
        if !matches!(record.kind, WorkKind::Reflection(_))
            || !matches!(record.state, WorkState::Queued)
        {
            return None;
        }
        record.state = WorkState::Running;
        let reflection = reflection_work_mut(record);
        (
            reflection.task,
            reflection.block.take(),
            reflection
                .machine
                .take()
                .expect("claimable reflection work must retain its machine"),
        )
    };
    state.observation_waiters.remove(&id);
    remove_ready_reflection(state, id);
    Some(ClaimedReflectionWork {
        id,
        task,
        demand,
        prior_block,
        machine: Some(machine),
    })
}

pub(super) fn reflection_state(record: &WorkRecord) -> ReflectionWorkState {
    match record.state {
        WorkState::Dormant => ReflectionWorkState::Dormant,
        WorkState::Reserved => ReflectionWorkState::Reserved,
        WorkState::Queued => ReflectionWorkState::Queued,
        WorkState::Running => ReflectionWorkState::Running,
        WorkState::InlineForced => unreachable!("only a lazy route is forced inline"),
        WorkState::Blocked => ReflectionWorkState::Blocked(
            reflection_work(record)
                .block
                .clone()
                .expect("blocked reflection work must retain its block"),
        ),
        WorkState::Terminalizing => ReflectionWorkState::Terminalizing,
    }
}

/// Removes reflection work from the coordinator, returning its machine and
/// its block for the caller to drop after releasing its locks: a block's
/// dependency may hold a lazy route's last lease, whose release takes them.
pub(super) fn detach_reflection(
    state: &mut WorkCoordinatorState,
    id: EvaluationWorkId,
    require_settled: bool,
) -> (
    Option<Box<dyn EvaluationTaskMachine>>,
    Option<EvaluationTaskBlock>,
) {
    state.observation_waiters.remove(&id);
    remove_ready_reflection(state, id);
    unregister_background_root(state, id);
    let mut record = state
        .work
        .remove(&id)
        .expect("retired reflection work must remain registered");
    let discarded = if require_settled {
        assert!(
            record.obligations.is_empty(),
            "reflection work cannot retire before terminal settlement"
        );
        None
    } else {
        assert!(
            record.obligations.owned_promises.is_empty(),
            "an uncommitted reflection reservation cannot own promises"
        );
        record.obligations.take_producer()
    };
    let WorkKind::Reflection(reflection) = record.kind else {
        panic!("reflection retirement must contain reflection work")
    };
    if let Some(obligation) = discarded {
        let ProducerSettlementObligation::ReflectionTask(publisher) = obligation else {
            panic!("reflection work must retain a reflection task-wait obligation")
        };
        assert_eq!(publisher.wait, reflection.wait);
    }
    assert_eq!(
        state.reflection.by_task.remove(&reflection.task),
        Some(id),
        "reflection task index must agree with its work record"
    );
    assert_eq!(
        state.reflection.by_wait.remove(&reflection.wait),
        Some(id),
        "reflection wait index must agree with its work record"
    );
    let descendants = state
        .reflection
        .children_by_parent
        .remove(&reflection.task)
        .unwrap_or_default();
    if let Some(parent) = state.reflection.helping_parent_by_child.remove(&id) {
        let children = state
            .reflection
            .children_by_parent
            .get_mut(&parent)
            .expect("activated reflection child must retain its parent index");
        let position = children
            .iter()
            .position(|child| *child == id)
            .expect("activated reflection child must remain in its parent index");
        children.remove(position);
        children.splice(position..position, descendants.iter().copied());
        for descendant in descendants {
            state
                .reflection
                .helping_parent_by_child
                .insert(descendant, parent);
        }
        if children.is_empty() {
            state.reflection.children_by_parent.remove(&parent);
        }
    } else {
        // A root with no helping parent leaves autonomous descendants, not
        // orphaned index entries under a retired task identity.
        for descendant in descendants {
            state.reflection.helping_parent_by_child.remove(&descendant);
        }
    }
    if let Some(session_work) = state.work_by_session.get_mut(&record.demand_session) {
        session_work.remove(&id);
        if session_work.is_empty() {
            state.work_by_session.remove(&record.demand_session);
        }
    }
    prune_closed_session_registration(state, record.demand_session);
    (reflection.machine, reflection.block)
}
