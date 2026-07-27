use std::sync::Arc;

use crate::api::Value;
use crate::reflection::{
    CommitResult, ExactConflictAnalysis, HostSnapshot, ReflectionStore, StoreSnapshot, TaskCommit,
    TaskEnvironment, TaskHost,
};

use super::effects::MacroEffects;

#[derive(Clone)]
pub(super) struct MacroSnapshot {
    pub(super) environment: Value,
}

#[derive(Clone, Default)]
pub(super) struct MacroJournal {
    diagnostics: Arc<Vec<crate::api::Diagnostic>>,
    pub(super) active_cases: Vec<Value>,
    pub(super) visited_cases: Vec<Value>,
}

impl MacroJournal {
    pub(super) fn push_diagnostic(&mut self, diagnostic: crate::api::Diagnostic) {
        Arc::make_mut(&mut self.diagnostics).push(diagnostic);
    }

    pub(super) fn diagnostics(&self) -> &[crate::api::Diagnostic] {
        &self.diagnostics
    }
}

pub(super) struct MacroHost {
    snapshot: MacroSnapshot,
    store: StoreSnapshot,
}

impl MacroHost {
    pub(super) fn new(environment: Value) -> Self {
        Self {
            snapshot: MacroSnapshot {
                environment: environment.clone(),
            },
            store: ReflectionStore::new(Arc::new(ExactConflictAnalysis)).snapshot(),
        }
    }
}

impl TaskEnvironment for MacroHost {
    fn reflection_environment(&self) -> Value {
        self.snapshot.environment.clone()
    }
}

impl TaskHost<MacroEffects> for MacroHost {
    fn snapshot(&self) -> HostSnapshot<MacroEffects> {
        HostSnapshot::new(1, self.store.clone(), self.snapshot.clone())
    }

    fn commit(&self, _commit: TaskCommit<MacroEffects>) -> CommitResult {
        // The all-results runner owns the outer transaction. Macro journals
        // are selected explicitly and never commit through the host.
        CommitResult::Closed
    }

    fn wait_for_change(&self, _observed_generation: u64) -> bool {
        false
    }
}
