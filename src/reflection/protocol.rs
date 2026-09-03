use std::convert::Infallible;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;

use super::machine::task_eval_error;
use super::requests::{
    ReflectionHost, ReflectionJournal, ReflectionRequest, handle_reflection_request,
    reflection_request_specs,
};
use super::search::IsolatedEffectSearch;
use super::store::{StoreJournal, StoreSnapshot, VolumeId};
use crate::api::{Diagnostic, Error as ApiError, EvaluatedValue, Value as PublicValue, Values};
use crate::core::{Dict, EvaluationFailure, EvaluationHalt, Key, List, Value};
use crate::core_net::CoreWaitToken;
use crate::diagnostic::Severity;
use crate::eval;
use crate::evaluation::{EvalContext, EvaluationPollContext, EvaluationWaitToken};
use crate::runtime::{EvaluationRuntimeId, RuntimeFailureRoot, RuntimeValueRoot};

/// One additional effect constructor contributed by a task specialization.
pub struct EffectRequestSpec<R> {
    pub(super) api_path: Option<Arc<[Arc<str>]>>,
    pub(super) tag_path: Arc<[Arc<str>]>,
    pub(super) arity: usize,
    pub(super) request: R,
}

impl<R> EffectRequestSpec<R> {
    pub fn new<I, P>(api_name: impl Into<Arc<str>>, tag_path: I, arity: usize, request: R) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<Arc<str>>,
    {
        Self::at_path([api_name], tag_path, arity, request)
    }

    /// Contributes a request constructor at a nested effect API path.
    pub fn at_path<A, P, I, T>(api_path: A, tag_path: I, arity: usize, request: R) -> Self
    where
        A: IntoIterator<Item = P>,
        P: Into<Arc<str>>,
        I: IntoIterator<Item = T>,
        T: Into<Arc<str>>,
    {
        Self {
            api_path: Some(api_path.into_iter().map(Into::into).collect()),
            tag_path: tag_path.into_iter().map(Into::into).collect(),
            arity,
            request,
        }
    }

    /// Registers a request tag without placing its constructor in the effect
    /// API. This is useful for host-owned close operations paired with a
    /// visible scoped request.
    #[doc(hidden)]
    pub fn hidden<I, T>(tag_path: I, arity: usize, request: R) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<Arc<str>>,
    {
        Self {
            api_path: None,
            tag_path: tag_path.into_iter().map(Into::into).collect(),
            arity,
            request,
        }
    }

    pub fn map_request<T>(self, map: impl FnOnce(R) -> T) -> EffectRequestSpec<T> {
        EffectRequestSpec {
            api_path: self.api_path,
            tag_path: self.tag_path,
            arity: self.arity,
            request: map(self.request),
        }
    }

    /// Constructs the constant effect for this registered request.
    ///
    /// This is primarily useful for specialization-owned hidden close
    /// operations paired with [`RequestResult::Scoped`]. The request tag
    /// remains inaccessible to Glam code except through values issued by the
    /// owning Rust specialization.
    pub fn effect(
        &self,
        values: &Values,
        arguments: impl IntoIterator<Item = PublicValue>,
    ) -> Result<PublicValue, ApiError> {
        let arguments = arguments.into_iter().collect::<Vec<_>>();
        if arguments.len() != self.arity {
            return Err(ApiError::new(format!(
                "effect request expects {} arguments, received {}",
                self.arity,
                arguments.len()
            )));
        }
        for argument in &arguments {
            if argument.runtime_id() != values.runtime_id() {
                return Err(ApiError::new(format!(
                    "effect request argument belongs to evaluation runtime {}, expected evaluation runtime {}",
                    argument.runtime_id().get(),
                    values.runtime_id().get()
                )));
            }
        }
        Ok(PublicValue::from_core(
            values.core(),
            eval::constant_effect(
                values.core(),
                request_value(
                    &Key::abstract_global_path(self.tag_path.iter().map(Arc::as_ref)),
                    arguments.into_iter().map(PublicValue::into_core).collect(),
                ),
            ),
        ))
    }
}

