//! Runtime-owned work coordination independent of worker ownership.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, Weak};

use crate::runtime::RuntimeMutationAdmission;

use super::EvaluationSession;

#[derive(Default)]
struct WorkCoordinatorState {
    sessions: HashMap<u64, Weak<EvaluationSession>>,
    ready_sessions: VecDeque<u64>,
    ready_session_set: HashSet<u64>,
    prefer_spark: bool,
    work_generation: u64,
}

/// Runtime-owned scheduling state shared by serial and worker execution.
///
/// Phase 3A moves only demand-session registration, ready-session selection,
/// fairness, and waiting here. Active task records remain session-owned and
/// spark payloads remain executor-owned until their later migration phases.
pub(crate) struct EvaluationWorkCoordinator {
    admission: Arc<RuntimeMutationAdmission>,
    state: Mutex<WorkCoordinatorState>,
    work_available: Condvar,
}

pub(super) enum CoordinatorSelection {
    Reflection(Arc<EvaluationSession>),
    Spark,
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
            .field("session_count", &state.sessions.len())
            .field("ready_session_count", &state.ready_session_set.len())
            .field("work_generation", &state.work_generation)
            .finish_non_exhaustive()
    }
}

impl EvaluationWorkCoordinator {
    pub(crate) fn new(admission: Arc<RuntimeMutationAdmission>) -> Arc<Self> {
        Arc::new(Self {
            admission,
            state: Mutex::new(WorkCoordinatorState::default()),
            work_available: Condvar::new(),
        })
    }

    pub(super) fn register_session(&self, session: &Arc<EvaluationSession>) {
        self.publish_transition(|state| {
            let replaced = state
                .sessions
                .insert(session.id.get(), Arc::downgrade(session));
            assert!(
                replaced.is_none(),
                "evaluation session identities must be unique within a runtime"
            );
        });
    }

    pub(super) fn unregister_session(&self, session: u64) {
        self.publish_transition(|state| {
            state.sessions.remove(&session);
            state.ready_session_set.remove(&session);
            state
                .ready_sessions
                .retain(|candidate| *candidate != session);
        });
    }

    pub(super) fn contains_session(&self, session: u64) -> bool {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .sessions
            .contains_key(&session)
    }

    pub(super) fn notify_session_ready(&self, session: u64) {
        let mutation = self.admission.mutation_guard();
        let changed = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            if !state.sessions.contains_key(&session) || !state.ready_session_set.insert(session) {
                false
            } else {
                state.ready_sessions.push_back(session);
                state.work_generation = state.work_generation.wrapping_add(1);
                true
            }
        };
        drop(mutation);
        if changed {
            self.work_available.notify_one();
        }
    }

    /// Records a readiness change in executor-owned transitional work.
    ///
    /// Spark payloads remain outside the coordinator in Phase 3A, but their
    /// admission, parking, disturbance, and shutdown must wake workers waiting
    /// on the coordinator's one condition variable.
    pub(super) fn notify_external_work_changed(&self) {
        self.publish_transition(|_| {});
    }

    pub(super) fn work_generation(&self) -> u64 {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .work_generation
    }

    pub(super) fn select(&self, spark_available: bool) -> CoordinatorSelection {
        let mutation = self.admission.mutation_guard();
        let (selection, changed) = {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            let initial_generation = state.work_generation;
            let had_ready_session = !state.ready_sessions.is_empty();
            let selection = if state.prefer_spark && spark_available {
                state.prefer_spark = false;
                CoordinatorSelection::Spark
            } else if let Some(session) = pop_ready_session(&mut state) {
                state.prefer_spark = true;
                CoordinatorSelection::Reflection(session)
            } else if spark_available {
                state.prefer_spark = false;
                CoordinatorSelection::Spark
            } else {
                CoordinatorSelection::None
            };
            if !matches!(selection, CoordinatorSelection::None) || had_ready_session {
                state.work_generation = state.work_generation.wrapping_add(1);
            }
            (selection, state.work_generation != initial_generation)
        };
        drop(mutation);
        if changed {
            self.work_available.notify_all();
        }
        selection
    }

    pub(super) fn wait_for_change(&self, observed_generation: u64) {
        let mut state = self
            .state
            .lock()
            .expect("evaluation work coordinator was poisoned");
        while state.work_generation == observed_generation {
            state = self
                .work_available
                .wait(state)
                .expect("evaluation work coordinator was poisoned");
        }
    }

    fn publish_transition(&self, transition: impl FnOnce(&mut WorkCoordinatorState)) {
        let mutation = self.admission.mutation_guard();
        {
            let mut state = self
                .state
                .lock()
                .expect("evaluation work coordinator was poisoned");
            transition(&mut state);
            state.work_generation = state.work_generation.wrapping_add(1);
        }
        drop(mutation);
        self.work_available.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn registered_session_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .sessions
            .len()
    }

    #[cfg(test)]
    pub(crate) fn ready_session_count(&self) -> usize {
        self.state
            .lock()
            .expect("evaluation work coordinator was poisoned")
            .ready_session_set
            .len()
    }
}

fn pop_ready_session(state: &mut WorkCoordinatorState) -> Option<Arc<EvaluationSession>> {
    while let Some(session_id) = state.ready_sessions.pop_front() {
        state.ready_session_set.remove(&session_id);
        let session = state.sessions.get(&session_id).and_then(Weak::upgrade);
        if session.is_some() {
            return session;
        }
        state.sessions.remove(&session_id);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinator_owns_session_registration_and_ready_selection() {
        let (coordinator, executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = EvaluationSession::shared(&coordinator, &executor);

        assert_eq!(coordinator.registered_session_count(), 1);
        coordinator.notify_session_ready(session.id.get());
        coordinator.notify_session_ready(session.id.get());
        assert_eq!(coordinator.ready_session_count(), 1);

        let CoordinatorSelection::Reflection(selected) = coordinator.select(false) else {
            panic!("the ready session should be selected")
        };
        assert!(Arc::ptr_eq(&selected, &session));
        assert_eq!(coordinator.ready_session_count(), 0);

        drop(selected);
        drop(session);
        assert_eq!(coordinator.registered_session_count(), 0);
    }

    #[test]
    fn coordinator_fairness_alternates_ready_sessions_and_sparks() {
        let (coordinator, executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = EvaluationSession::shared(&coordinator, &executor);

        coordinator.notify_session_ready(session.id.get());
        assert!(matches!(
            coordinator.select(true),
            CoordinatorSelection::Reflection(_)
        ));

        coordinator.notify_session_ready(session.id.get());
        assert!(matches!(
            coordinator.select(true),
            CoordinatorSelection::Spark
        ));
        assert!(matches!(
            coordinator.select(false),
            CoordinatorSelection::Reflection(_)
        ));
    }

    #[test]
    fn dropping_executor_does_not_discard_coordinator_state() {
        let (coordinator, executor) = super::super::test_execution_resources(0)
            .expect("test execution resources should build");
        let session = EvaluationSession::shared(&coordinator, &executor);
        drop(executor);

        coordinator.notify_session_ready(session.id.get());
        assert!(matches!(
            coordinator.select(false),
            CoordinatorSelection::Reflection(_)
        ));
        assert_eq!(coordinator.registered_session_count(), 1);
    }
}
