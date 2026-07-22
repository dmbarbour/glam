use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::api::{Diagnostic, Value};
use crate::reflection::{
    CommitResult, ExactConflictAnalysis, HostSnapshot, ReflectionJournal, ReflectionServices,
    ReflectionStore, ReflectionTransaction, StoreSnapshot, TaskCommit, TaskEnvironment, TaskHost,
};

use super::completion::{CompletionEvidence, ExpectationEvidence};
use super::effects::CliEffects;
use super::model::CommandEdit;

static NEXT_CLI_INVOCATION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(super) struct CliInvocation {
    pub(super) id: u64,
    pub(super) args: Arc<[OsString]>,
    pub(super) completion: Option<CompletionPoint>,
}

impl CliInvocation {
    pub(super) fn new(args: Arc<[OsString]>) -> Self {
        Self::from_parts(args, None)
    }

    pub(super) fn for_completion(
        args: Arc<[OsString]>,
        argument: usize,
        prefix: OsString,
        suffix: OsString,
    ) -> Self {
        Self::from_parts(
            args,
            Some(CompletionPoint {
                argument,
                prefix,
                suffix,
            }),
        )
    }

    fn from_parts(args: Arc<[OsString]>, completion: Option<CompletionPoint>) -> Self {
        let id = NEXT_CLI_INVOCATION_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("CLI invocation IDs exhausted");
        Self {
            id,
            args,
            completion,
        }
    }
}

#[derive(Clone)]
pub(super) struct CompletionPoint {
    pub(super) argument: usize,
    pub(super) prefix: OsString,
    pub(super) suffix: OsString,
}

#[derive(Clone)]
pub(super) struct CliSnapshot {
    pub(super) invocation: CliInvocation,
}

#[derive(Clone, Default)]
pub(super) struct CliJournal {
    pub(super) reflection: ReflectionJournal,
    pub(super) cursor: usize,
    pub(super) edits: Vec<CommandEdit>,
    pub(super) expectations: Vec<ExpectationEvidence>,
    pub(super) candidates: Vec<CompletionEvidence>,
    /// Cases currently enclosing the effect being interpreted. Failed reader
    /// evidence captures this stack before the branch terminates.
    pub(super) active_cases: Vec<Value>,
    /// Cases entered by this branch, retained for ambiguity explanations after
    /// successful scopes have closed.
    pub(super) visited_cases: Vec<Value>,
}

impl ReflectionTransaction for CliJournal {
    fn reflection_journal(&mut self) -> &mut ReflectionJournal {
        &mut self.reflection
    }
}

pub(super) struct CliHost {
    environment: Value,
    snapshot: CliSnapshot,
    store: StoreSnapshot,
}

impl CliHost {
    pub(super) fn new(environment: Value, invocation: CliInvocation) -> Self {
        Self {
            environment,
            snapshot: CliSnapshot { invocation },
            store: ReflectionStore::new(Arc::new(ExactConflictAnalysis)).snapshot(),
        }
    }
}

impl TaskEnvironment for CliHost {
    fn reflection_environment(&self) -> Value {
        self.environment.clone()
    }
}

impl ReflectionServices for CliHost {
    fn emit_diagnostic(&self, _diagnostic: Diagnostic) {
        // CLI `.log` is always journaled by the isolated outer search.
    }

    fn update_query(
        &self,
        _handle: &Arc<crate::reflection::EvaluationQueryHandle>,
        _result: Value,
    ) {
        unreachable!("the CLI effect API does not expose task queries")
    }
}

impl TaskHost<CliEffects> for CliHost {
    fn snapshot(&self) -> HostSnapshot<CliEffects> {
        HostSnapshot::new(1, self.store.clone(), self.snapshot.clone())
    }

    fn commit(&self, _commit: TaskCommit<CliEffects>) -> CommitResult {
        // The all-results runner owns the only outer transaction and never
        // commits it. Returning Closed keeps accidental escapes non-mutating.
        CommitResult::Closed
    }

    fn wait_for_change(&self, _observed_generation: u64) -> bool {
        false
    }
}
