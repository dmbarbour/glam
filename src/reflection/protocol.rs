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
use crate::evaluation::{EvalContext, EvaluationWaitToken};

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
            eval::constant_effect(request_value(
                &Key::abstract_global_path(self.tag_path.iter().map(Arc::as_ref)),
                arguments.into_iter().map(PublicValue::into_core).collect(),
            )),
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
    type Request: Clone + Send + Sync + 'static;
    type Snapshot: Clone + Send + Sync + 'static;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutcome {
    Complete(PublicValue),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskHalt(TaskHaltKind);

#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskHaltKind {
    Failure(Arc<EvaluationFailure>),
    Blocked(EvaluationWaitToken),
}

impl TaskHalt {
    pub fn new(message: impl Into<Arc<str>>) -> Self {
        let message = message.into();
        Self::failure(Arc::new(EvaluationFailure::message(message.as_ref())))
    }

    pub(super) fn failure(failure: Arc<EvaluationFailure>) -> Self {
        Self(TaskHaltKind::Failure(failure))
    }

    pub(super) fn with_core_context(self, context: Value) -> Self {
        match self.0 {
            TaskHaltKind::Failure(failure) => {
                Self::failure(Arc::new(failure.with_context(context)))
            }
            TaskHaltKind::Blocked(wait) => Self::blocked(wait),
        }
    }

    /// Prepends one structured frame when a host client propagates this task
    /// failure through another semantic boundary.
    pub fn with_context(self, context: PublicValue) -> Self {
        self.with_core_context(context.into_core())
    }

    /// Projects a permanent task failure into its structured diagnostic.
    pub fn diagnostic(&self, values: &Values) -> Diagnostic {
        let failure = self
            .permanent_failure()
            .expect("a blocked task halt has no failure diagnostic");
        Diagnostic::from_emission(
            Severity::Error,
            PublicValue::from_core(
                values.core(),
                eval::failure_diagnostic_value_with(values.core(), failure),
            ),
        )
    }

    pub(super) fn into_failure(self) -> Arc<EvaluationFailure> {
        match self.0 {
            TaskHaltKind::Failure(failure) => failure,
            TaskHaltKind::Blocked(_) => {
                panic!("a blocked task halt cannot become a permanent evaluation failure")
            }
        }
    }

    pub(crate) fn into_evaluation_halt(self) -> EvaluationHalt {
        match self.0 {
            TaskHaltKind::Failure(failure) => EvaluationHalt::failure(failure),
            TaskHaltKind::Blocked(wait) => EvaluationHalt::blocked(CoreWaitToken(wait)),
        }
    }

    pub(super) fn permanent_failure(&self) -> Option<&Arc<EvaluationFailure>> {
        match &self.0 {
            TaskHaltKind::Failure(failure) => Some(failure),
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
}

impl fmt::Display for TaskHalt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            TaskHaltKind::Failure(failure) => failure.fmt(formatter),
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
        Self::failure(Arc::new(EvaluationFailure::emission(
            error
                .structured_diagnostic()
                .map(|diagnostic| diagnostic.emission().as_core().clone())
                .unwrap_or_else(|| Value::binary_from_text(&error.to_string())),
        )))
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
        if value.runtime_id() != self.eval_context.values().runtime_id() {
            return Err(TaskHalt::new(
                "effect request value belongs to another runtime",
            ));
        }
        let value =
            eval::eval_value(self.eval_context, value.as_core()).map_err(task_eval_error)?;
        Ok(EvaluatedValue::from_whnf(PublicValue::from_core(
            self.eval_context.values(),
            value,
        )))
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
