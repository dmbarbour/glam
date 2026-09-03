use std::sync::Arc;

use super::machine::{EffectTask, EffectTaskPoll, TaskTerminal};
use super::protocol::{
    CommitResult, HostSnapshot, TaskCommit, TaskEnvironment, TaskHalt, TaskHost, TaskSpecialization,
};
use super::requests::ReflectionServices;
use super::store::{ExactConflictAnalysis, ReflectionStore, StoreSnapshot};
use crate::api::{Diagnostic, Value as PublicValue, Values};
use crate::core::CoreValueFactory;
use crate::evaluation::{EvalContext, EvaluationWaitToken};

/// Immutable host for one all-results effect search.
///
/// Isolated searches retain their branch journals as results, so this host has
/// no commit or mutable-observation path of its own.
pub struct IsolatedTaskHost<X> {
    environment: PublicValue,
    store: StoreSnapshot,
    extra: X,
}

impl<X> IsolatedTaskHost<X> {
    pub fn new(
        values: &crate::api::Values,
        environment: PublicValue,
        extra: X,
    ) -> Result<Self, crate::api::Error> {
        if environment.runtime_id() != values.runtime_id() {
            return Err(crate::api::Error::new(
                "isolated task environment belongs to another runtime",
            ));
        }
        Ok(Self {
            environment,
            store: ReflectionStore::new(values.core().clone(), Arc::new(ExactConflictAnalysis))
                .snapshot(),
            extra,
        })
    }

    pub(crate) fn new_core(values: CoreValueFactory, environment: PublicValue, extra: X) -> Self {
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
    dependency: Option<EvaluationWaitToken>,
    observed_generation: Option<u64>,
    error: Option<TaskHalt>,
}

impl IsolatedSearchBlock {
    pub fn waiting_on_dependency(&self) -> bool {
        self.dependency.is_some()
    }

    pub(crate) fn dependency(&self) -> Option<&EvaluationWaitToken> {
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
        let runtime = context.values().runtime_id();
        let values = Values::from_core_factory(context.values().clone());
        let effect = values.clone_core(effect)?;
        EffectTask::new_isolated_in_context(effect, specialization, host, context)
            .map(|task| Self { task, _owner: None })
            .map_err(|error| error.root_for_runtime(runtime))
    }

    fn root_poll_error(&self, error: TaskHalt) -> TaskHalt {
        error.root_for_runtime(self.task.eval_context.values().runtime_id())
    }

    pub fn poll(&mut self, step_budget: usize) -> IsolatedSearchPoll<S> {
        match self.task.poll(step_budget) {
            EffectTaskPoll::Yielded => IsolatedSearchPoll::Yielded,
            EffectTaskPoll::Blocked(blocked) => {
                let error = blocked.error.map(TaskHalt::rooted_failure);
                IsolatedSearchPoll::Blocked(IsolatedSearchBlock {
                    dependency: blocked.lazy,
                    observed_generation: blocked.observed_generation,
                    error,
                })
            }
            EffectTaskPoll::Complete(_) => {
                let results = self
                    .task
                    .completed_search()
                    .expect("isolated search completion must retain its branch results");
                IsolatedSearchPoll::Complete(results)
            }
            EffectTaskPoll::Failed(error) => {
                IsolatedSearchPoll::Failed(self.root_poll_error(error))
            }
            EffectTaskPoll::Cancelled => IsolatedSearchPoll::Cancelled,
            EffectTaskPoll::Exit(_) => {
                unreachable!("isolated effect-search profiles do not expose runtime exit")
            }
        }
    }