/// Result of handling one specialization-owned request.
pub enum RequestResult {
    Return(PublicValue),
    /// Resumes the current continuation once per value, preserving order.
    /// An empty collection is an ordinary effect failure.
    Alternatives(Vec<PublicValue>),
    /// Runs `operation` in the current branch, then runs the unit-returning
    /// `close` effect before delivering the operation's original result.
    /// Failed operation branches deliberately do not run `close`, allowing a
    /// specialization journal to retain scoped failure evidence.
    Scoped {
        operation: PublicValue,
        close: PublicValue,
    },
    ReturnUnit,
    Fail,
    Cancelled,
}

/// Runtime-local identity of one reasoning session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReasoningSessionId(NonZeroU64);

impl ReasoningSessionId {
    pub fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) fn from_u64(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }
}

#[derive(Default)]
pub(super) struct RequestActivity {
    pub(super) observed_generation: Option<u64>,
    pub(super) committed: bool,
}

/// Extra effects and transactional resources available to one task kind.
///
/// A specialization is immutable dispatch policy; mutable resources belong to
/// its [`TaskHost`], so cloning the specialization should remain inexpensive.
pub trait TaskSpecialization: Clone + Sized + Send + Sync + 'static {
    type Host: TaskHost<Self> + ?Sized;
    /// Decoded specialization state. Any semantic value retained here must
    /// use a public/runtime root rather than a bare core value.
    type Request: Clone + Send + Sync + 'static;
    /// Immutable specialization state retained across an optimistic
    /// transaction. Implementations own the exact tracing/root contract for
    /// any semantic values reachable from this type.
    type Snapshot: Clone + Send + Sync + 'static;
    /// Specialization changes retained by an active transaction or published
    /// search result. Implementations own the exact tracing/root contract for
    /// any semantic values reachable from this type.
    type Journal: Clone + Default + Send + Sync + 'static;

    /// Controls whether the shared `.heap.*` family is installed in this
    /// task's effect API. Task-local state and control effects remain standard.
    fn exposes_shared_heap(&self) -> bool {
        true
    }

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>>;

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<PublicValue>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskHalt>;
}

/// A task exposing only the standard effect machine.
#[derive(Clone, Copy, Default)]
pub struct StandardEffects;

impl TaskSpecialization for StandardEffects {
    type Host = dyn TaskHost<Self>;
    type Request = Infallible;
    type Snapshot = ();
    type Journal = ();

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        Vec::new()
    }

    fn handle_request(
        &self,
        request: Self::Request,
        _arguments: Vec<PublicValue>,
        _context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskHalt> {
        match request {}
    }
}

/// Standard control/state effects plus the reusable reflection request family.
#[derive(Clone, Copy, Default)]
pub struct ReflectionEffects;

impl TaskSpecialization for ReflectionEffects {
    type Host = dyn ReflectionHost<Self>;
    type Request = ReflectionRequest;
    type Snapshot = ();
    type Journal = ReflectionJournal;

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        reflection_request_specs()
    }

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<PublicValue>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskHalt> {
        handle_reflection_request(request, arguments, context)
    }
}

/// Immutable host state observed at the start of an optimistic transaction.
pub struct HostSnapshot<S: TaskSpecialization> {
    wake_generation: u64,
    store: StoreSnapshot,
    extra: S::Snapshot,
}

impl<S: TaskSpecialization> HostSnapshot<S> {
    pub fn new(wake_generation: u64, store: StoreSnapshot, extra: S::Snapshot) -> Self {
        Self {
            wake_generation,
            store,
            extra,
        }
    }

    pub fn generation(&self) -> u64 {
        self.wake_generation
    }

    #[doc(hidden)]
    pub fn store(&self) -> &StoreSnapshot {
        &self.store
    }

    pub fn extra(&self) -> &S::Snapshot {
        &self.extra
    }
}

impl<S: TaskSpecialization> Clone for HostSnapshot<S> {
    fn clone(&self) -> Self {
        Self {
            wake_generation: self.wake_generation,
            store: self.store.clone(),
            extra: self.extra.clone(),
        }
    }
}

/// Changes to host-owned resources produced by one successful outer cut.
pub struct TaskCommit<S: TaskSpecialization> {
    store: StoreJournal,
    extra_snapshot: S::Snapshot,
    extra: S::Journal,
}

impl<S: TaskSpecialization> TaskCommit<S> {
    pub fn new(store: StoreJournal, extra_snapshot: S::Snapshot, extra: S::Journal) -> Self {
        Self {
            store,
            extra_snapshot,
            extra,
        }
    }

