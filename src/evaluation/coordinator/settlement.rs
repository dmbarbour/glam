//! Runtime readiness snapshots and terminal settlement publication.

use std::sync::Arc;

use crate::runtime::{RuntimeFailureRoot, RuntimeMutationAuthority};

use super::super::{EvaluationFailure, RuntimeObservationEpoch};
use super::client_demand::{ClientDemandResult, detach_client_demand};
use super::deferred::detach_deferred;
use super::reflection::{detach_reflection, reflection_work, reflection_work_mut};
use super::{
    ClientDemandRetirement, CompletionWake, EvaluationExitBlock, EvaluationSessionId,
    EvaluationTaskId, EvaluationTaskMachine, EvaluationTaskStatus, EvaluationWaitTerminal,
    EvaluationWorkCoordinator, EvaluationWorkId, ExitIntent, ProducerSettlementObligation,
    TaskOwnedPromiseObligation, TaskStatusPublisher, TaskStatusUpdate, TaskStatusWake,
    WorkCoordinatorState, WorkDependency, WorkKind, WorkRecord, WorkState, task_block,
    task_for_record, task_observation_epoch, terminal_task_status, work_dependency,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimePumpSnapshot {
    pub(crate) useful_ready: bool,
    pub(crate) progress_owned: bool,
    pub(crate) abandonable_sparks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeCoordinatorReadiness {
    Busy,
    Ready {
        work_generation: u64,
        exits: Vec<RuntimeExitSnapshot>,
    },
    Deadlocked {
        work_generation: u64,
        exits: Vec<RuntimeExitSnapshot>,
        unfinished: Vec<RuntimeDeadlockWorkSnapshot>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeExitSnapshot {
    pub(crate) work: EvaluationWorkId,
    pub(crate) session: EvaluationSessionId,
    pub(crate) task: EvaluationTaskId,
    pub(crate) intent: ExitIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeWorkKindSnapshot {
    ReflectionTask,
    DeferredEvaluation,
    ClientDemand,
    Spark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeWorkStateSnapshot {
    Dormant,
    Reserved,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeDependencySnapshot {
    Wait {
        wait: u64,
        producer: EvaluationTaskId,
        session: EvaluationSessionId,
    },
    Promise {
        promise: u64,
        producer: Option<(u64, EvaluationTaskId, EvaluationSessionId)>,
    },
    #[cfg(test)]
    Test(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeDeadlockWorkSnapshot {
    pub(crate) work: EvaluationWorkId,
    pub(crate) session: EvaluationSessionId,
    pub(crate) task: Option<EvaluationTaskId>,
    pub(crate) kind: RuntimeWorkKindSnapshot,
    pub(crate) state: RuntimeWorkStateSnapshot,
    pub(crate) dependency: Option<RuntimeDependencySnapshot>,
    pub(crate) observed_epoch: Option<RuntimeObservationEpoch>,
    pub(crate) blocked_error: Option<RuntimeFailureRoot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedRuntimeSettlementPlan {
    pub(crate) work_generation: u64,
    pub(crate) exits: Vec<RuntimeExitSnapshot>,
    pub(crate) kills: Vec<RuntimeDeadlockWorkSnapshot>,
}

impl EvaluationWorkCoordinator {
    /// Inspects scheduler activity while the caller holds settlement
    /// admission exclusively. This method itself is observational.
    pub(crate) fn runtime_pump_snapshot(&self) -> RuntimePumpSnapshot {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        RuntimePumpSnapshot {
            useful_ready: state.work.values().any(|record| {
                matches!(
                    record.kind,
                    WorkKind::Reflection(_) | WorkKind::Deferred(_) | WorkKind::ClientDemand(_)
                ) && matches!(record.state, WorkState::Queued)
            }),
            progress_owned: state.work.values().any(|record| {
                matches!(record.state, WorkState::Running | WorkState::Terminalizing)
            }),
            abandonable_sparks: state.work.values().any(|record| {
                matches!(record.kind, WorkKind::Spark(_))
                    && matches!(record.state, WorkState::Queued | WorkState::Blocked)
            }),
        }
    }

    /// Classifies all retained work while the caller holds settlement
    /// admission exclusively. No queue, work record, or generation is changed.
    pub(crate) fn runtime_readiness_snapshot(&self) -> RuntimeCoordinatorReadiness {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        runtime_readiness_locked(&state)
    }

    /// Revalidates one proposed ready disposition set without changing work.
    pub(crate) fn validate_runtime_settlement(
        &self,
        work_generation: u64,
        exits: &[RuntimeExitSnapshot],
        kills: &[RuntimeDeadlockWorkSnapshot],
    ) -> Option<ValidatedRuntimeSettlementPlan> {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        if !settlement_dispositions_match_locked(&state, work_generation, exits, kills) {
            return None;
        }
        Some(ValidatedRuntimeSettlementPlan {
            work_generation,
            exits: exits.to_vec(),
            kills: kills.to_vec(),
        })
    }
}

fn runtime_readiness_locked(state: &WorkCoordinatorState) -> RuntimeCoordinatorReadiness {
    let mut exits = Vec::new();
    let mut unfinished = Vec::new();

    for record in state.work.values() {
        if matches!(
            record.state,
            WorkState::Queued | WorkState::Running | WorkState::Terminalizing
        ) || matches!(record.kind, WorkKind::Spark(_))
        {
            return RuntimeCoordinatorReadiness::Busy;
        }

        if matches!(record.state, WorkState::ExitWaiting) {
            let reflection = reflection_work(record);
            exits.push(RuntimeExitSnapshot {
                work: record.id,
                session: record.demand_session,
                task: reflection.task,
                intent: reflection
                    .exit
                    .as_ref()
                    .expect("exit-waiting reflection work must retain its exit summary")
                    .intent
                    .clone(),
            });
            continue;
        }

        let state_snapshot = match record.state {
            WorkState::Dormant => RuntimeWorkStateSnapshot::Dormant,
            WorkState::Reserved => RuntimeWorkStateSnapshot::Reserved,
            WorkState::Blocked => RuntimeWorkStateSnapshot::Blocked,
            WorkState::Queued
            | WorkState::Running
            | WorkState::ExitWaiting
            | WorkState::Terminalizing => unreachable!("handled above"),
        };
        unfinished.push(RuntimeDeadlockWorkSnapshot {
            work: record.id,
            session: record.demand_session,
            task: task_for_record(record),
            kind: runtime_work_kind(record),
            state: state_snapshot,
            dependency: work_dependency(record).map(runtime_dependency_snapshot),
            observed_epoch: task_observation_epoch(record),
            blocked_error: task_block(record)
                .and_then(|block| block.error.as_ref())
                .cloned(),
        });
    }

    exits.sort_by_key(|exit| exit.work.get());
    if unfinished.is_empty() {
        RuntimeCoordinatorReadiness::Ready {
            work_generation: state.work_generation,
            exits,
        }
    } else {
        unfinished.sort_by_key(|work| work.work.get());
        RuntimeCoordinatorReadiness::Deadlocked {
            work_generation: state.work_generation,
            exits,
            unfinished,
        }
    }
}

fn settlement_dispositions_match_locked(
    state: &WorkCoordinatorState,
    work_generation: u64,
    exits: &[RuntimeExitSnapshot],
    kills: &[RuntimeDeadlockWorkSnapshot],
) -> bool {
    match runtime_readiness_locked(state) {
        RuntimeCoordinatorReadiness::Ready {
            work_generation: current_generation,
            exits: current_exits,
        } => kills.is_empty() && current_generation == work_generation && current_exits == exits,
        RuntimeCoordinatorReadiness::Deadlocked {
            work_generation: current_generation,
            exits: current_exits,
            unfinished,
        } => {
            !kills.is_empty()
                && current_generation == work_generation
                && current_exits == exits
                && unfinished == kills
        }
        RuntimeCoordinatorReadiness::Busy => false,
    }
}

fn runtime_work_kind(record: &WorkRecord) -> RuntimeWorkKindSnapshot {
    match record.kind {
        WorkKind::Reflection(_) => RuntimeWorkKindSnapshot::ReflectionTask,
        WorkKind::Deferred(_) => RuntimeWorkKindSnapshot::DeferredEvaluation,
        WorkKind::ClientDemand(_) => RuntimeWorkKindSnapshot::ClientDemand,
        WorkKind::Spark(_) => RuntimeWorkKindSnapshot::Spark,
    }
}

fn runtime_dependency_snapshot(dependency: &WorkDependency) -> RuntimeDependencySnapshot {
    match dependency {
        WorkDependency::Wait(wait) => RuntimeDependencySnapshot::Wait {
            wait: wait.get(),
            producer: wait.producer(),
            session: wait.owner_id(),
        },
        WorkDependency::Promise(promise) => RuntimeDependencySnapshot::Promise {
            promise: promise.id().get(),
            producer: dependency
                .producer_wait()
                .map(|wait| (wait.get(), wait.producer(), wait.owner_id())),
        },
        #[cfg(test)]
        WorkDependency::Test(dependency) => RuntimeDependencySnapshot::Test(dependency.id.get()),
    }
}

impl EvaluationWorkCoordinator {
    /// Revalidates and atomically publishes every proposed disposition while
    /// the caller holds exclusive settlement admission.
    pub(crate) fn publish_runtime_settlement(
        self: &Arc<Self>,
        mutation: &dyn RuntimeMutationAuthority,
        plan: &ValidatedRuntimeSettlementPlan,
        kill_failure: Option<RuntimeFailureRoot>,
    ) -> Option<RuntimeSettlementRelease> {
        if plan.kills.is_empty() != kill_failure.is_none() {
            return None;
        }
        let exit_promise_failure = (!plan.exits.is_empty()).then(|| {
            RuntimeFailureRoot::from_observer(
                &self.values,
                Arc::new(EvaluationFailure::message(
                    "reflection task exited without fulfilling its promised value",
                )),
            )
        });
        let (mut selected, client_demands) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if !settlement_dispositions_match_locked(
                &state,
                plan.work_generation,
                &plan.exits,
                &plan.kills,
            ) {
                return None;
            }

            let mut selected = Vec::with_capacity(plan.exits.len() + plan.kills.len());
            let mut client_demands = Vec::new();
            for proposed in &plan.exits {
                let (producer, status_update, promises, machine, exit) = {
                    let record = state
                        .work
                        .get_mut(&proposed.work)
                        .expect("validated exit work must remain registered");
                    assert!(matches!(record.state, WorkState::ExitWaiting));
                    let reflection = reflection_work_mut(record);
                    assert_eq!(reflection.task, proposed.task);
                    let exit = reflection
                        .exit
                        .take()
                        .expect("exit-waiting work must retain its exit summary");
                    assert_eq!(exit.intent, proposed.intent);
                    let machine = reflection.machine.take();
                    let mut producer = record
                        .obligations
                        .take_producer()
                        .expect("exit work must retain its producer obligation");
                    let status_update = match &mut producer {
                        ProducerSettlementObligation::ReflectionTask(publisher) => {
                            publisher.update_status(EvaluationTaskStatus::Exited, true)
                        }
                        ProducerSettlementObligation::DeferredClaim { .. } => {
                            panic!("only reflection work may publish an exit disposition")
                        }
                    };
                    let promises = record.obligations.owned_promises.clone();
                    record.state = WorkState::Terminalizing;
                    (producer, status_update, promises, machine, exit)
                };
                state.observation_waiters.remove(&proposed.work);
                selected.push(SelectedTaskSettlement {
                    work: proposed.work,
                    producer: Some(producer),
                    status_updates: status_update,
                    promises,
                    machine,
                    block: None,
                    exit: Some(exit),
                    terminal: EvaluationWaitTerminal::Exited,
                    promise_failure: exit_promise_failure
                        .as_ref()
                        .expect("an exit settlement must retain its promise failure")
                        .clone(),
                });
            }

            for proposed in &plan.kills {
                if matches!(
                    state
                        .work
                        .get(&proposed.work)
                        .expect("validated killed work must remain registered")
                        .kind,
                    WorkKind::ClientDemand(_)
                ) {
                    client_demands.push(detach_client_demand(
                        &mut state,
                        proposed.work,
                        None,
                        None,
                        ClientDemandResult::Killed(
                            kill_failure
                                .as_ref()
                                .expect("forced settlement must retain its failure")
                                .clone(),
                        ),
                    ));
                    continue;
                }

                let (producer, status_update, promises, machine, block) = {
                    let record = state
                        .work
                        .get_mut(&proposed.work)
                        .expect("validated killed work must remain registered");
                    assert!(matches!(
                        record.state,
                        WorkState::Dormant | WorkState::Reserved | WorkState::Blocked
                    ));
                    let (machine, block) = match &mut record.kind {
                        WorkKind::Reflection(reflection) => {
                            (reflection.machine.take(), reflection.block.take())
                        }
                        WorkKind::Deferred(deferred) => (
                            Some(
                                deferred
                                    .machine
                                    .take()
                                    .expect("stable deferred work must retain its machine"),
                            ),
                            deferred.block.take(),
                        ),
                        WorkKind::Spark(_) => {
                            unreachable!("stable deadlock cannot retain best-effort spark work")
                        }
                        WorkKind::ClientDemand(_) => unreachable!("handled above"),
                    };
                    let mut producer = record
                        .obligations
                        .take_producer()
                        .expect("killed task work must retain its producer obligation");
                    let killed = EvaluationTaskStatus::Killed(
                        kill_failure
                            .as_ref()
                            .expect("forced settlement must retain its failure")
                            .clone(),
                    );
                    let status_update = match &mut producer {
                        ProducerSettlementObligation::ReflectionTask(publisher) => {
                            publisher.update_status(killed, true)
                        }
                        ProducerSettlementObligation::DeferredClaim { .. } => Vec::new(),
                    };
                    let promises = record.obligations.owned_promises.clone();
                    record.state = WorkState::Terminalizing;
                    (producer, status_update, promises, machine, block)
                };
                state.observation_waiters.remove(&proposed.work);
                let failure_root = kill_failure
                    .as_ref()
                    .expect("forced settlement must retain its failure")
                    .clone();
                selected.push(SelectedTaskSettlement {
                    work: proposed.work,
                    producer: Some(producer),
                    status_updates: status_update,
                    promises,
                    machine,
                    block,
                    exit: None,
                    terminal: EvaluationWaitTerminal::Killed(failure_root.clone()),
                    promise_failure: failure_root,
                });
            }
            if !selected.is_empty() || !client_demands.is_empty() {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            (selected, client_demands)
        };

        let mut completion_wakes = Vec::new();
        let mut status_wakes = Vec::new();
        let mut status_publishers = Vec::new();
        let mut promise_publications = Vec::new();
        for selected in &mut selected {
            let wait = match selected
                .producer
                .as_ref()
                .expect("selected task must retain its producer")
            {
                ProducerSettlementObligation::ReflectionTask(publisher) => &publisher.wait,
                ProducerSettlementObligation::DeferredClaim { wait, .. } => wait,
            };
            let (_, wake) =
                wait.publish_terminal_guarded(self, mutation, selected.terminal.clone());
            completion_wakes.push(wake);
            for (publisher, status) in std::mem::take(&mut selected.status_updates) {
                debug_assert_eq!(status, terminal_task_status(&selected.terminal));
                status_wakes.push(publisher.publish_guarded(mutation, status));
                status_publishers.push(publisher);
            }
            for obligation in &selected.promises {
                let (producer, completion) = obligation.clone().publish_failure_guarded(
                    self,
                    mutation,
                    selected.promise_failure.as_failure().clone(),
                );
                promise_publications.push(producer);
                completion_wakes.push(completion);
            }
        }

        let (machines, blocks, exits, terminals) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let machines = selected
                .iter_mut()
                .filter_map(|selected| {
                    let record = state
                        .work
                        .get(&selected.work)
                        .expect("settled exit work must remain registered");
                    assert!(record.obligations.is_empty());
                    match record.kind {
                        WorkKind::Reflection(_) => {
                            let detached = detach_reflection(&mut state, selected.work, true);
                            assert!(
                                detached.is_none(),
                                "settlement must detach a reflection machine before retirement"
                            );
                        }
                        WorkKind::Deferred(_) => detach_deferred(&mut state, selected.work),
                        WorkKind::Spark(_) | WorkKind::ClientDemand(_) => {
                            unreachable!("selected task settlement must contain task work")
                        }
                    }
                    selected.machine.take()
                })
                .collect::<Vec<_>>();
            if !selected.is_empty() {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            let blocks = selected
                .iter_mut()
                .filter_map(|selected| selected.block.take())
                .collect();
            let exits = selected
                .iter_mut()
                .filter_map(|selected| selected.exit.take())
                .collect();
            let terminals = selected
                .iter()
                .map(|selected| selected.terminal.clone())
                .collect();
            (machines, blocks, exits, terminals)
        };

        Some(RuntimeSettlementRelease {
            coordinator: self.clone(),
            producers: selected
                .iter_mut()
                .map(|selected| {
                    selected
                        .producer
                        .take()
                        .expect("settled task must retain its detached producer")
                })
                .collect(),
            machines,
            blocks,
            exits,
            terminals,
            client_demands,
            completion_wakes,
            status_wakes,
            status_publishers,
            promise_publications,
        })
    }
}

pub(super) struct SelectedTaskSettlement {
    pub(super) work: EvaluationWorkId,
    pub(super) producer: Option<ProducerSettlementObligation>,
    pub(super) status_updates: Vec<TaskStatusUpdate>,
    pub(super) promises: Vec<TaskOwnedPromiseObligation>,
    pub(super) machine: Option<Box<dyn EvaluationTaskMachine>>,
    pub(super) block: Option<super::EvaluationTaskBlock>,
    pub(super) exit: Option<EvaluationExitBlock>,
    pub(super) terminal: EvaluationWaitTerminal,
    pub(super) promise_failure: RuntimeFailureRoot,
}

/// Resources detached by one successful exit settlement.
///
/// All semantic terminal cells are authoritative before this value is
/// returned. Its notifications and potentially value-owning drops are delayed
/// until the caller has released exclusive settlement admission.
pub(crate) struct RuntimeSettlementRelease {
    pub(super) coordinator: Arc<EvaluationWorkCoordinator>,
    pub(super) producers: Vec<ProducerSettlementObligation>,
    pub(super) machines: Vec<Box<dyn EvaluationTaskMachine>>,
    pub(super) blocks: Vec<super::EvaluationTaskBlock>,
    pub(super) exits: Vec<EvaluationExitBlock>,
    pub(super) terminals: Vec<EvaluationWaitTerminal>,
    pub(super) client_demands: Vec<ClientDemandRetirement>,
    pub(super) completion_wakes: Vec<CompletionWake>,
    pub(super) status_wakes: Vec<TaskStatusWake>,
    pub(super) status_publishers: Vec<TaskStatusPublisher>,
    pub(super) promise_publications: Vec<super::PromiseProducerPublication>,
}

impl RuntimeSettlementRelease {
    pub(crate) fn finish(self) {
        let Self {
            coordinator,
            producers,
            machines,
            blocks,
            exits,
            terminals,
            client_demands,
            completion_wakes,
            status_wakes,
            status_publishers,
            promise_publications,
        } = self;
        for retirement in client_demands {
            retirement.finish();
        }
        for wake in completion_wakes {
            wake.notify();
        }
        for publication in promise_publications {
            publication.notify();
        }
        for wake in status_wakes {
            wake.notify();
        }
        coordinator.work_available.notify_all();
        coordinator.admission.notify_settlement();
        drop(producers);
        drop(machines);
        drop(blocks);
        drop(exits);
        drop(terminals);
        drop(status_publishers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-exhaustive ownership latch for the failure-bearing settlement
    /// records migrated by I4F.1c.4.
    fn assert_settlement_failure_boundary_inventory(
        snapshot: &RuntimeDeadlockWorkSnapshot,
        selected: &SelectedTaskSettlement,
    ) {
        let RuntimeDeadlockWorkSnapshot { blocked_error, .. } = snapshot;
        let _: &Option<RuntimeFailureRoot> = blocked_error;

        let SelectedTaskSettlement {
            terminal,
            promise_failure,
            ..
        } = selected;
        let _: &EvaluationWaitTerminal = terminal;
        let _: &RuntimeFailureRoot = promise_failure;
    }

    #[test]
    fn settlement_failure_boundary_inventory_is_complete() {
        let _: fn(&RuntimeDeadlockWorkSnapshot, &SelectedTaskSettlement) =
            assert_settlement_failure_boundary_inventory;
    }
}
