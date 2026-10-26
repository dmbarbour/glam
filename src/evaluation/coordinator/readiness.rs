//! Runtime readiness snapshots: whether work runs, and what is unfinished.

use crate::core::LazyId;
use crate::runtime::RuntimeFailureRoot;

use super::super::RuntimeObservationEpoch;
use super::{
    CausalBackgroundProbe, EvaluationSessionId, EvaluationTaskId, EvaluationWaitToken,
    EvaluationWorkCoordinator, EvaluationWorkId, WorkCoordinatorState, WorkDependency, WorkKind,
    WorkRecord, WorkState, causal_background_probe_locked, task_block, task_for_record,
    task_observation_epoch, work_dependency,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimePumpSnapshot {
    /// Ready work which the background pump is authorized to claim.
    ///
    /// A queued foreground client record remains `Busy` for readiness, but is
    /// not a reason for the background pump to spin: only its exact client
    /// driver may advance it.
    pub(crate) background_ready: bool,
    pub(crate) background_busy: bool,
    pub(crate) spark_busy: bool,
    pub(crate) abandonable_sparks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeCoordinatorReadiness {
    Busy,
    /// No work is queued or runs: whatever remains waits, as an analysis
    /// classifies it (`quiescence-is-analyzed`).
    Stable {
        work_generation: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeWorkKindSnapshot {
    ReflectionTask,
    DeferredEvaluation,
    LazyRoute,
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
    LazyWait {
        wait: u64,
        lazy: LazyId,
    },
    Promise {
        promise: u64,
        producer: Option<(u64, EvaluationTaskId, EvaluationSessionId)>,
    },
    #[cfg(test)]
    Test(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeWorkSnapshot {
    pub(crate) work: EvaluationWorkId,
    pub(crate) session: Option<EvaluationSessionId>,
    pub(crate) task: Option<EvaluationTaskId>,
    pub(crate) lazy: Option<LazyId>,
    pub(crate) kind: RuntimeWorkKindSnapshot,
    pub(crate) state: RuntimeWorkStateSnapshot,
    pub(crate) dependency: Option<RuntimeDependencySnapshot>,
    pub(crate) observed_epoch: Option<RuntimeObservationEpoch>,
    pub(crate) blocked_error: Option<RuntimeFailureRoot>,
}

impl EvaluationWorkCoordinator {
    /// Inspects scheduler activity while the caller holds exclusive
    /// admission exclusively. This method itself is observational.
    pub(crate) fn runtime_pump_snapshot(&self) -> RuntimePumpSnapshot {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        runtime_pump_snapshot_locked(&state)
    }

    /// Classifies all retained work while the caller holds exclusive
    /// admission exclusively. No queue, work record, or generation is changed.
    pub(crate) fn runtime_readiness_snapshot(&self) -> RuntimeCoordinatorReadiness {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        runtime_readiness_locked(&state)
    }
}

pub(super) fn runtime_pump_snapshot_locked(state: &WorkCoordinatorState) -> RuntimePumpSnapshot {
    let mut background_ready = false;
    let mut background_busy = false;
    for root in &state.background_roots {
        let Some(record) = state.work.get(root) else {
            continue;
        };
        if !matches!(record.kind, WorkKind::Reflection(_)) {
            continue;
        }
        match causal_background_probe_locked(state, *root) {
            CausalBackgroundProbe::Ready(candidate) => {
                if state.work.contains_key(&candidate) {
                    background_ready = true;
                }
            }
            CausalBackgroundProbe::Busy(_) => background_busy = true,
            CausalBackgroundProbe::None => {}
        }
    }
    RuntimePumpSnapshot {
        background_ready,
        background_busy,
        spark_busy: state.work.values().any(|record| {
            matches!(record.kind, WorkKind::Spark(_))
                && matches!(record.state, WorkState::Running | WorkState::Terminalizing)
        }),
        abandonable_sparks: state.work.values().any(|record| {
            matches!(record.kind, WorkKind::Spark(_))
                && matches!(record.state, WorkState::Queued | WorkState::Blocked)
        }),
    }
}

fn runtime_readiness_locked(state: &WorkCoordinatorState) -> RuntimeCoordinatorReadiness {
    // Foreground evaluation is external client activity, not scheduler work
    // which the runtime may diagnose as unfinished. Its owner completes,
    // cancels, or drops it.
    if !state.client_demands.is_empty() {
        return RuntimeCoordinatorReadiness::Busy;
    }

    for record in state.work.values() {
        if matches!(
            record.state,
            WorkState::Queued
                | WorkState::Running
                | WorkState::InlineForced
                | WorkState::Terminalizing
        ) || matches!(record.kind, WorkKind::Spark(_))
        {
            return RuntimeCoordinatorReadiness::Busy;
        }
    }
    RuntimeCoordinatorReadiness::Stable {
        work_generation: state.work_generation,
    }
}

/// `record`'s snapshot as unfinished work, waiting or not yet started, or
/// `None` where it runs or ends.
pub(super) fn unfinished_work_snapshot_locked(
    state: &WorkCoordinatorState,
    record: &WorkRecord,
) -> Option<RuntimeWorkSnapshot> {
    let state_snapshot = match record.state {
        WorkState::Dormant => RuntimeWorkStateSnapshot::Dormant,
        WorkState::Reserved => RuntimeWorkStateSnapshot::Reserved,
        WorkState::Blocked => RuntimeWorkStateSnapshot::Blocked,
        WorkState::Queued
        | WorkState::Running
        | WorkState::InlineForced
        | WorkState::Terminalizing => return None,
    };
    Some(RuntimeWorkSnapshot {
        work: record.id,
        session: (!matches!(record.kind, WorkKind::LazyRoute(_))).then_some(record.demand_session),
        task: task_for_record(record),
        lazy: match &record.kind {
            WorkKind::LazyRoute(route) => Some(route.lazy.id()),
            _ => None,
        },
        kind: runtime_work_kind(record),
        state: state_snapshot,
        dependency: work_dependency(record)
            .map(|dependency| runtime_dependency_snapshot(state, dependency)),
        observed_epoch: task_observation_epoch(record),
        blocked_error: task_block(record)
            .and_then(|block| block.error.as_ref())
            .cloned(),
    })
}

fn runtime_work_kind(record: &WorkRecord) -> RuntimeWorkKindSnapshot {
    match record.kind {
        WorkKind::Reflection(_) => RuntimeWorkKindSnapshot::ReflectionTask,
        WorkKind::Deferred(_) => RuntimeWorkKindSnapshot::DeferredEvaluation,
        WorkKind::LazyRoute(_) => RuntimeWorkKindSnapshot::LazyRoute,
        WorkKind::Spark(_) => RuntimeWorkKindSnapshot::Spark,
    }
}

fn runtime_dependency_snapshot(
    state: &WorkCoordinatorState,
    dependency: &WorkDependency,
) -> RuntimeDependencySnapshot {
    let origin = |wait: &EvaluationWaitToken| {
        let work = super::work_for_wait_locked(state, wait)
            .expect("active dependency wait must have a work record");
        let record = state
            .work
            .get(&work)
            .expect("indexed dependency work must remain registered");
        match &record.kind {
            WorkKind::LazyRoute(route) => RuntimeDependencySnapshot::LazyWait {
                wait: wait.get(),
                lazy: route.lazy.id(),
            },
            _ => RuntimeDependencySnapshot::Wait {
                wait: wait.get(),
                producer: super::task_for_record(record)
                    .expect("non-lazy dependency is task-owned"),
                session: record.demand_session,
            },
        }
    };
    match dependency {
        WorkDependency::Wait(wait) => origin(wait),
        WorkDependency::Promise(promise) => RuntimeDependencySnapshot::Promise {
            promise: promise.id().get(),
            producer: dependency.producer_wait().map(|wait| match origin(&wait) {
                RuntimeDependencySnapshot::Wait {
                    producer, session, ..
                } => (wait.get(), producer, session),
                RuntimeDependencySnapshot::LazyWait { .. } => {
                    panic!("task-owned promise cannot have a lazy route producer")
                }
                _ => unreachable!(),
            }),
        },
        #[cfg(test)]
        WorkDependency::Test(dependency) => RuntimeDependencySnapshot::Test(dependency.id.get()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-exhaustive ownership latch for the failure-bearing readiness
    /// record.
    fn assert_readiness_failure_boundary_inventory(snapshot: &RuntimeWorkSnapshot) {
        let RuntimeWorkSnapshot { blocked_error, .. } = snapshot;
        let _: &Option<RuntimeFailureRoot> = blocked_error;
    }

    #[test]
    fn readiness_failure_boundary_inventory_is_complete() {
        let _: fn(&RuntimeWorkSnapshot) = assert_readiness_failure_boundary_inventory;
    }
}