    pub fn extra_snapshot(&self) -> &S::Snapshot {
        &self.extra_snapshot
    }

    pub fn extra(&self) -> &S::Journal {
        &self.extra
    }

    #[doc(hidden)]
    pub fn into_parts(self) -> (StoreJournal, S::Snapshot, S::Journal) {
        (self.store, self.extra_snapshot, self.extra)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitResult {
    Committed,
    Conflict,
    MissingVolume(VolumeId),
    Closed,
}

/// Supplies the immutable environment owned by a task's reasoning host.
/// Reflection code can read it through `.env`, but cannot replace it.
pub trait TaskEnvironment: Send + Sync {
    fn reflection_environment(&self) -> PublicValue;
}

pub trait TaskHost<S: TaskSpecialization>: TaskEnvironment + Send + Sync {
    fn snapshot(&self) -> HostSnapshot<S>;
    fn commit(&self, commit: TaskCommit<S>) -> CommitResult;

    /// Identifies the reasoning scope accepted by private volume capability
    /// requests. Hosts without protected volumes retain the default.
    fn reasoning_session_id(&self) -> Option<ReasoningSessionId> {
        None
    }

    /// Waits until the observed generation changes. Returns false when the
    /// task should stop rather than retry.
    fn wait_for_change(&self, observed_generation: u64) -> bool;
}

#[derive(Debug, Clone)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum TaskOutcome {
    Complete(PublicValue),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct TaskHalt(TaskHaltKind);

#[derive(Debug, Clone)]
enum TaskHaltKind {
    Failure(TaskFailure),
    Blocked(EvaluationWaitToken),
}

/// Root disposition for one permanent task failure.
///
/// `EdgeFree` is restricted to freshly constructed text-only failures and the
/// bounded compatibility path from an evaluator phase. Any failure retained
/// by a lifecycle, search result, or other host-visible protocol surface is
/// converted to `Rooted` first. I4F.1d.3 removes the evaluator compatibility
/// case when parked machine failures adopt their final root shape.
#[derive(Debug, Clone)]
enum TaskFailure {
    EdgeFree(Arc<EvaluationFailure>),
    Rooted(RuntimeFailureRoot),
}

impl TaskFailure {
    fn as_failure(&self) -> &Arc<EvaluationFailure> {
        match self {
            Self::EdgeFree(failure) => failure,
            Self::Rooted(failure) => failure.as_failure(),
        }
    }

    fn into_failure(self) -> Arc<EvaluationFailure> {
        match self {
            Self::EdgeFree(failure) => failure,
            Self::Rooted(failure) => failure.into_failure(),
        }
    }

    fn runtime_id(&self) -> Option<EvaluationRuntimeId> {
        match self {
            Self::EdgeFree(_) => None,
            Self::Rooted(failure) => Some(failure.runtime_id()),
        }
    }
}

impl PartialEq for TaskHalt {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (TaskHaltKind::Failure(left), TaskHaltKind::Failure(right)) => {
                left.as_failure() == right.as_failure()
            }
            (TaskHaltKind::Blocked(left), TaskHaltKind::Blocked(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for TaskHalt {}

impl TaskHalt {
    pub fn new(message: impl Into<Arc<str>>) -> Self {
        let message = message.into();
        Self::failure(Arc::new(EvaluationFailure::message(message.as_ref())))
    }

    /// Constructs the bounded compatibility form used within one evaluator or
    /// interpreter phase. The caller must root it before durable publication.
    pub(super) fn failure(failure: Arc<EvaluationFailure>) -> Self {
        Self(TaskHaltKind::Failure(TaskFailure::EdgeFree(failure)))
    }

    pub(super) fn rooted_failure(failure: RuntimeFailureRoot) -> Self {
        Self(TaskHaltKind::Failure(TaskFailure::Rooted(failure)))
    }

    pub(super) fn root_for_runtime(self, runtime: EvaluationRuntimeId) -> Self {
        match self.0 {
            TaskHaltKind::Failure(TaskFailure::EdgeFree(failure)) => {
                Self::rooted_failure(RuntimeFailureRoot::from_runtime(runtime, failure))
            }
            TaskHaltKind::Failure(failure @ TaskFailure::Rooted(_)) => {
                debug_assert_eq!(failure.runtime_id(), Some(runtime));
                Self(TaskHaltKind::Failure(failure))
            }
            TaskHaltKind::Blocked(wait) => Self::blocked(wait),
        }
    }

    /// Consumes one permanent halt at a runtime-owned publication boundary.
    /// Existing roots retain their identity; bounded evaluator failures gain
    /// their compatibility root exactly once here.
    pub(super) fn into_failure_root(self, runtime: EvaluationRuntimeId) -> RuntimeFailureRoot {
        match self.0 {
            TaskHaltKind::Failure(TaskFailure::EdgeFree(failure)) => {
                RuntimeFailureRoot::from_runtime(runtime, failure)
            }
            TaskHaltKind::Failure(TaskFailure::Rooted(failure)) => {
                debug_assert_eq!(failure.runtime_id(), runtime);
                failure
            }
            TaskHaltKind::Blocked(_) => {
                panic!("a blocked task halt cannot become a permanent failure root")
            }
        }
    }

    pub(super) fn with_core_context(self, context: Value) -> Self {
        match self.0 {
            TaskHaltKind::Failure(failure) => {
                let runtime = failure.runtime_id();
                let failure = Arc::new(failure.into_failure().with_context(context));
                match runtime {
                    Some(runtime) => {
                        Self::rooted_failure(RuntimeFailureRoot::from_runtime(runtime, failure))
                    }
                    None => Self::failure(failure),
                }
            }
            TaskHaltKind::Blocked(wait) => Self::blocked(wait),
        }
    }

    /// Prepends one structured frame when a host client propagates this task
    /// failure through another semantic boundary.
    pub fn with_context(self, context: PublicValue) -> Self {
        let runtime = context.runtime_id();
        if let TaskHaltKind::Failure(failure) = &self.0
            && failure.runtime_id().is_some_and(|owner| owner != runtime)
        {
            return Self::new(format!(
                "task failure context belongs to evaluation runtime {}, not {}",
                runtime.get(),
                failure
                    .runtime_id()
                    .expect("the rooted owner was checked")
                    .get()
            ));
        }
        self.with_core_context(context.into_core())
            .root_for_runtime(runtime)
    }

    /// Projects a permanent task failure into its structured diagnostic.
    pub fn diagnostic(&self, values: &Values) -> Diagnostic {
        let failure = self
            .permanent_failure()
            .expect("a blocked task halt has no failure diagnostic");
        Diagnostic::from_parts(
            values.core(),
            None,
            Severity::Error,
            eval::failure_diagnostic_value_with(values.core(), failure),
            None,
        )
    }

    pub(super) fn into_failure(self) -> Arc<EvaluationFailure> {
        match self.0 {
            TaskHaltKind::Failure(failure) => failure.into_failure(),
            TaskHaltKind::Blocked(_) => {
                panic!("a blocked task halt cannot become a permanent evaluation failure")
            }
        }
    }

    pub(crate) fn into_evaluation_halt(self) -> EvaluationHalt {
        match self.0 {
            TaskHaltKind::Failure(failure) => EvaluationHalt::failure(failure.into_failure()),
            TaskHaltKind::Blocked(wait) => EvaluationHalt::blocked(CoreWaitToken(wait)),
        }
    }

    pub(super) fn permanent_failure(&self) -> Option<&Arc<EvaluationFailure>> {
        match &self.0 {
            TaskHaltKind::Failure(failure) => Some(failure.as_failure()),
            TaskHaltKind::Blocked(_) => None,
        }
    }

    pub(super) fn blocked(wait: EvaluationWaitToken) -> Self {
        Self(TaskHaltKind::Blocked(wait))
    }

    pub(super) fn blocked_on(&self) -> Option<&EvaluationWaitToken> {
        match &self.0 {
            TaskHaltKind::Blocked(wait) => Some(wait),
            TaskHaltKind::Failure(_) => None,
        }
    }

    pub(super) fn failure_root(&self) -> Option<&RuntimeFailureRoot> {
        match &self.0 {
            TaskHaltKind::Failure(TaskFailure::Rooted(failure)) => Some(failure),
            TaskHaltKind::Failure(TaskFailure::EdgeFree(_)) | TaskHaltKind::Blocked(_) => None,
        }
    }
}

impl fmt::Display for TaskHalt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            TaskHaltKind::Failure(failure) => failure.as_failure().fmt(formatter),
            TaskHaltKind::Blocked(wait) => {
                write!(
                    formatter,
                    "reflection task blocked on wait token {}",
                    wait.get()
                )
            }
        }
    }
}

impl std::error::Error for TaskHalt {}

impl From<EvaluationHalt> for TaskHalt {
    fn from(error: EvaluationHalt) -> Self {
        task_eval_error(error)
    }
}

impl From<ApiError> for TaskHalt {
    fn from(error: ApiError) -> Self {
        match error.structured_diagnostic() {
            Some(diagnostic) => {
                let runtime = diagnostic.emission().runtime_id();
                let failure = Arc::new(EvaluationFailure::emission(
                    diagnostic.emission().as_core().clone(),
                ));
                Self::rooted_failure(RuntimeFailureRoot::from_runtime(runtime, failure))
            }
            None => Self::new(error.to_string()),
        }
    }
}

#[derive(Clone)]
pub(super) struct Transaction<S: TaskSpecialization> {
    pub(super) snapshot: HostSnapshot<S>,
    pub(super) store: StoreJournal,
    pub(super) journal: S::Journal,
    pub(super) observed: bool,
}

impl<S: TaskSpecialization> Transaction<S> {
    pub(super) fn new(snapshot: HostSnapshot<S>) -> Self {
        let store = StoreJournal::new(snapshot.store().clone());
        Self {
            snapshot,
            store,
            journal: S::Journal::default(),
            observed: false,
        }
    }
}

/// Restricted access to the host and current transaction for extra effects.
pub struct RequestContext<'a, S: TaskSpecialization> {
    pub(super) eval_context: &'a EvalContext,
    pub(super) poll_context: &'a EvaluationPollContext,
    pub(super) host: &'a Arc<S::Host>,
    pub(super) transaction: Option<&'a mut Transaction<S>>,
    pub(super) activity: &'a mut RequestActivity,
}

impl<'a, S: TaskSpecialization> RequestContext<'a, S> {
    pub(crate) fn eval_context(&self) -> &EvalContext {
        self.eval_context
    }

