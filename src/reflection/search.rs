use std::sync::Arc;

use super::{
    CommitResult, CoreValueFactory, Diagnostic, EffectTask, EffectTaskPoll, EvalContext,
    ExactConflictAnalysis, HostSnapshot, PublicValue, ReflectionServices, ReflectionStore,
    StoreSnapshot, TaskCommit, TaskEnvironment, TaskHalt, TaskHost, TaskSpecialization,
};

/// Immutable host for one all-results effect search.
///
/// Isolated searches retain their branch journals as results, so this host has
/// no commit or mutable-observation path of its own.
pub(crate) struct IsolatedTaskHost<X> {
    environment: PublicValue,
    store: StoreSnapshot,
    extra: X,
}

impl<X> IsolatedTaskHost<X> {
    pub(crate) fn new(values: CoreValueFactory, environment: PublicValue, extra: X) -> Self {
        Self {
            environment,
            store: ReflectionStore::new(values, Arc::new(ExactConflictAnalysis)).snapshot(),
            extra,
        }
    }
}

impl<X> TaskEnvironment for IsolatedTaskHost<X>
where
    X: Send + Sync,
{
    fn reflection_environment(&self) -> PublicValue {
        self.environment.clone()
    }
}

impl<X> ReflectionServices for IsolatedTaskHost<X>
where
    X: Send + Sync,
{
    fn emit_diagnostic(&self, _diagnostic: Diagnostic) {
        // Isolated `.log` operations are retained in their branch journal;
        // this host has no committed diagnostic destination.
    }
}

impl<S, X> TaskHost<S> for IsolatedTaskHost<X>
where
    S: TaskSpecialization<Snapshot = X>,
    X: Clone + Send + Sync + 'static,
{
    fn snapshot(&self) -> HostSnapshot<S> {
        HostSnapshot::new(1, self.store.clone(), self.extra.clone())
    }

    fn commit(&self, _commit: TaskCommit<S>) -> CommitResult {
        CommitResult::Closed
    }

    fn wait_for_change(&self, _observed_generation: u64) -> bool {
        false
    }
}

/// Selects how terminal branches at the outer effect boundary are handled.
///
/// Ordinary reflection execution preserves the language rule that choice must
/// be scoped by an explicit cut. Isolated search instead retains every
/// terminal outer branch as evidence and never commits its transaction to the
/// host.
pub(super) enum SearchPolicy<B, R> {
    FirstSuccess,
    RetainAll(AllResults<B, R>),
}

impl<B, R> SearchPolicy<B, R> {
    pub(super) fn retaining_all(root: B) -> Self {
        Self::RetainAll(AllResults {
            root,
            alternatives: Vec::new(),
            results: Vec::new(),
            completed: None,
        })
    }

    pub(super) fn retains_all(&self) -> bool {
        matches!(self, Self::RetainAll(_))
    }

    pub(super) fn fork(&mut self, left: B, right: B) -> Option<B> {
        let Self::RetainAll(search) = self else {
            return None;
        };
        search.alternatives.push(right);
        Some(left)
    }

    pub(super) fn retain(&mut self, result: R) {
        let Self::RetainAll(search) = self else {
            panic!("only all-results search can retain terminal branches");
        };
        search.results.push(result);
    }

    pub(super) fn next_alternative(&mut self) -> Option<B> {
        match self {
            Self::FirstSuccess => None,
            Self::RetainAll(search) => search.alternatives.pop(),
        }
    }

    pub(super) fn finish(&mut self) {
        let Self::RetainAll(search) = self else {
            panic!("only all-results search can finish a result collection");
        };
        debug_assert!(search.alternatives.is_empty());
        debug_assert!(search.completed.is_none());
        search.completed = Some(Arc::from(std::mem::take(&mut search.results)));
    }

    pub(super) fn completed(&self) -> Option<Arc<[R]>> {
        match self {
            Self::FirstSuccess => None,
            Self::RetainAll(search) => search.completed.clone(),
        }
    }

    /// Drops every branch-local result and alternative while retaining the
    /// immutable root needed to restart an isolated search.
    pub(super) fn discard_progress(&mut self) {
        let Self::RetainAll(search) = self else {
            return;
        };
        search.alternatives.clear();
        search.results.clear();
        search.completed = None;
    }
}

impl<B: Clone, R> SearchPolicy<B, R> {
    pub(super) fn restart(&mut self) -> Option<B> {
        let Self::RetainAll(search) = self else {
            return None;
        };
        search.alternatives.clear();
        search.results.clear();
        search.completed = None;
        Some(search.root.clone())
    }
}

pub(super) struct AllResults<B, R> {
    root: B,
    alternatives: Vec<B>,
    results: Vec<R>,
    completed: Option<Arc<[R]>>,
}

#[doc(hidden)]
pub struct IsolatedSearchBranch<S: TaskSpecialization> {
    value: Option<PublicValue>,
    transaction: TaskCommit<S>,
}