    pub fn cancel(&mut self) {
        self.task.finish(TaskTerminal::Cancelled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EffectTokenDomain, Values};
    use crate::reflection::{StandardEffects, StoreJournal};
    use std::convert::Infallible;
    use std::sync::Weak;

    #[derive(Clone, Copy)]
    struct SearchRootTestEffects;

    impl TaskSpecialization for SearchRootTestEffects {
        type Host = dyn TaskHost<Self>;
        type Request = Infallible;
        type Snapshot = PublicValue;
        type Journal = Vec<PublicValue>;

        fn requests(&self) -> Vec<super::super::protocol::EffectRequestSpec<Self::Request>> {
            Vec::new()
        }

        fn handle_request(
            &self,
            request: Self::Request,
            _arguments: Vec<PublicValue>,
            _context: &mut super::super::protocol::RequestContext<'_, Self>,
        ) -> Result<super::super::protocol::RequestResult, TaskHalt> {
            match request {}
        }
    }

    fn assert_isolated_host_inventory(host: &IsolatedTaskHost<PublicValue>) {
        let IsolatedTaskHost {
            environment,
            store,
            extra,
        } = host;
        let _: &PublicValue = environment;
        let _: &StoreSnapshot = store;
        let _: &PublicValue = extra;
    }

    fn assert_search_policy_inventory(policy: &SearchPolicy<PublicValue, PublicValue>) {
        match policy {
            SearchPolicy::FirstSuccess => {}
            SearchPolicy::RetainAll(AllResults {
                root,
                alternatives,
                results,
                completed,
            }) => {
                let _: &PublicValue = root;
                let _: &Vec<PublicValue> = alternatives;
                let _: &Vec<PublicValue> = results;
                let _: &Option<Arc<[PublicValue]>> = completed;
            }
        }
    }

    fn assert_search_result_inventory(
        branch: &IsolatedSearchBranch<SearchRootTestEffects>,
        block: &IsolatedSearchBlock,
        poll: &IsolatedSearchPoll<SearchRootTestEffects>,
    ) {
        let IsolatedSearchBranch { value, transaction } = branch;
        let _: &Option<PublicValue> = value;
        let _: &TaskCommit<SearchRootTestEffects> = transaction;

        let IsolatedSearchBlock {
            dependency,
            observed_generation,
            error,
        } = block;
        let _: &Option<EvaluationWaitToken> = dependency;
        let _: &Option<u64> = observed_generation;
        let _: &Option<TaskHalt> = error;

        match poll {
            IsolatedSearchPoll::Yielded | IsolatedSearchPoll::Cancelled => {}
            IsolatedSearchPoll::Blocked(block) => {
                let _: &IsolatedSearchBlock = block;
            }
            IsolatedSearchPoll::Complete(branches) => {
                let _: &Arc<[IsolatedSearchBranch<SearchRootTestEffects>]> = branches;
            }
            IsolatedSearchPoll::Failed(error) => {
                let _: &TaskHalt = error;
            }
        }
    }

    fn assert_effect_search_inventory(search: &IsolatedEffectSearch<SearchRootTestEffects>) {
        let IsolatedEffectSearch { task, _owner } = search;
        let _: &EffectTask<SearchRootTestEffects> = task;
        let _: &Option<Arc<super::super::super::evaluation::EvaluationSession>> = _owner;
    }

    #[test]
    fn isolated_search_root_inventory_is_complete() {
        let _: fn(&IsolatedTaskHost<PublicValue>) = assert_isolated_host_inventory;
        let _: fn(&SearchPolicy<PublicValue, PublicValue>) = assert_search_policy_inventory;
        let _: fn(
            &IsolatedSearchBranch<SearchRootTestEffects>,
            &IsolatedSearchBlock,
            &IsolatedSearchPoll<SearchRootTestEffects>,
        ) = assert_search_result_inventory;
        let _: fn(&IsolatedEffectSearch<SearchRootTestEffects>) = assert_effect_search_inventory;
    }

    fn retained_search_value(domain: &EffectTokenDomain<Arc<()>>) -> (PublicValue, Weak<()>) {
        let payload = Arc::new(());
        let retained = Arc::downgrade(&payload);
        (domain.issue(payload), retained)
    }

    fn search_store(values: &Values) -> ReflectionStore {
        ReflectionStore::new(values.core().clone(), Arc::new(ExactConflictAnalysis))
    }

    fn search_commit(
        store: &ReflectionStore,
        snapshot: PublicValue,
        journal: PublicValue,
    ) -> TaskCommit<SearchRootTestEffects> {
        TaskCommit::new(StoreJournal::new(store.snapshot()), snapshot, vec![journal])
    }

    #[test]
    fn isolated_host_and_branches_retain_roots_until_retirement() {
        let core = crate::core::test_value_factory();
        let values = Values::from_core_factory(core);
        let domain = EffectTokenDomain::new(&values);
        let store = search_store(&values);

        let (environment, retained_environment) = retained_search_value(&domain);
        let (extra, retained_extra) = retained_search_value(&domain);
        let host = IsolatedTaskHost::new(&values, environment, extra)
            .expect("same-runtime roots should construct an isolated host");
        assert!(retained_environment.upgrade().is_some());
        assert!(retained_extra.upgrade().is_some());
        drop(host);
        assert!(retained_environment.upgrade().is_none());
        assert!(retained_extra.upgrade().is_none());

        let (result, retained_result) = retained_search_value(&domain);
        let (snapshot, retained_snapshot) = retained_search_value(&domain);
        let (journal, retained_journal) = retained_search_value(&domain);
        let branch =
            IsolatedSearchBranch::complete(result, search_commit(&store, snapshot, journal));
        assert!(retained_result.upgrade().is_some());
        assert!(retained_snapshot.upgrade().is_some());
        assert!(retained_journal.upgrade().is_some());
        drop(branch);
        assert!(retained_result.upgrade().is_none());
        assert!(retained_snapshot.upgrade().is_none());
        assert!(retained_journal.upgrade().is_none());

        let (snapshot, retained_snapshot) = retained_search_value(&domain);
        let (journal, retained_journal) = retained_search_value(&domain);
        let branch = IsolatedSearchBranch::failed(search_commit(&store, snapshot, journal));
        assert!(retained_snapshot.upgrade().is_some());
        assert!(retained_journal.upgrade().is_some());
        drop(branch);
        assert!(retained_snapshot.upgrade().is_none());
        assert!(retained_journal.upgrade().is_none());
    }

    #[test]
    fn search_policy_discards_progress_but_returned_results_own_their_roots() {
        let core = crate::core::test_value_factory();
        let values = Values::from_core_factory(core);
        let domain = EffectTokenDomain::new(&values);
        let (root, retained_root) = retained_search_value(&domain);
        let (left, retained_left) = retained_search_value(&domain);
        let (right, retained_right) = retained_search_value(&domain);
        let (result, retained_result) = retained_search_value(&domain);
        let mut policy = SearchPolicy::retaining_all(root);

        drop(
            policy
                .fork(left, right)
                .expect("all-results policy should select its left branch"),
        );
        policy.retain(result);
        assert!(retained_left.upgrade().is_none());
        assert!(retained_right.upgrade().is_some());
        assert!(retained_result.upgrade().is_some());
        assert!(retained_root.upgrade().is_some());

        policy.discard_progress();
        assert!(retained_right.upgrade().is_none());
        assert!(retained_result.upgrade().is_none());
        assert!(retained_root.upgrade().is_some());

        let (completed, retained_completed) = retained_search_value(&domain);
        policy.retain(completed);
        policy.finish();
        let returned = policy
            .completed()
            .expect("finished search should publish its result collection");
        drop(policy);
        assert!(retained_root.upgrade().is_none());
        assert!(retained_completed.upgrade().is_some());
        drop(returned);
        assert!(retained_completed.upgrade().is_none());
    }

    #[test]
    fn blocked_search_error_retains_a_runtime_root_until_retirement() {
        let core = crate::core::test_value_factory();
        let values = Values::from_core_factory(core);
        let domain = EffectTokenDomain::new(&values);
        let (context, retained) = retained_search_value(&domain);
        let error = TaskHalt::new("retryable search failure").with_context(&values, context);
        let block = IsolatedSearchBlock {
            dependency: None,
            observed_generation: Some(1),
            error: Some(error),
        };
        assert!(
            block.error().and_then(TaskHalt::failure_root).is_some(),
            "a published blocked error must retain its explicit runtime root"
        );
        assert!(retained.upgrade().is_some());
        drop(block);
        assert!(retained.upgrade().is_none());
    }

    #[test]
    fn isolated_task_host_has_one_immutable_non_committing_snapshot() {
        let values = crate::core::test_value_factory();
        let public_values = Values::from_core_factory(values.clone());
        let environment = public_values.empty_dict();
        let host = IsolatedTaskHost::new_core(values, environment.clone(), ());
        let snapshot = <IsolatedTaskHost<()> as TaskHost<StandardEffects>>::snapshot(&host);

        assert_eq!(snapshot.generation(), 1);
        assert_eq!(snapshot.extra(), &());
        assert_eq!(
            public_values
                .clone_core(&host.reflection_environment())
                .expect("host environment belongs to the search runtime"),
            public_values
                .clone_core(&environment)
                .expect("test environment belongs to the search runtime")
        );
        assert!(!<IsolatedTaskHost<()> as TaskHost<StandardEffects>>::wait_for_change(&host, 1));

        let commit = TaskCommit::new(StoreJournal::new(snapshot.store().clone()), (), ());
        assert_eq!(
            <IsolatedTaskHost<()> as TaskHost<StandardEffects>>::commit(&host, commit),
            CommitResult::Closed
        );
    }
}