    /// Returns runtime-local value construction for this effect request.
    pub fn values(&self) -> Values {
        Values::from_core_factory(self.eval_context.values().clone())
    }

    /// Demands the outer weak-head normal form of a request argument.
    pub fn evaluate(&self, value: &PublicValue) -> Result<EvaluatedValue, TaskHalt> {
        let value = self.evaluate_root(value)?;
        let values = Values::from_core_factory(self.eval_context.values().clone());
        Ok(EvaluatedValue::from_whnf(
            &values,
            PublicValue::from_runtime_root(value),
        ))
    }

    /// Evaluates one path expression entirely inside a bounded evaluator
    /// phase. The resulting keys contain no managed value authority and may
    /// safely cross back into the request interpreter.
    pub(crate) fn evaluate_key_path(&self, value: &PublicValue) -> Result<Vec<Key>, TaskHalt> {
        self.require_runtime_value(value)?;
        self.poll_context
            .evaluate(self.eval_context, |evaluator| {
                eval::eval_key_path_list_in(evaluator, value.as_core())
            })
            .map_err(task_eval_error)
    }

    /// Selects a path through a runtime-local value in one bounded evaluator
    /// phase and roots the selected value before returning to the interpreter.
    pub(crate) fn evaluate_path(
        &self,
        value: &PublicValue,
        path: &[Key],
    ) -> Result<PublicValue, TaskHalt> {
        self.require_runtime_value(value)?;
        let value = self
            .poll_context
            .evaluate(self.eval_context, |evaluator| {
                let mut current = value.as_core().clone();
                for key in path {
                    let Value::Dict(dict) = eval::eval_value_in(evaluator, &current)? else {
                        return Err(EvaluationHalt::new(
                            "state path traverses a non-dictionary value",
                        ));
                    };
                    current = dict
                        .get(key)
                        .cloned()
                        .unwrap_or_else(|| Value::Dict(Dict::new_sync()));
                }
                Ok(evaluator.root_value(current))
            })
            .map_err(task_eval_error)?;
        Ok(PublicValue::from_runtime_root(value))
    }

