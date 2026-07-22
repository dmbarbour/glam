use std::sync::Arc;

use super::{
    EffectTask, EffectTaskPoll, EvalContext, PublicValue, TaskCommit, TaskError, TaskSpecialization,
};

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
    error: Option<Arc<str>>,
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

    pub fn error(&self) -> Option<&Arc<str>> {
        self.error.as_ref()
    }
}

#[doc(hidden)]
pub enum IsolatedSearchPoll<S: TaskSpecialization> {
    Yielded,
    Blocked(IsolatedSearchBlock),
    Complete(Arc<[IsolatedSearchBranch<S>]>),
    Failed(TaskError),
    Cancelled,
}

/// Pollable all-results execution used by policy tests and, later, configured
/// CLI parsing. Successful and failed branch journals remain isolated from the
/// host.
#[doc(hidden)]
pub struct IsolatedEffectSearch<S: TaskSpecialization> {
    task: EffectTask<S>,
}

impl<S: TaskSpecialization> IsolatedEffectSearch<S> {
    pub fn new(
        effect: &PublicValue,
        specialization: S,
        host: Arc<S::Host>,
    ) -> Result<Self, TaskError> {
        Self::new_in_context(effect, specialization, host, EvalContext::standalone())
    }

    pub(crate) fn new_in_context(
        effect: &PublicValue,
        specialization: S,
        host: Arc<S::Host>,
        context: EvalContext,
    ) -> Result<Self, TaskError> {
        Ok(Self {
            task: EffectTask::new_isolated_in_context(
                effect.as_core().clone(),
                specialization,
                host,
                context,
            )?,
        })
    }

    pub fn poll(&mut self, step_budget: usize) -> IsolatedSearchPoll<S> {
        match self.task.poll(step_budget) {
            EffectTaskPoll::Yielded => IsolatedSearchPoll::Yielded,
            EffectTaskPoll::Blocked(blocked) => IsolatedSearchPoll::Blocked(IsolatedSearchBlock {
                dependency: blocked.lazy,
                observed_generation: blocked.observed_generation,
                error: blocked.error,
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
        }
    }

    pub fn cancel(&mut self) {
        self.task.finish(super::TaskTerminal::Cancelled);
    }
}
