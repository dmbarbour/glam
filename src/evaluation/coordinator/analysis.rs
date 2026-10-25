//! The quiescence analysis's view of work (`quiescence-is-analyzed`): each
//! unfinished work with what it waits on, classified by family, at any
//! time, changing nothing.

use std::collections::HashSet;

use super::settlement::unfinished_work_snapshot_locked;
use super::{
    DependencyEdge, EvaluationWorkCoordinator, EvaluationWorkId, RuntimeDeadlockWorkSnapshot,
    WorkCoordinatorState, WorkKind, WorkState, dependency_edge_locked, task_block, work_dependency,
};

/// What a waiting work waits on, as the analysis classifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeWaitFamilySnapshot {
    /// It owes nothing: it waits on observed state or input, or for
    /// queued tasks, or has not started, no one demanding it.
    Idle,
    /// It waits on work that still runs, or can, so the runtime is not
    /// quiescent.
    InProgress,
    /// It awaits a value, a promise's or other work's, that nothing in
    /// progress will produce.
    PromiseWait,
    /// It waits for a change after an error, which a change might cure.
    ErredWait,
}

/// One unfinished work, its family, the work its wait ends at, where that
/// is another's, and whether another unfinished work awaits it, so that it
/// is a link in that work's wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeWaitSnapshot {
    pub(crate) work: RuntimeDeadlockWorkSnapshot,
    pub(crate) family: RuntimeWaitFamilySnapshot,
    pub(crate) cause: Option<EvaluationWorkId>,
    pub(crate) awaited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeAnalysisSnapshot {
    /// Whether no work runs or can: nothing queued or running, no client
    /// demand outstanding, and no wait in progress.
    pub(crate) quiescent: bool,
    pub(crate) waits: Vec<RuntimeWaitSnapshot>,
}

impl EvaluationWorkCoordinator {
    /// Classifies every unfinished work by what it waits on. It observes
    /// only, under the coordinator's lock, and may be taken at any time.
    pub(crate) fn runtime_analysis_snapshot(&self) -> RuntimeAnalysisSnapshot {
        let state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        let mut quiescent = state.client_demands.is_empty();
        let awaited: HashSet<EvaluationWorkId> = state
            .work
            .values()
            .filter(|record| matches!(record.state, WorkState::Blocked))
            .filter_map(work_dependency)
            .filter_map(
                |dependency| match dependency_edge_locked(&state, dependency) {
                    DependencyEdge::Producer(producer) => Some(producer),
                    DependencyEdge::WakePending | DependencyEdge::End => None,
                },
            )
            .collect();
        let mut waits = Vec::new();
        for record in state.work.values() {
            if matches!(record.kind, WorkKind::Spark(_)) {
                quiescent = false;
                continue;
            }
            let Some(work) = unfinished_work_snapshot_locked(&state, record) else {
                quiescent = false;
                continue;
            };
            let (family, cause) = classify_wait_locked(&state, record.id);
            quiescent &= family != RuntimeWaitFamilySnapshot::InProgress;
            waits.push(RuntimeWaitSnapshot {
                work,
                family,
                cause,
                awaited: awaited.contains(&record.id),
            });
        }
        waits.sort_by_key(|wait| wait.work.work.get());
        RuntimeAnalysisSnapshot { quiescent, waits }
    }
}

/// The family of `work`'s wait, following what it waits on, from producer
/// to producer, to where the chain ends: at work in progress, or at work
/// that waits on no other's. A wait on another's value is owed, whatever
/// that other waits on, so it is a promise wait naming where the chain
/// ends, unless the chain is in progress.
fn classify_wait_locked(
    state: &WorkCoordinatorState,
    work: EvaluationWorkId,
) -> (RuntimeWaitFamilySnapshot, Option<EvaluationWorkId>) {
    let mut visited = HashSet::new();
    let mut current = work;
    let mut cause = None;
    loop {
        visited.insert(current);
        let Some(record) = state.work.get(&current) else {
            return (RuntimeWaitFamilySnapshot::PromiseWait, cause);
        };
        let producer = current != work;
        match record.state {
            WorkState::Queued
            | WorkState::Running
            | WorkState::InlineForced
            | WorkState::Terminalizing
            | WorkState::Reserved
                if producer =>
            {
                return (RuntimeWaitFamilySnapshot::InProgress, cause);
            }
            // A dormant producer runs once its demander's chain reaches it.
            WorkState::Dormant
                if producer
                    && matches!(record.kind, WorkKind::Deferred(_) | WorkKind::LazyRoute(_)) =>
            {
                return (RuntimeWaitFamilySnapshot::InProgress, cause);
            }
            WorkState::Blocked => {}
            _ if producer => return (RuntimeWaitFamilySnapshot::PromiseWait, cause),
            _ => return (RuntimeWaitFamilySnapshot::Idle, None),
        }
        if !producer && task_block(record).is_some_and(|block| block.error.is_some()) {
            return (RuntimeWaitFamilySnapshot::ErredWait, None);
        }
        let Some(dependency) = work_dependency(record) else {
            return if producer {
                (RuntimeWaitFamilySnapshot::PromiseWait, cause)
            } else {
                (RuntimeWaitFamilySnapshot::Idle, None)
            };
        };
        match dependency_edge_locked(state, dependency) {
            // The runtime fails a cycle among evaluations, so no chain
            // returns to itself; should one, it ends there.
            DependencyEdge::Producer(next) if visited.contains(&next) => {
                return (RuntimeWaitFamilySnapshot::PromiseWait, Some(next));
            }
            DependencyEdge::Producer(next) => {
                cause = Some(next);
                current = next;
            }
            DependencyEdge::WakePending => {
                return (RuntimeWaitFamilySnapshot::InProgress, cause);
            }
            DependencyEdge::End => return (RuntimeWaitFamilySnapshot::PromiseWait, cause),
        }
    }
}