    fn evaluate_root(&self, value: &PublicValue) -> Result<RuntimeValueRoot, TaskHalt> {
        self.require_runtime_value(value)?;
        self.poll_context
            .evaluate(self.eval_context, |evaluator| {
                let mut value = value.as_core().clone();
                while matches!(value, Value::Lazy(_) | Value::Promised(_)) {
                    value = eval::eval_value_in(evaluator, &value)?;
                }
                Ok(evaluator.root_value(value))
            })
            .map_err(task_eval_error)
    }

    fn require_runtime_value(&self, value: &PublicValue) -> Result<(), TaskHalt> {
        if value.runtime_id() != self.eval_context.values().runtime_id() {
            Err(TaskHalt::new(
                "effect request value belongs to another runtime",
            ))
        } else {
            Ok(())
        }
    }

    /// Starts a nested isolated search in the current evaluation session.
    /// Branch journals remain isolated and dependencies retain the current
    /// request's demand ownership.
    pub fn isolated_search<N>(
        &self,
        effect: &PublicValue,
        specialization: N,
        host: Arc<N::Host>,
    ) -> Result<IsolatedEffectSearch<N>, TaskHalt>
    where
        N: TaskSpecialization,
    {
        IsolatedEffectSearch::new_in_context(
            effect,
            specialization,
            host,
            self.eval_context.clone(),
        )
    }