impl<S: TaskSpecialization> IsolatedSearchBranch<S> {
    pub(super) fn complete(value: PublicValue, transaction: TaskCommit<S>) -> Self {
        Self {
            value: Some(value),
            transaction,
        }
    }

    pub(super) fn failed(transaction: TaskCommit<S>) -> Self {
        Self {
            value: None,
            transaction,
        }
    }

    pub fn value(&self) -> Option<&PublicValue> {
        self.value.as_ref()
    }

    pub fn journal(&self) -> &S::Journal {
        self.transaction.extra()
    }
}

#[doc(hidden)]
pub struct IsolatedSearchBlock {
    dependency: Option<super::EvaluationWaitToken>,
    observed_generation: Option<u64>,
    error: Option<TaskHalt>,
}

impl IsolatedSearchBlock {
    pub fn waiting_on_dependency(&self) -> bool {
        self.dependency.is_some()
    }

    pub(crate) fn dependency(&self) -> Option<&super::EvaluationWaitToken> {
        self.dependency.as_ref()
    }

    pub fn observed_generation(&self) -> Option<u64> {
        self.observed_generation
    }

    pub fn error(&self) -> Option<&TaskHalt> {
        self.error.as_ref()
    }
}

#[doc(hidden)]
pub enum IsolatedSearchPoll<S: TaskSpecialization> {
    Yielded,
    Blocked(IsolatedSearchBlock),
    Complete(Arc<[IsolatedSearchBranch<S>]>),
    Failed(TaskHalt),
    Cancelled,
}

/// Pollable all-results execution shared by configured CLI and token parsing,
/// macro expansion, interaction-net construction, and policy tests.
/// Successful and failed branch journals remain isolated from the host.
#[doc(hidden)]
pub struct IsolatedEffectSearch<S: TaskSpecialization> {
    task: EffectTask<S>,
    _owner: Option<Arc<super::super::evaluation::EvaluationSession>>,
}

impl<S: TaskSpecialization> IsolatedEffectSearch<S> {
    pub fn new(
        runtime: &crate::api::EvaluationRuntime,
        effect: &PublicValue,
        specialization: S,
        host: Arc<S::Host>,
    ) -> Result<Self, TaskHalt> {
        let owner = runtime.new_evaluation_session()?;
        let context = EvalContext::new(&owner);
        let mut search = Self::new_in_context(effect, specialization, host, context)?;
        search._owner = Some(owner);
        Ok(search)
    }

    pub(crate) fn new_in_context(
        effect: &PublicValue,
        specialization: S,
        host: Arc<S::Host>,
        context: EvalContext,
    ) -> Result<Self, TaskHalt> {
        Ok(Self {
            task: EffectTask::new_isolated_in_context(
                effect.as_core().clone(),
                specialization,
                host,
                context,
            )?,
            _owner: None,
        })
    }

    pub fn poll(&mut self, step_budget: usize) -> IsolatedSearchPoll<S> {
        match self.task.poll(step_budget) {
            EffectTaskPoll::Yielded => IsolatedSearchPoll::Yielded,
            EffectTaskPoll::Blocked(blocked) => IsolatedSearchPoll::Blocked(IsolatedSearchBlock {
                dependency: blocked.lazy,
                observed_generation: blocked.observed_generation,
                error: blocked.error.map(TaskHalt::failure),
            }),
            EffectTaskPoll::Complete(_) => {
                let results = self
                    .task
                    .completed_search()
                    .expect("isolated search completion must retain its branch results");
                IsolatedSearchPoll::Complete(results)
            }
            EffectTaskPoll::Failed(error) => IsolatedSearchPoll::Failed(error),
            EffectTaskPoll::Cancelled => IsolatedSearchPoll::Cancelled,
            EffectTaskPoll::Exit(_) => {
                unreachable!("isolated effect-search profiles do not expose runtime exit")
            }
        }
    }

    pub fn cancel(&mut self) {
        self.task.finish(super::TaskTerminal::Cancelled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Dict, Value};
    use crate::reflection::{StandardEffects, StoreJournal};

    #[test]
    fn isolated_task_host_has_one_immutable_non_committing_snapshot() {
        let values = crate::core::test_value_factory();
        let environment = PublicValue::from_core(&values, Value::Dict(Dict::new_sync()));
        let host = IsolatedTaskHost::new(values, environment.clone(), ());
        let snapshot = <IsolatedTaskHost<()> as TaskHost<StandardEffects>>::snapshot(&host);

        assert_eq!(snapshot.generation(), 1);
        assert_eq!(snapshot.extra(), &());
        assert_eq!(
            host.reflection_environment().as_core(),
            environment.as_core()
        );
        assert!(!<IsolatedTaskHost<()> as TaskHost<StandardEffects>>::wait_for_change(&host, 1));

        let commit = TaskCommit::new(StoreJournal::new(snapshot.store().clone()), (), ());
        assert_eq!(
            <IsolatedTaskHost<()> as TaskHost<StandardEffects>>::commit(&host, commit),
            CommitResult::Closed
        );
    }
}