    pub fn host(&self) -> &S::Host {
        self.host.as_ref()
    }

    pub fn transaction(&mut self) -> Option<TransactionContext<'_, S>> {
        self.transaction
            .as_deref_mut()
            .map(|transaction| TransactionContext { transaction })
    }

    /// Records that this request consulted host state at `generation`.
    /// Failed computations may be retried only when such an observation exists.
    pub fn observe_host_generation(&mut self, generation: u64) {
        if let Some(transaction) = self.transaction.as_deref_mut() {
            transaction.observed = true;
        } else if self.activity.observed_generation.is_none() {
            self.activity.observed_generation = Some(generation);
        }
    }

    /// Marks a successful immediate host mutation as a retry barrier.
    pub fn committed(&mut self) {
        assert!(
            self.transaction.is_none(),
            "journaled transaction effects do not commit immediately"
        );
        self.activity.committed = true;
    }

    pub fn transaction_generation(&self) -> Option<u64> {
        self.transaction
            .as_deref()
            .map(|transaction| transaction.snapshot.generation())
    }
}

/// Specialization-owned portions of one active transaction.
pub struct TransactionContext<'a, S: TaskSpecialization> {
    transaction: &'a mut Transaction<S>,
}

impl<S: TaskSpecialization> TransactionContext<'_, S> {
    pub fn parts(&mut self) -> (&S::Snapshot, &mut S::Journal) {
        (
            self.transaction.snapshot.extra(),
            &mut self.transaction.journal,
        )
    }

    pub(crate) fn store(&mut self) -> &mut StoreJournal {
        &mut self.transaction.store
    }
}

pub(super) fn request_value(tag: &Key, arguments: Vec<Value>) -> Value {
    Value::Dict(Dict::new_sync().insert(tag.clone(), Value::List(List::from_values(arguments))))
}

#[cfg(test)]
mod root_inventory_tests {
    use super::*;
    use crate::api::EffectTokenDomain;
    use crate::number::Number;
    use crate::reflection::{ExactConflictAnalysis, ReflectionStore};
    use std::sync::Weak;

    #[derive(Clone, Copy)]
    struct ProtocolRootTestEffects;

    impl TaskSpecialization for ProtocolRootTestEffects {
        type Host = dyn TaskHost<Self>;
        type Request = Infallible;
        type Snapshot = PublicValue;
        type Journal = Vec<PublicValue>;

        fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
            Vec::new()
        }

        fn handle_request(
            &self,
            request: Self::Request,
            _arguments: Vec<PublicValue>,
            _context: &mut RequestContext<'_, Self>,
        ) -> Result<RequestResult, TaskHalt> {
            match request {}
        }
    }

    fn assert_effect_request_spec_inventory<R>(spec: &EffectRequestSpec<R>) {
        let EffectRequestSpec {
            api_path,
            tag_path,
            arity,
            request,
        } = spec;
        let _: &Option<Arc<[Arc<str>]>> = api_path;
        let _: &Arc<[Arc<str>]> = tag_path;
        let _: &usize = arity;
        let _: &R = request;
    }

    fn assert_request_result_inventory(result: &RequestResult) {
        match result {
            RequestResult::Return(value) => {
                let _: &PublicValue = value;
            }
            RequestResult::Alternatives(values) => {
                let _: &Vec<PublicValue> = values;
            }
            RequestResult::Scoped { operation, close } => {
                let _: &PublicValue = operation;
                let _: &PublicValue = close;
            }
            RequestResult::ReturnUnit | RequestResult::Fail | RequestResult::Cancelled => {}
        }
    }

    fn assert_edge_free_protocol_inventory(
        session: &ReasoningSessionId,
        activity: &RequestActivity,
        result: &CommitResult,
    ) {
        let ReasoningSessionId(id) = session;
        let _: &NonZeroU64 = id;
        let RequestActivity {
            observed_generation,
            committed,
        } = activity;
        let _: &Option<u64> = observed_generation;
        let _: &bool = committed;
        match result {
            CommitResult::Committed | CommitResult::Conflict | CommitResult::Closed => {}
            CommitResult::MissingVolume(volume) => {
                let _: &VolumeId = volume;
            }
        }
    }

    fn assert_protocol_transaction_inventory(
        snapshot: &HostSnapshot<ProtocolRootTestEffects>,
        commit: &TaskCommit<ProtocolRootTestEffects>,
        transaction: &Transaction<ProtocolRootTestEffects>,
    ) {
        let HostSnapshot {
            wake_generation,
            store,
            extra,
        } = snapshot;
        let _: &u64 = wake_generation;
        let _: &StoreSnapshot = store;
        let _: &PublicValue = extra;

        let TaskCommit {
            store,
            extra_snapshot,
            extra,
        } = commit;
        let _: &StoreJournal = store;
        let _: &PublicValue = extra_snapshot;
        let _: &Vec<PublicValue> = extra;

        let Transaction {
            snapshot,
            store,
            journal,
            observed,
        } = transaction;
        let _: &HostSnapshot<ProtocolRootTestEffects> = snapshot;
        let _: &StoreJournal = store;
        let _: &Vec<PublicValue> = journal;
        let _: &bool = observed;
    }

    fn assert_borrowed_protocol_context_inventory<S: TaskSpecialization>(
        request: &RequestContext<'_, S>,
        transaction_context: &TransactionContext<'_, S>,
    ) {
        let RequestContext {
            eval_context,
            poll_context,
            host,
            transaction,
            activity,
        } = request;
        let _ = (eval_context, poll_context, host, transaction, activity);
        let TransactionContext { transaction } = transaction_context;
        let _ = transaction;
    }

    fn assert_task_outcome_inventory(outcome: &TaskOutcome) {
        match outcome {
            TaskOutcome::Complete(value) => {
                let _: &PublicValue = value;
            }
            TaskOutcome::Cancelled => {}
        }
    }

    fn assert_task_halt_root_inventory(halt: &TaskHalt) {
        let TaskHalt(kind) = halt;
        match kind {
            TaskHaltKind::Failure(TaskFailure::EdgeFree(failure)) => {
                let _: &Arc<EvaluationFailure> = failure;
            }
            TaskHaltKind::Failure(TaskFailure::Rooted(failure)) => {
                let _: &RuntimeFailureRoot = failure;
            }
            TaskHaltKind::Blocked(wait) => {
                let _: &EvaluationWaitToken = wait;
            }
        }
    }

    #[test]
    fn task_halt_root_inventory_is_complete() {
        let _: fn(&TaskHalt) = assert_task_halt_root_inventory;
    }

    #[test]
    fn reflection_protocol_root_inventory_is_complete() {
        let _: fn(&EffectRequestSpec<Infallible>) = assert_effect_request_spec_inventory;
        let _: fn(&RequestResult) = assert_request_result_inventory;
        let _: fn(&ReasoningSessionId, &RequestActivity, &CommitResult) =
            assert_edge_free_protocol_inventory;
        let _: fn(
            &HostSnapshot<ProtocolRootTestEffects>,
            &TaskCommit<ProtocolRootTestEffects>,
            &Transaction<ProtocolRootTestEffects>,
        ) = assert_protocol_transaction_inventory;
        let _: fn(
            &RequestContext<'_, ProtocolRootTestEffects>,
            &TransactionContext<'_, ProtocolRootTestEffects>,
        ) = assert_borrowed_protocol_context_inventory;
        let _: fn(&TaskOutcome) = assert_task_outcome_inventory;
    }

    fn retained_protocol_value(domain: &EffectTokenDomain<Arc<()>>) -> (PublicValue, Weak<()>) {
        let payload = Arc::new(());
        let retained = Arc::downgrade(&payload);
        (domain.issue(payload), retained)
    }

    fn protocol_store(values: &Values) -> ReflectionStore {
        ReflectionStore::new(values.core().clone(), Arc::new(ExactConflictAnalysis))
    }

    #[test]
    fn request_results_and_outcomes_retain_public_roots_until_retirement() {
        let core = crate::core::test_value_factory();
        let values = Values::from_core_factory(core);
        let domain = EffectTokenDomain::new(&values);

        for build in [
            RequestResult::Return as fn(PublicValue) -> RequestResult,
            |value| RequestResult::Alternatives(vec![value]),
            |value| RequestResult::Scoped {
                operation: value.clone(),
                close: value,
            },
        ] {
            let (value, retained) = retained_protocol_value(&domain);
            let result = build(value);
            assert!(retained.upgrade().is_some());
            drop(result);
            assert!(retained.upgrade().is_none());
        }

        let (value, retained) = retained_protocol_value(&domain);
        let outcome = TaskOutcome::Complete(value);
        assert!(retained.upgrade().is_some());
        drop(outcome);
        assert!(retained.upgrade().is_none());
    }

    #[test]
    fn protocol_snapshots_commits_and_transactions_retain_specialization_roots() {
        let core = crate::core::test_value_factory();
        let values = Values::from_core_factory(core);
        let domain = EffectTokenDomain::new(&values);
        let store = protocol_store(&values);

        let (extra, retained) = retained_protocol_value(&domain);
        let snapshot = HostSnapshot::<ProtocolRootTestEffects>::new(1, store.snapshot(), extra);
        assert!(retained.upgrade().is_some());
        drop(snapshot);
        assert!(retained.upgrade().is_none());

        let (extra_snapshot, retained_snapshot) = retained_protocol_value(&domain);
        let (journal_value, retained_journal) = retained_protocol_value(&domain);
        let store_snapshot = store.snapshot();
        let commit = TaskCommit::<ProtocolRootTestEffects>::new(
            StoreJournal::new(store_snapshot),
            extra_snapshot,
            vec![journal_value],
        );
        assert!(retained_snapshot.upgrade().is_some());
        assert!(retained_journal.upgrade().is_some());
        drop(commit);
        assert!(retained_snapshot.upgrade().is_none());
        assert!(retained_journal.upgrade().is_none());

        let (extra, retained_snapshot) = retained_protocol_value(&domain);
        let (journal_value, retained_journal) = retained_protocol_value(&domain);
        let snapshot = HostSnapshot::<ProtocolRootTestEffects>::new(2, store.snapshot(), extra);
        let mut transaction = Transaction::new(snapshot);
        transaction.journal.push(journal_value);
        assert!(retained_snapshot.upgrade().is_some());
        assert!(retained_journal.upgrade().is_some());
        drop(transaction);
        assert!(retained_snapshot.upgrade().is_none());
        assert!(retained_journal.upgrade().is_none());
    }

    #[test]
    fn public_context_roots_a_bounded_evaluation_failure() {
        let values = crate::core::test_value_factory();
        let public_values = Values::from_core_factory(values.clone());
        let emission = Value::Number(Number::integer(41));
        let context = public_values.integer(42);
        let halt = TaskHalt::from(EvaluationHalt::from_value(emission));
        assert!(
            halt.failure_root().is_none(),
            "the evaluator conversion remains bounded until publication"
        );

        let halt = halt.with_context(context);
        let root = halt
            .failure_root()
            .expect("a public context must establish the runtime root");
        assert_eq!(root.runtime_id(), values.runtime_id());
        assert_eq!(root.direct_value_roots().len(), 2);
    }

    #[test]
    fn structured_api_error_preserves_its_runtime_root() {
        let values = crate::core::test_value_factory();
        let error = ApiError::from_eval(
            &values,
            EvaluationHalt::from_value(Value::Number(Number::integer(42))),
        );
        let halt = TaskHalt::from(error);
        let root = halt
            .failure_root()
            .expect("structured public errors must remain runtime-rooted");
        assert_eq!(root.runtime_id(), values.runtime_id());
        assert_eq!(root.direct_value_roots().len(), 1);
    }
}
