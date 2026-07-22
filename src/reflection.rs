//! External freer-effect task interpreter used by reflection clients.
//!
//! Effect requests are ordinary core values sealed by private abstract-global
//! tags. Interaction-net operators only construct those values; this module
//! performs the state, control, transaction, and host operations.

mod requests;
mod search;
mod store;

#[doc(hidden)]
pub use search::{
    IsolatedEffectSearch, IsolatedSearchBlock, IsolatedSearchPoll, IsolatedSearchResult,
};

pub use requests::{
    ReflectionHost, ReflectionJournal, ReflectionRequest, ReflectionServices,
    ReflectionTransaction, handle_reflection_request, reflection_request_specs,
};
pub use store::{
    CoarseConflictAnalysis, ConflictAnalysisStrategy, ConflictObservationIndex, ConflictPath,
    EvaluationQueryHandle, ExactConflictAnalysis, FingerprintConflictAnalysis, ReflectionStore,
    StoreCommitResult, StoreJournal, StoreSnapshot, VolumeId,
};
pub(crate) use store::{EvaluationQueryPoll, EvaluationQueryState, decode_query_state};

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;

use search::SearchPolicy;

use crate::api::{EvaluationRuntime, Value as PublicValue};
use crate::core::{
    Atom, Dict, FunctionValue, Key, LazyValue, List, NetValue, PromisedValue, Value, keys,
};
use crate::core_net::{CoreDataKey, CoreSpecialization};
use crate::eval;
use crate::evaluation::{
    EvalContext, EvaluationMachinePoll, EvaluationPumpOutcome, EvaluationSession,
    EvaluationSessionRun, EvaluationTaskBlock, EvaluationTaskId, EvaluationTaskMachine,
    EvaluationTaskPoll, EvaluationWaitToken, ReflectionTaskKind, ReflectionTaskLauncher,
};
use crate::interaction_net::NetBuilder;
use crate::number::Number;

/// One additional effect constructor contributed by a task specialization.
pub struct EffectRequestSpec<R> {
    api_path: Arc<[Arc<str>]>,
    tag_path: Arc<[Arc<str>]>,
    arity: usize,
    request: R,
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
            api_path: api_path.into_iter().map(Into::into).collect(),
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
}

/// Result of handling one specialization-owned request.
pub enum RequestResult {
    Return(PublicValue),
    ReturnUnit,
    Fail,
    Cancelled,
}

/// Globally unique identity of one reasoning session. Volume IDs are local to
/// this scope and capability requests carry both identities.
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
struct RequestActivity {
    observed_generation: Option<u64>,
    committed: bool,
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

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>>;

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<PublicValue>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskError>;
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
    ) -> Result<RequestResult, TaskError> {
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
    ) -> Result<RequestResult, TaskError> {
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
    fn reflection_environment(&self) -> PublicValue {
        PublicValue::empty_record()
    }
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
pub struct TaskError(TaskErrorKind);

#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskErrorKind {
    Message(Arc<str>),
    Blocked(EvaluationWaitToken),
}

impl TaskError {
    pub fn new(message: impl Into<Arc<str>>) -> Self {
        Self(TaskErrorKind::Message(message.into()))
    }

    fn blocked(wait: EvaluationWaitToken) -> Self {
        Self(TaskErrorKind::Blocked(wait))
    }

    fn blocked_on(&self) -> Option<&EvaluationWaitToken> {
        match &self.0 {
            TaskErrorKind::Blocked(wait) => Some(wait),
            TaskErrorKind::Message(_) => None,
        }
    }
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            TaskErrorKind::Message(message) => formatter.write_str(message),
            TaskErrorKind::Blocked(wait) => {
                write!(
                    formatter,
                    "reflection task blocked on wait token {}",
                    wait.get()
                )
            }
        }
    }
}

impl std::error::Error for TaskError {}

/// Configures and synchronously runs one effect task.
///
/// Runtime attachment and child reflection capabilities are independent. If
/// child reflection is enabled, all scheduled children are drained before the
/// run returns; a child failure or stable deadlock fails the composed run.
pub struct EffectRun<S: TaskSpecialization> {
    effect: PublicValue,
    specialization: S,
    host: Arc<S::Host>,
    runtime: Option<EvaluationRuntime>,
    reflection_children: Option<Arc<dyn ReflectionHost<ReflectionEffects>>>,
    result_policy: EffectResultPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectResultPolicy {
    Return,
    RequireUnit,
}

impl<S: TaskSpecialization> EffectRun<S> {
    pub fn new(effect: &PublicValue, specialization: S, host: Arc<S::Host>) -> Self {
        Self {
            effect: effect.clone(),
            specialization,
            host,
            runtime: None,
            reflection_children: None,
            result_policy: EffectResultPolicy::Return,
        }
    }

    /// Runs the task in a service session using the runtime's shared executor.
    pub fn with_runtime(mut self, runtime: &EvaluationRuntime) -> Self {
        self.runtime = Some(runtime.clone());
        self
    }

    /// Gives `.task.new` children only the reusable reflection capabilities.
    pub fn with_reflection_children(
        mut self,
        host: Arc<dyn ReflectionHost<ReflectionEffects>>,
    ) -> Self {
        self.reflection_children = Some(host);
        self
    }

    /// Requires the task's discarded result to be unit, as with
    /// `(effect =>> .r ())`.
    pub fn requiring_unit_result(mut self) -> Self {
        self.result_policy = EffectResultPolicy::RequireUnit;
        self
    }

    pub fn run(self) -> Result<TaskOutcome, TaskError> {
        let Self {
            effect,
            specialization,
            host,
            runtime,
            reflection_children,
            result_policy,
        } = self;
        let session = match runtime {
            Some(runtime) => EvaluationSession::shared(runtime.executor()),
            None => Arc::new(EvaluationSession::new()),
        };
        let drain_children = reflection_children.is_some();
        if let Some(reflection_host) = reflection_children {
            session
                .install_reflection_launcher(task_launcher(ReflectionEffects, reflection_host))
                .map_err(|error| TaskError::new(error.as_ref()))?;
        }
        let mut task = EffectTask::new_in_context(
            effect.into_core(),
            specialization,
            host,
            EvalContext::new(session),
        )?;
        if result_policy == EffectResultPolicy::RequireUnit {
            task = task.requiring_unit_result();
        }
        if drain_children {
            run_composed_effect_task(task)
        } else {
            task.run()
        }
    }
}

/// Runs one reflection effect with a statically selected set of extra effects.
pub fn run<S: TaskSpecialization>(
    effect: &PublicValue,
    specialization: S,
    host: Arc<S::Host>,
) -> Result<TaskOutcome, TaskError> {
    EffectRun::new(effect, specialization, host).run()
}

fn run_composed_effect_task<S: TaskSpecialization>(
    mut task: EffectTask<S>,
) -> Result<TaskOutcome, TaskError> {
    let parent = task.run();
    let children = task.eval_context.run_until_quiescent();
    let child_error = composed_child_error(children);
    match (parent, child_error) {
        (Ok(outcome), None) => Ok(outcome),
        (Ok(_), Some(error)) | (Err(error), None) => Err(error),
        (Err(parent), Some(children)) => Err(TaskError::new(format!(
            "{parent}; child task failure: {children}"
        ))),
    }
}

fn composed_child_error(run: EvaluationSessionRun) -> Option<TaskError> {
    let (quiescent, report) = match run {
        EvaluationSessionRun::Complete(report) => (false, report),
        EvaluationSessionRun::Quiescent(report) | EvaluationSessionRun::Deadlocked(report) => {
            (true, report)
        }
    };
    if report.failures.is_empty() && !quiescent {
        return None;
    }

    let mut details = Vec::new();
    for failure in report.failures {
        details.push(format!(
            "task {} failed: {}",
            failure.task.get(),
            failure.error
        ));
    }
    if quiescent {
        details.push(format!(
            "scheduler deadlocked with {} unfinished task{}",
            report.unfinished.len(),
            if report.unfinished.len() == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    Some(TaskError::new(details.join("; ")))
}

/// Builds a type-erased launcher for annotation and joinable reflection tasks.
pub(crate) fn task_launcher<S: TaskSpecialization>(
    specialization: S,
    host: Arc<S::Host>,
) -> Arc<dyn ReflectionTaskLauncher> {
    Arc::new(EffectTaskLauncher {
        specialization,
        host,
    })
}

struct EffectTaskLauncher<S: TaskSpecialization> {
    specialization: S,
    host: Arc<S::Host>,
}

impl<S: TaskSpecialization> ReflectionTaskLauncher for EffectTaskLauncher<S> {
    fn build(
        &self,
        context: EvalContext,
        effect: Value,
        kind: ReflectionTaskKind,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<str>> {
        let task = EffectTask::new_in_context(
            effect,
            self.specialization.clone(),
            self.host.clone(),
            context,
        )
        .map_err(|error| Arc::from(error.to_string()))?;
        Ok(match kind {
            ReflectionTaskKind::Annotation => Box::new(AnnotationEffectTask(task)),
            ReflectionTaskKind::Joinable => Box::new(JoinableEffectTask(task)),
        })
    }
}

/// Runs a task with standard effects and no specialization-owned requests.
pub fn run_standard(
    effect: &PublicValue,
    host: Arc<dyn TaskHost<StandardEffects>>,
) -> Result<TaskOutcome, TaskError> {
    EffectRun::new(effect, StandardEffects, host).run()
}

#[derive(Clone)]
struct Tags {
    r: Key,
    seq: Key,
    alt: Key,
    fail: Key,
    cut: Key,
    fix: Key,
    get: Key,
    set: Key,
    heap_get: Key,
    heap_set: Key,
    heap_rewrite: Key,
    reset: Key,
    shift: Key,
    resume: Key,
    continuation_state: Key,
}

impl Tags {
    fn new() -> Self {
        let tag = |name| {
            Key::atom_from_key(&Key::abstract_global_path([
                "reflection_runtime",
                "v0",
                "request",
                name,
            ]))
        };
        Self {
            r: tag("r"),
            seq: tag("seq"),
            alt: tag("alt"),
            fail: tag("fail"),
            cut: tag("cut"),
            fix: tag("fix"),
            get: tag("get"),
            set: tag("set"),
            heap_get: tag("heap_get"),
            heap_set: tag("heap_set"),
            heap_rewrite: tag("heap_rewrite"),
            reset: tag("reset"),
            shift: tag("shift"),
            resume: tag("resume"),
            // The key is private, but its value deliberately travels with
            // whole-user-state get/set operations.
            continuation_state: Key::abstract_global_path([
                "reflection_runtime",
                "v0",
                "state",
                "continuations",
            ]),
        }
    }
}

struct EffectTask<S: TaskSpecialization> {
    eval_context: EvalContext,
    id: EvaluationTaskId,
    specialization: S,
    host: Arc<S::Host>,
    tags: Tags,
    specialized_requests: Vec<SpecializedRequest<S::Request>>,
    api: Value,
    next_continuation: u64,
    next_control_order: usize,
    continuations: HashMap<u64, CapturedContinuation>,
    search: SearchPolicy<Branch<S>, IsolatedSearchResult<S>>,
    execution: TaskExecution<S>,
    blocked: Option<BlockedExecution<S>>,
    terminal: Option<TaskTerminal>,
}

impl<S: TaskSpecialization> EffectTask<S> {
    #[cfg(test)]
    fn new(effect: Value, specialization: S, host: Arc<S::Host>) -> Result<Self, TaskError> {
        Self::new_in_context(effect, specialization, host, EvalContext::standalone())
    }

    fn new_in_context(
        effect: Value,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
    ) -> Result<Self, TaskError> {
        Self::new_in_context_with_policy(effect, specialization, host, eval_context, false)
    }

    fn new_isolated_in_context(
        effect: Value,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
    ) -> Result<Self, TaskError> {
        Self::new_in_context_with_policy(effect, specialization, host, eval_context, true)
    }

    fn new_in_context_with_policy(
        effect: Value,
        specialization: S,
        host: Arc<S::Host>,
        eval_context: EvalContext,
        retain_all: bool,
    ) -> Result<Self, TaskError> {
        let tags = Tags::new();
        let (api, specialized_requests) = effect_api(&tags, specialization.requests())?;
        let id = eval_context
            .task_id()
            .map_err(|error| TaskError::new(error.as_ref()))?;
        let initial_state = Value::Dict(Dict::new_sync());
        let root = Branch::new(effect, initial_state);
        let (search, branch) = if retain_all {
            let mut branch = root.clone();
            branch.transaction = Some(Transaction::new(host.snapshot()));
            (SearchPolicy::retaining_all(root), branch)
        } else {
            (SearchPolicy::FirstSuccess, root)
        };
        Ok(Self {
            eval_context,
            id,
            specialization,
            host,
            tags,
            specialized_requests,
            api,
            next_continuation: 1,
            next_control_order: 1,
            continuations: HashMap::new(),
            search,
            execution: TaskExecution {
                work: MachineWork::Drive {
                    branch,
                    scope_depth: 0,
                },
                cuts: Vec::new(),
            },
            blocked: None,
            terminal: None,
        })
    }

    fn completed_search(&self) -> Option<Arc<[IsolatedSearchResult<S>]>> {
        self.search.completed()
    }

    fn requiring_unit_result(mut self) -> Self {
        self.execution
            .work
            .branch_mut()
            .expect("a fresh effect task must contain its initial branch")
            .control
            .sequence
            .push(Continuation::RequireUnit);
        self
    }

    fn allocate_control_order(&mut self) -> Result<usize, TaskError> {
        let order = self.next_control_order;
        self.next_control_order = self
            .next_control_order
            .checked_add(1)
            .ok_or_else(|| TaskError::new("reflection control order exhausted"))?;
        Ok(order)
    }

    fn capture_continuation(
        &mut self,
        continuation: CapturedContinuation,
    ) -> Result<Value, TaskError> {
        let id = self.next_continuation;
        self.next_continuation = self
            .next_continuation
            .checked_add(1)
            .ok_or_else(|| TaskError::new("reflection continuation IDs exhausted"))?;
        self.continuations.insert(id, continuation);
        Ok(request_function(
            self.tags.resume.clone(),
            3,
            vec![
                Value::Number(Number::from_u64(self.id.get())),
                Value::Number(Number::from_u64(id)),
            ],
            true,
        ))
    }

    fn start_fixpoint(
        &mut self,
        root: Arc<FixRoot<S>>,
        choices: Vec<FixChoice>,
    ) -> Result<MachineWork<S>, TaskError> {
        let mut branch = root.entry.clone();
        let reset_stack = reset_stack_value(
            &self.eval_context,
            &branch.state,
            &self.tags.continuation_state,
        )?;
        let state = with_reset_frames(
            &self.eval_context,
            branch.state.clone(),
            &self.tags.continuation_state,
            &[],
        )?;
        let order = self.allocate_control_order()?;
        let handle = PromisedValue::fixpoint(&self.eval_context, "reflection effect fixpoint")
            .map_err(|error| TaskError::new(error.as_ref()))?;
        let marker = Value::Promised(handle.clone());
        let outer_control = std::mem::take(&mut branch.control);
        branch.state = state;
        branch.active_fixes.push(ActiveFix {
            root: root.clone(),
            choices,
            next_choice: 0,
            handle: handle.clone(),
        });
        branch.control.sequence.push(Continuation::Fix(handle));
        branch.control.delimiters.push(Delimiter::Restore {
            outer: Box::new(outer_control),
            reset_stack,
            scope_depth: root.scope_depth,
            order,
        });
        Ok(MachineWork::Apply {
            function: root.function.clone(),
            arguments: vec![marker],
            branch,
            scope_depth: root.scope_depth,
        })
    }

    fn restart_fixpoint_at_scope(
        &mut self,
        branch: &mut Branch<S>,
        scope_depth: usize,
    ) -> Result<Option<MachineWork<S>>, TaskError> {
        let Some(restart) = branch.fix_restarts.last() else {
            return Ok(None);
        };
        if restart.root.scope_depth < scope_depth {
            return Ok(None);
        }
        if restart.root.scope_depth > scope_depth {
            return Err(TaskError::new(
                "reflection fixpoint restart escaped its evaluation scope",
            ));
        }

        let restart = branch
            .fix_restarts
            .pop()
            .expect("restart observed above must exist");
        let mut restarted = self.start_fixpoint(restart.root, restart.choices)?;
        restarted
            .branch_mut()
            .expect("fixpoint restart must retain its branch")
            .fix_restarts = restart.inherited_restarts;
        Ok(Some(restarted))
    }

    fn install_captured_control(
        &mut self,
        branch: &mut Branch<S>,
        captured: &CapturedContinuation,
        scope_depth: usize,
    ) -> Result<(), TaskError> {
        let mut layers = captured
            .reset_frames
            .iter()
            .cloned()
            .map(CapturedLayer::Reset)
            .chain(
                captured
                    .delimiters
                    .iter()
                    .cloned()
                    .map(CapturedLayer::Delimiter),
            )
            .collect::<Vec<_>>();
        layers.sort_by_key(CapturedLayer::order);

        let mut reset_frames = reset_frames(
            &self.eval_context,
            &branch.state,
            &self.tags.continuation_state,
        )?;
        let next_order = self
            .next_control_order
            .checked_add(layers.len())
            .ok_or_else(|| TaskError::new("reflection control order exhausted"))?;
        let mut delimiters = Vec::new();
        for (order, layer) in (self.next_control_order..).zip(layers) {
            match layer {
                CapturedLayer::Reset(mut frame) => {
                    frame.scope_depth = scope_depth;
                    frame.order = order;
                    reset_frames.push(frame);
                }
                CapturedLayer::Delimiter(mut delimiter) => {
                    delimiter.rebase(scope_depth, order);
                    delimiters.push(delimiter);
                }
            }
        }
        let state = with_reset_frames(
            &self.eval_context,
            branch.state.clone(),
            &self.tags.continuation_state,
            &reset_frames,
        )?;
        self.next_control_order = next_order;
        branch.state = state;
        branch.control.delimiters.extend(delimiters);
        Ok(())
    }

    fn run(&mut self) -> Result<TaskOutcome, TaskError> {
        loop {
            match self.poll(256) {
                EffectTaskPoll::Yielded => {}
                EffectTaskPoll::Blocked(blocked) => {
                    if let Some(wait) = blocked.lazy {
                        match self.eval_context.pump_wait(&wait, 4_096) {
                            EvaluationPumpOutcome::TargetReady
                            | EvaluationPumpOutcome::BudgetExhausted => continue,
                            EvaluationPumpOutcome::NoProgress
                                if blocked.observed_generation.is_none() =>
                            {
                                let error = TaskError::new(
                                    "synchronous reflection task has no runnable producer for its dependency",
                                );
                                self.finish(TaskTerminal::Failed(error.clone()));
                                return Err(error);
                            }
                            EvaluationPumpOutcome::NoProgress => {}
                        }
                    }
                    let generation = blocked.observed_generation.ok_or_else(|| {
                        TaskError::new("blocked reflection task has no wake condition")
                    })?;
                    if !self.host.wait_for_change(generation) {
                        self.finish(TaskTerminal::Cancelled);
                    }
                }
                EffectTaskPoll::Complete(value) => return Ok(TaskOutcome::Complete(value)),
                EffectTaskPoll::Failed(error) => return Err(error),
                EffectTaskPoll::Cancelled => return Ok(TaskOutcome::Cancelled),
            }
        }
    }

    fn poll(&mut self, steps: usize) -> EffectTaskPoll {
        if let Some(terminal) = &self.terminal {
            return terminal.poll();
        }
        if let Some(blocked) = self.poll_blocked() {
            return blocked;
        }

        for _ in 0..steps {
            let work = self.execution.work.clone();
            match self.step(work) {
                Ok(MachineStep::Continue(work)) => self.execution.work = work,
                Ok(MachineStep::Blocked(blocked)) => {
                    self.blocked = Some(blocked);
                    return self.blocked_poll();
                }
                Ok(MachineStep::Terminal(terminal)) => {
                    self.finish(terminal);
                    return self.terminal.as_ref().expect("terminal set above").poll();
                }
                Err(error) => {
                    if let Some(wait) = error.blocked_on() {
                        self.blocked = Some(self.waiting_block(wait.clone()));
                        return self.blocked_poll();
                    }
                    if let Some(retry) = self.retry_wake() {
                        self.blocked = Some(BlockedExecution::evaluation_error(error, retry));
                        return self.blocked_poll();
                    }
                    self.finish(TaskTerminal::Failed(error));
                    return self.terminal.as_ref().expect("terminal set above").poll();
                }
            }
        }
        EffectTaskPoll::Yielded
    }

    fn step(&mut self, work: MachineWork<S>) -> Result<MachineStep<S>, TaskError> {
        match work {
            MachineWork::Drive {
                branch,
                scope_depth,
            } => self.drive_step(branch, scope_depth),
            MachineWork::Deliver {
                value,
                branch,
                scope_depth,
            } => self.deliver_step(value, branch, scope_depth),
            MachineWork::Apply {
                function,
                arguments,
                mut branch,
                scope_depth,
            } => {
                branch.effect = apply(&self.eval_context, function, arguments)?;
                Ok(MachineStep::Continue(MachineWork::Drive {
                    branch,
                    scope_depth,
                }))
            }
            MachineWork::Outcome {
                outcome,
                scope_depth,
            } => self.handle_outcome(outcome, scope_depth),
        }
    }

    fn drive_step(
        &mut self,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskError> {
        let request = self.effect_request(branch.effect.clone())?;
        let work = match request {
            Request::Return(value) => MachineWork::Deliver {
                value,
                branch,
                scope_depth,
            },
            Request::Seq(operation, continuation) => {
                branch
                    .control
                    .sequence
                    .push(Continuation::Glam(continuation));
                branch.effect = operation;
                MachineWork::Drive {
                    branch,
                    scope_depth,
                }
            }
            Request::Alt(left, right) => {
                if (scope_depth > 0 || self.search.retains_all()) && !branch.active_fixes.is_empty()
                {
                    let inherited_restarts = branch.fix_restarts.clone();
                    let active = branch
                        .active_fixes
                        .first_mut()
                        .expect("checked nonempty fixpoint stack");
                    if let Some(choice) = active.choices.get(active.next_choice).copied() {
                        active.next_choice += 1;
                        branch.effect = match choice {
                            FixChoice::Left => left,
                            FixChoice::Right => right,
                        };
                    } else {
                        let root = active.root.clone();
                        let mut right_choices = active.choices.clone();
                        right_choices.push(FixChoice::Right);
                        active.choices.push(FixChoice::Left);
                        active.next_choice += 1;
                        branch.effect = left;
                        branch.fix_restarts.push(FixRestart {
                            root,
                            choices: right_choices,
                            inherited_restarts,
                        });
                    }
                    MachineWork::Drive {
                        branch,
                        scope_depth,
                    }
                } else {
                    MachineWork::Outcome {
                        outcome: BranchOutcome::Fork(
                            Box::new(branch.with_effect(left)),
                            Box::new(branch.with_effect(right)),
                        ),
                        scope_depth,
                    }
                }
            }
            Request::Fail => MachineWork::Outcome {
                outcome: branch.into_failure(),
                scope_depth,
            },
            Request::Cut(operation) => {
                return Ok(MachineStep::Continue(self.enter_cut(
                    operation,
                    branch,
                    scope_depth,
                )));
            }
            Request::Get(path) => {
                let path =
                    eval::eval_key_path_list(&self.eval_context, &path).map_err(task_eval_error)?;
                let value = get_value_path(&self.eval_context, &branch.state, &path)?;
                MachineWork::Deliver {
                    value,
                    branch,
                    scope_depth,
                }
            }
            Request::Set(path, value) => {
                branch.state = set_state_path(&self.eval_context, branch.state, &path, value)?;
                MachineWork::Deliver {
                    value: unit_value(),
                    branch,
                    scope_depth,
                }
            }
            Request::HeapGet(path) => {
                let path =
                    eval::eval_key_path_list(&self.eval_context, &path).map_err(task_eval_error)?;
                let checkpoint = branch.retry_candidate();
                let heap = if let Some(transaction) = branch.transaction.as_mut() {
                    let generation = transaction.snapshot.generation();
                    let observed = transaction.store.observe_read(&path);
                    let heap = transaction.store.view().as_core().clone();
                    if observed {
                        branch.observe(checkpoint, generation);
                    }
                    heap
                } else {
                    let snapshot = self.host.snapshot();
                    branch.observe(checkpoint, snapshot.generation());
                    snapshot.store().root().as_core().clone()
                };
                let value = lazy_value_path(heap, &path);
                MachineWork::Deliver {
                    value,
                    branch,
                    scope_depth,
                }
            }
            Request::HeapSet(path, value) => {
                let path =
                    eval::eval_key_path_list(&self.eval_context, &path).map_err(task_eval_error)?;
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction.store.write(path, PublicValue::from_core(value));
                    MachineWork::Deliver {
                        value: unit_value(),
                        branch,
                        scope_depth,
                    }
                } else {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.write(path, PublicValue::from_core(value));
                    let commit =
                        TaskCommit::new(store, snapshot.extra().clone(), S::Journal::default());
                    match self.host.commit(commit) {
                        CommitResult::Committed => {
                            branch.retry = None;
                            MachineWork::Deliver {
                                value: unit_value(),
                                branch,
                                scope_depth,
                            }
                        }
                        CommitResult::Conflict => MachineWork::Drive {
                            branch,
                            scope_depth,
                        },
                        CommitResult::MissingVolume(volume) => {
                            return Err(missing_volume_error(volume));
                        }
                        CommitResult::Closed => MachineWork::Outcome {
                            outcome: BranchOutcome::Cancelled,
                            scope_depth,
                        },
                    }
                }
            }
            Request::HeapRewrite(path, updater) => {
                let path =
                    eval::eval_key_path_list(&self.eval_context, &path).map_err(task_eval_error)?;
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction
                        .store
                        .rewrite(path, PublicValue::from_core(updater));
                    MachineWork::Deliver {
                        value: unit_value(),
                        branch,
                        scope_depth,
                    }
                } else {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.rewrite(path, PublicValue::from_core(updater));
                    let commit =
                        TaskCommit::new(store, snapshot.extra().clone(), S::Journal::default());
                    match self.host.commit(commit) {
                        CommitResult::Committed => {
                            branch.retry = None;
                            MachineWork::Deliver {
                                value: unit_value(),
                                branch,
                                scope_depth,
                            }
                        }
                        CommitResult::Conflict => MachineWork::Drive {
                            branch,
                            scope_depth,
                        },
                        CommitResult::MissingVolume(volume) => {
                            return Err(missing_volume_error(volume));
                        }
                        CommitResult::Closed => MachineWork::Outcome {
                            outcome: BranchOutcome::Cancelled,
                            scope_depth,
                        },
                    }
                }
            }
            Request::VolumeGet(volume, path) => {
                let path =
                    eval::eval_key_path_list(&self.eval_context, &path).map_err(task_eval_error)?;
                let checkpoint = branch.retry_candidate();
                let root = if let Some(transaction) = branch.transaction.as_mut() {
                    let generation = transaction.snapshot.generation();
                    let observed = transaction.store.observe_volume_read(volume, &path);
                    let root = transaction.store.volume_view(volume);
                    if observed {
                        branch.observe(checkpoint, generation);
                    }
                    root
                } else {
                    let snapshot = self.host.snapshot();
                    branch.observe(checkpoint, snapshot.generation());
                    snapshot.store().volume(volume).cloned()
                };
                let value = match root {
                    Some(root) => lazy_value_path(root.into_core(), &path),
                    None => missing_volume_value(volume),
                };
                MachineWork::Deliver {
                    value,
                    branch,
                    scope_depth,
                }
            }
            Request::VolumeSet(volume, path, value) => {
                let path =
                    eval::eval_key_path_list(&self.eval_context, &path).map_err(task_eval_error)?;
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction
                        .store
                        .write_volume(volume, path, PublicValue::from_core(value));
                    MachineWork::Deliver {
                        value: unit_value(),
                        branch,
                        scope_depth,
                    }
                } else {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.write_volume(volume, path, PublicValue::from_core(value));
                    let commit =
                        TaskCommit::new(store, snapshot.extra().clone(), S::Journal::default());
                    match self.host.commit(commit) {
                        CommitResult::Committed => {
                            branch.retry = None;
                            MachineWork::Deliver {
                                value: unit_value(),
                                branch,
                                scope_depth,
                            }
                        }
                        CommitResult::Conflict => MachineWork::Drive {
                            branch,
                            scope_depth,
                        },
                        CommitResult::MissingVolume(volume) => {
                            return Err(missing_volume_error(volume));
                        }
                        CommitResult::Closed => MachineWork::Outcome {
                            outcome: BranchOutcome::Cancelled,
                            scope_depth,
                        },
                    }
                }
            }
            Request::VolumeRewrite(volume, path, updater) => {
                let path =
                    eval::eval_key_path_list(&self.eval_context, &path).map_err(task_eval_error)?;
                if let Some(transaction) = branch.transaction.as_mut() {
                    transaction
                        .store
                        .rewrite_volume(volume, path, PublicValue::from_core(updater));
                    MachineWork::Deliver {
                        value: unit_value(),
                        branch,
                        scope_depth,
                    }
                } else {
                    let snapshot = self.host.snapshot();
                    let mut store = StoreJournal::new(snapshot.store().clone());
                    store.rewrite_volume(volume, path, PublicValue::from_core(updater));
                    let commit =
                        TaskCommit::new(store, snapshot.extra().clone(), S::Journal::default());
                    match self.host.commit(commit) {
                        CommitResult::Committed => {
                            branch.retry = None;
                            MachineWork::Deliver {
                                value: unit_value(),
                                branch,
                                scope_depth,
                            }
                        }
                        CommitResult::Conflict => MachineWork::Drive {
                            branch,
                            scope_depth,
                        },
                        CommitResult::MissingVolume(volume) => {
                            return Err(missing_volume_error(volume));
                        }
                        CommitResult::Closed => MachineWork::Outcome {
                            outcome: BranchOutcome::Cancelled,
                            scope_depth,
                        },
                    }
                }
            }
            Request::Reset(key, operation) => {
                let key = value_key(&self.eval_context, key)?;
                let mut frames = reset_frames(
                    &self.eval_context,
                    &branch.state,
                    &self.tags.continuation_state,
                )?;
                let order = self.allocate_control_order()?;
                let continuation = self.capture_continuation(CapturedContinuation {
                    sequence: std::mem::take(&mut branch.control.sequence),
                    delimiters: Vec::new(),
                    reset_frames: Vec::new(),
                })?;
                frames.push(ResetFrame {
                    key,
                    continuation,
                    scope_depth,
                    order,
                });
                branch.state =
                    replace_reset_frames(branch.state, &self.tags.continuation_state, &frames);
                branch.effect = operation;
                MachineWork::Drive {
                    branch,
                    scope_depth,
                }
            }
            Request::Shift(key, function) => {
                let key = value_key(&self.eval_context, key)?;
                let mut frames = reset_frames(
                    &self.eval_context,
                    &branch.state,
                    &self.tags.continuation_state,
                )?;
                let Some(index) = frames.iter().rposition(|frame| frame.key == key) else {
                    return Err(TaskError::new("`.shift` key is not in reset scope"));
                };
                let inner_reset_frames = frames.split_off(index + 1);
                let target = frames.pop().expect("matching reset frame must exist");
                let first_inner_delimiter = branch
                    .control
                    .delimiters
                    .iter()
                    .position(|delimiter| delimiter.order() > target.order)
                    .unwrap_or(branch.control.delimiters.len());
                let inner_delimiters = branch.control.delimiters.split_off(first_inner_delimiter);
                let continuation = self.capture_continuation(CapturedContinuation {
                    sequence: std::mem::take(&mut branch.control.sequence),
                    delimiters: inner_delimiters,
                    reset_frames: inner_reset_frames,
                })?;
                branch.state =
                    replace_reset_frames(branch.state, &self.tags.continuation_state, &frames);
                branch
                    .control
                    .sequence
                    .push(Continuation::Glam(target.continuation));
                MachineWork::Apply {
                    function,
                    arguments: vec![continuation],
                    branch,
                    scope_depth,
                }
            }
            Request::Resume(task_id, id, value) => {
                if task_id != self.id {
                    return Err(TaskError::new(
                        "captured continuation belongs to another reflection task",
                    ));
                }
                let captured = self
                    .continuations
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| TaskError::new("unknown reflection continuation"))?;
                let order = self.allocate_control_order()?;
                let caller_sequence = std::mem::take(&mut branch.control.sequence);
                branch.control.delimiters.push(Delimiter::Resume {
                    outer_sequence: caller_sequence,
                    scope_depth,
                    order,
                });
                self.install_captured_control(&mut branch, &captured, scope_depth)?;
                branch.control.sequence = captured.sequence.clone();
                MachineWork::Deliver {
                    value,
                    branch,
                    scope_depth,
                }
            }
            Request::Fix(function) => {
                let root = Arc::new(FixRoot {
                    function,
                    entry: branch,
                    scope_depth,
                });
                self.start_fixpoint(root, Vec::new())?
            }
            Request::Specialized(request, arguments) => {
                let checkpoint = branch.retry_candidate();
                let mut activity = RequestActivity::default();
                let result = self.specialization.handle_request(
                    request,
                    arguments,
                    &mut RequestContext {
                        eval_context: &self.eval_context,
                        host: &self.host,
                        transaction: branch.transaction.as_mut(),
                        activity: &mut activity,
                    },
                )?;
                if let Some(generation) = activity.observed_generation {
                    branch.observe(checkpoint.clone(), generation);
                }
                if activity.committed {
                    branch.retry = None;
                }
                match result {
                    RequestResult::Return(value) => MachineWork::Deliver {
                        value: value.into_core(),
                        branch,
                        scope_depth,
                    },
                    RequestResult::ReturnUnit => MachineWork::Deliver {
                        value: unit_value(),
                        branch,
                        scope_depth,
                    },
                    RequestResult::Fail => MachineWork::Outcome {
                        outcome: branch.into_failure(),
                        scope_depth,
                    },
                    RequestResult::Cancelled => MachineWork::Outcome {
                        outcome: BranchOutcome::Cancelled,
                        scope_depth,
                    },
                }
            }
        };
        Ok(MachineStep::Continue(work))
    }

    fn deliver_step(
        &mut self,
        value: Value,
        mut branch: Branch<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskError> {
        if let Some(continuation) = branch.control.sequence.last().cloned() {
            return match continuation {
                Continuation::Glam(function) => {
                    let function = evaluate(&self.eval_context, function)?;
                    branch.control.sequence.pop();
                    Ok(MachineStep::Continue(MachineWork::Apply {
                        function,
                        arguments: vec![value],
                        branch,
                        scope_depth,
                    }))
                }
                Continuation::RequireUnit => {
                    let value = evaluate(&self.eval_context, value)?;
                    if value != unit_value() {
                        return Err(TaskError::new(format!(
                            "`=>>` requires discarded effect results to be unit, got {value:?}"
                        )));
                    }
                    branch.control.sequence.pop();
                    Ok(MachineStep::Continue(MachineWork::Deliver {
                        value: unit_value(),
                        branch,
                        scope_depth,
                    }))
                }
                Continuation::Fix(handle) => {
                    let active = branch.active_fixes.last().ok_or_else(|| {
                        TaskError::new("reflection fixpoint lost its active branch")
                    })?;
                    if active.handle != handle {
                        return Err(TaskError::new(
                            "reflection fixpoint control became unbalanced",
                        ));
                    }
                    if active.next_choice != active.choices.len() {
                        return Err(TaskError::new("reflection fixpoint choice replay diverged"));
                    }
                    handle
                        .set(value.clone())
                        .map_err(|_| TaskError::new("reflection fixpoint initialized twice"))?;
                    branch.control.sequence.pop();
                    branch.active_fixes.pop();
                    Ok(MachineStep::Continue(MachineWork::Deliver {
                        value,
                        branch,
                        scope_depth,
                    }))
                }
            };
        }

        let mut resets = reset_frames(
            &self.eval_context,
            &branch.state,
            &self.tags.continuation_state,
        )?;
        let reset_order = resets
            .last()
            .filter(|frame| frame.scope_depth >= scope_depth)
            .map(|frame| frame.order);
        let delimiter_order = branch
            .control
            .delimiters
            .last()
            .filter(|delimiter| delimiter.scope_depth() >= scope_depth)
            .map(Delimiter::order);
        if reset_order > delimiter_order {
            let frame = resets.pop().expect("reset order came from a frame");
            branch.state =
                replace_reset_frames(branch.state, &self.tags.continuation_state, &resets);
            return Ok(MachineStep::Continue(MachineWork::Apply {
                function: frame.continuation,
                arguments: vec![value],
                branch,
                scope_depth,
            }));
        }
        let Some(_) = delimiter_order else {
            return Ok(MachineStep::Continue(MachineWork::Outcome {
                outcome: BranchOutcome::Complete(value, branch),
                scope_depth,
            }));
        };
        match branch
            .control
            .delimiters
            .last()
            .cloned()
            .expect("delimiter order came from a delimiter")
        {
            Delimiter::Resume { outer_sequence, .. } => {
                branch.control.delimiters.pop();
                branch.control.sequence = outer_sequence;
            }
            Delimiter::Restore {
                outer, reset_stack, ..
            } => {
                let state = with_reset_stack_value(
                    &self.eval_context,
                    branch.state.clone(),
                    &self.tags.continuation_state,
                    reset_stack,
                )?;
                branch.control.delimiters.pop();
                branch.state = state;
                branch.control = *outer;
            }
        }
        Ok(MachineStep::Continue(MachineWork::Deliver {
            value,
            branch,
            scope_depth,
        }))
    }

    fn enter_cut(
        &mut self,
        operation: Value,
        mut outer: Branch<S>,
        parent_scope_depth: usize,
    ) -> MachineWork<S> {
        let outer_sequence = std::mem::take(&mut outer.control.sequence);
        let mut frame = CutFrame {
            operation,
            outer,
            outer_sequence,
            parent_scope_depth,
            scope_depth: parent_scope_depth + 1,
            owns_transaction: false,
            alternatives: Vec::new(),
            retry: None,
            observed_failure: false,
        };
        frame.owns_transaction = frame.outer.transaction.is_none();
        self.begin_cut_attempt(&mut frame);
        let work = frame.next_alternative();
        self.execution.cuts.push(frame);
        work
    }

    fn begin_cut_attempt(&mut self, frame: &mut CutFrame<S>) {
        frame.alternatives.clear();
        frame.retry = None;
        frame.observed_failure = false;
        if frame.owns_transaction {
            let snapshot = self.host.snapshot();
            frame.outer.transaction = Some(Transaction::new(snapshot));
        }
        let mut initial = frame.outer.clone().with_effect(frame.operation.clone());
        initial.control.sequence.clear();
        frame.alternatives.push(initial);
    }

    fn handle_outcome(
        &mut self,
        outcome: BranchOutcome<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskError> {
        if self.execution.cuts.is_empty() {
            return self.handle_top_level_outcome(outcome, scope_depth);
        }
        let expected_scope = self
            .execution
            .cuts
            .last()
            .expect("checked nonempty cut stack")
            .scope_depth;
        if scope_depth != expected_scope {
            return Err(TaskError::new(
                "reflection cut stack became unbalanced during polling",
            ));
        }

        match outcome {
            BranchOutcome::Complete(value, mut completed) => {
                let owns_transaction = self
                    .execution
                    .cuts
                    .last()
                    .expect("checked nonempty cut stack")
                    .owns_transaction;
                if owns_transaction {
                    let transaction = completed
                        .transaction
                        .as_ref()
                        .expect("outer cut must own a transaction");
                    let commit = TaskCommit::new(
                        transaction.store.clone(),
                        transaction.snapshot.extra().clone(),
                        transaction.journal.clone(),
                    );
                    match self.host.commit(commit) {
                        CommitResult::Committed => {
                            completed.transaction = None;
                        }
                        CommitResult::Conflict => {
                            let frame = self
                                .execution
                                .cuts
                                .last_mut()
                                .expect("checked nonempty cut stack");
                            frame.observed_failure = true;
                            frame.retry = Some(completed);
                            return self.finish_cut_attempt();
                        }
                        CommitResult::MissingVolume(volume) => {
                            return Err(missing_volume_error(volume));
                        }
                        CommitResult::Closed => {
                            let parent_scope = self
                                .execution
                                .cuts
                                .pop()
                                .expect("checked nonempty cut stack")
                                .parent_scope_depth;
                            return Ok(MachineStep::Continue(MachineWork::Outcome {
                                outcome: BranchOutcome::Cancelled,
                                scope_depth: parent_scope,
                            }));
                        }
                    }
                }
                let frame = self
                    .execution
                    .cuts
                    .pop()
                    .expect("checked nonempty cut stack");
                completed.control.sequence = frame.outer_sequence;
                Ok(MachineStep::Continue(MachineWork::Deliver {
                    value,
                    branch: completed,
                    scope_depth: frame.parent_scope_depth,
                }))
            }
            BranchOutcome::Fork(left, right) => {
                let frame = self
                    .execution
                    .cuts
                    .last_mut()
                    .expect("checked nonempty cut stack");
                frame.alternatives.push(*right);
                frame.alternatives.push(*left);
                Ok(MachineStep::Continue(frame.next_alternative()))
            }
            BranchOutcome::Fail(mut failed) | BranchOutcome::Retry(mut failed) => {
                if let Some(restarted) = self.restart_fixpoint_at_scope(&mut failed, scope_depth)? {
                    return Ok(MachineStep::Continue(restarted));
                }
                let frame = self
                    .execution
                    .cuts
                    .last_mut()
                    .expect("checked nonempty cut stack");
                frame.observed_failure |= failed
                    .transaction
                    .as_ref()
                    .is_some_and(|transaction| transaction.observed);
                frame.retry = Some(failed);
                if !frame.alternatives.is_empty() {
                    return Ok(MachineStep::Continue(frame.next_alternative()));
                }
                self.finish_cut_attempt()
            }
            BranchOutcome::Cancelled => {
                let parent_scope = self
                    .execution
                    .cuts
                    .pop()
                    .expect("checked nonempty cut stack")
                    .parent_scope_depth;
                Ok(MachineStep::Continue(MachineWork::Outcome {
                    outcome: BranchOutcome::Cancelled,
                    scope_depth: parent_scope,
                }))
            }
        }
    }

    fn finish_cut_attempt(&mut self) -> Result<MachineStep<S>, TaskError> {
        let mut frame = self
            .execution
            .cuts
            .pop()
            .expect("cut attempt requires a cut frame");
        let mut failed = frame.retry.take().unwrap_or_else(|| frame.outer.clone());
        if frame.observed_failure
            && let Some(transaction) = failed.transaction.as_mut()
        {
            transaction.observed = true;
        }
        if failed
            .fix_restarts
            .last()
            .is_some_and(|restart| restart.root.scope_depth < frame.scope_depth)
        {
            return Ok(MachineStep::Continue(MachineWork::Outcome {
                outcome: BranchOutcome::Retry(failed),
                scope_depth: frame.parent_scope_depth,
            }));
        }
        if !frame.owns_transaction {
            return Ok(MachineStep::Continue(MachineWork::Outcome {
                outcome: failed.into_failure(),
                scope_depth: frame.parent_scope_depth,
            }));
        }
        if !frame.observed_failure {
            failed.transaction = None;
            return Ok(MachineStep::Continue(MachineWork::Outcome {
                outcome: failed.into_failure(),
                scope_depth: frame.parent_scope_depth,
            }));
        }

        let generation = failed
            .transaction
            .as_ref()
            .map(|transaction| transaction.snapshot.generation())
            .unwrap_or_else(|| self.host.snapshot().generation());
        frame.retry = Some(failed);
        let index = self.execution.cuts.len();
        self.execution.cuts.push(frame);
        Ok(MachineStep::Blocked(BlockedExecution::exhausted(
            RetryWake {
                observed_generation: generation,
                action: WakeAction::RestartCut(index),
            },
        )))
    }

    fn handle_top_level_outcome(
        &mut self,
        outcome: BranchOutcome<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskError> {
        if self.search.retains_all() {
            return self.handle_isolated_search_outcome(outcome, scope_depth);
        }
        match outcome {
            BranchOutcome::Complete(value, _) => Ok(MachineStep::Terminal(TaskTerminal::Complete(
                PublicValue::from_core(value),
            ))),
            BranchOutcome::Fail(_) => Ok(MachineStep::Terminal(TaskTerminal::Failed(
                TaskError::new("reflection task failed permanently"),
            ))),
            BranchOutcome::Fork(_, _) => Ok(MachineStep::Terminal(TaskTerminal::Failed(
                TaskError::new("`.alt` requires an enclosing `.cut`"),
            ))),
            BranchOutcome::Retry(mut failed) => {
                if let Some(restarted) = self.restart_fixpoint_at_scope(&mut failed, scope_depth)? {
                    return Ok(MachineStep::Continue(restarted));
                }
                let checkpoint = failed.retry.take().ok_or_else(|| {
                    TaskError::new("retryable reflection failure lost its observation")
                })?;
                let generation = checkpoint.generation.ok_or_else(|| {
                    TaskError::new("retryable reflection failure lost its wake generation")
                })?;
                Ok(MachineStep::Blocked(BlockedExecution::exhausted(
                    RetryWake {
                        observed_generation: generation,
                        action: WakeAction::ReplaceWork(Box::new(MachineWork::Drive {
                            branch: *checkpoint.branch,
                            scope_depth,
                        })),
                    },
                )))
            }
            BranchOutcome::Cancelled => Ok(MachineStep::Terminal(TaskTerminal::Cancelled)),
        }
    }

    fn handle_isolated_search_outcome(
        &mut self,
        outcome: BranchOutcome<S>,
        scope_depth: usize,
    ) -> Result<MachineStep<S>, TaskError> {
        debug_assert_eq!(scope_depth, 0);
        match outcome {
            BranchOutcome::Complete(value, mut completed) => {
                let restarted = self.restart_fixpoint_at_scope(&mut completed, scope_depth)?;
                let transaction = completed
                    .transaction
                    .take()
                    .expect("an isolated outer branch must retain its transaction");
                self.search.retain(IsolatedSearchResult::new(
                    PublicValue::from_core(value),
                    TaskCommit::new(
                        transaction.store,
                        transaction.snapshot.extra().clone(),
                        transaction.journal,
                    ),
                ));
                Ok(restarted
                    .map(MachineStep::Continue)
                    .unwrap_or_else(|| self.advance_isolated_search()))
            }
            BranchOutcome::Fork(left, right) => {
                let branch = self
                    .search
                    .fork(*left, *right)
                    .expect("isolated search must accept an outer alternative");
                Ok(MachineStep::Continue(MachineWork::Drive {
                    branch,
                    scope_depth: 0,
                }))
            }
            BranchOutcome::Fail(mut failed) => {
                if let Some(restarted) = self.restart_fixpoint_at_scope(&mut failed, scope_depth)? {
                    return Ok(MachineStep::Continue(restarted));
                }
                Ok(self.advance_isolated_search())
            }
            BranchOutcome::Retry(mut failed) => {
                if let Some(restarted) = self.restart_fixpoint_at_scope(&mut failed, scope_depth)? {
                    return Ok(MachineStep::Continue(restarted));
                }
                let generation = failed
                    .transaction
                    .as_ref()
                    .filter(|transaction| transaction.observed)
                    .map(|transaction| transaction.snapshot.generation())
                    .or_else(|| {
                        failed
                            .retry
                            .as_ref()
                            .and_then(|checkpoint| checkpoint.generation)
                    })
                    .ok_or_else(|| {
                        TaskError::new("retryable isolated branch lost its wake generation")
                    })?;
                Ok(MachineStep::Blocked(BlockedExecution::exhausted(
                    RetryWake {
                        observed_generation: generation,
                        action: WakeAction::RestartSearch,
                    },
                )))
            }
            BranchOutcome::Cancelled => Ok(MachineStep::Terminal(TaskTerminal::Cancelled)),
        }
    }

    fn advance_isolated_search(&mut self) -> MachineStep<S> {
        if let Some(branch) = self.search.next_alternative() {
            MachineStep::Continue(MachineWork::Drive {
                branch,
                scope_depth: 0,
            })
        } else {
            self.search.finish();
            MachineStep::Terminal(TaskTerminal::Complete(PublicValue::from_core(unit_value())))
        }
    }

    fn waiting_block(&self, wait: EvaluationWaitToken) -> BlockedExecution<S> {
        BlockedExecution::waiting_on(wait, self.retry_wake())
    }

    fn retry_wake(&self) -> Option<RetryWake<S>> {
        if let Some(index) = self
            .execution
            .cuts
            .iter()
            .rposition(|frame| frame.owns_transaction)
        {
            let frame_observed = self.execution.cuts[index..]
                .iter()
                .any(|frame| frame.observed_failure);
            let branch_observed = self
                .execution
                .work
                .branch()
                .and_then(|branch| branch.transaction.as_ref())
                .is_some_and(|transaction| transaction.observed);
            if frame_observed || branch_observed {
                let generation = self.execution.cuts[index]
                    .outer
                    .transaction
                    .as_ref()
                    .map(|transaction| transaction.snapshot.generation());
                if let Some(generation) = generation {
                    return Some(RetryWake {
                        observed_generation: generation,
                        action: WakeAction::RestartCut(index),
                    });
                }
            }
        }
        if self.search.retains_all()
            && let Some(transaction) = self
                .execution
                .work
                .branch()
                .and_then(|branch| branch.transaction.as_ref())
            && transaction.observed
        {
            return Some(RetryWake {
                observed_generation: transaction.snapshot.generation(),
                action: WakeAction::RestartSearch,
            });
        }
        let branch = self.execution.work.branch()?;
        let checkpoint = branch.retry.as_ref()?;
        let observed_generation = checkpoint.generation?;
        Some(RetryWake {
            observed_generation,
            action: WakeAction::ReplaceWork(Box::new(MachineWork::Drive {
                branch: (*checkpoint.branch).clone(),
                scope_depth: self.execution.work.scope_depth(),
            })),
        })
    }

    fn poll_blocked(&mut self) -> Option<EffectTaskPoll> {
        let blocked = self.blocked.as_ref()?;
        if let Some(retry) = &blocked.retry
            && self.host.snapshot().generation() != retry.observed_generation
        {
            let retry = self
                .blocked
                .take()
                .and_then(|blocked| blocked.retry)
                .expect("changed retry generation must retain its wake action");
            self.apply_wake(retry.action);
            return None;
        }
        let BlockReason::WaitingOn(wait) = &blocked.reason else {
            return Some(self.blocked_poll());
        };
        match self.eval_context.poll_wait(wait) {
            EvaluationTaskPoll::Pending(_) => {
                if matches!(
                    self.eval_context.pump_wait(wait, 256),
                    EvaluationPumpOutcome::TargetReady
                ) {
                    self.blocked = None;
                    None
                } else {
                    Some(self.blocked_poll())
                }
            }
            EvaluationTaskPoll::Complete(_)
            | EvaluationTaskPoll::Failed(_)
            | EvaluationTaskPoll::Cancelled
            | EvaluationTaskPoll::ForeignSession => {
                self.blocked = None;
                None
            }
        }
    }

    fn blocked_poll(&self) -> EffectTaskPoll {
        let blocked = self
            .blocked
            .as_ref()
            .expect("blocked poll requires blocked state");
        EffectTaskPoll::Blocked(TaskBlock {
            lazy: blocked.lazy(),
            observed_generation: blocked.observed_generation(),
            error: blocked.error(),
        })
    }

    fn apply_wake(&mut self, wake: WakeAction<S>) {
        match wake {
            WakeAction::ReplaceWork(work) => self.execution.work = *work,
            WakeAction::RestartCut(index) => self.restart_cut(index),
            WakeAction::RestartSearch => self.restart_search(),
        }
    }

    fn restart_search(&mut self) {
        self.execution.cuts.clear();
        let mut root = self
            .search
            .restart()
            .expect("only isolated search can restart its outer boundary");
        root.transaction = Some(Transaction::new(self.host.snapshot()));
        self.execution.work = MachineWork::Drive {
            branch: root,
            scope_depth: 0,
        };
    }

    fn restart_cut(&mut self, index: usize) {
        self.execution.cuts.truncate(index + 1);
        let mut frame = self
            .execution
            .cuts
            .pop()
            .expect("blocked cut must remain on the cut stack");
        frame.outer.transaction = None;
        self.begin_cut_attempt(&mut frame);
        let work = frame.next_alternative();
        self.execution.cuts.push(frame);
        self.execution.work = work;
    }

    fn finish(&mut self, terminal: TaskTerminal) {
        if self.terminal.is_some() {
            return;
        }
        let unfinished_reason: Arc<str> = match &terminal {
            TaskTerminal::Complete(_) => {
                Arc::from("reflection task completed without fulfilling its fixpoint")
            }
            TaskTerminal::Cancelled => Arc::from("reflection fixpoint producer was cancelled"),
            TaskTerminal::Failed(error) => {
                Arc::from(format!("reflection fixpoint producer failed: {error}"))
            }
        };
        self.eval_context
            .fail_unresolved_promises(unfinished_reason);
        self.blocked = None;
        self.terminal = Some(terminal);
    }

    fn effect_request(&self, effect: Value) -> Result<Request<S::Request>, TaskError> {
        let effect = evaluate(&self.eval_context, effect)?;
        let Value::Dict(effect) = effect else {
            return Err(TaskError::new(format!(
                "reflection task requires an effect object, got {effect:?}"
            )));
        };
        let function = effect
            .get(&*keys::EFF)
            .cloned()
            .ok_or_else(|| TaskError::new("reflection effect has no `eff` member"))?;
        let function = evaluate(&self.eval_context, function).map_err(|error| {
            TaskError::new(format!(
                "reflection effect function could not be evaluated: {error}"
            ))
        })?;
        let request =
            apply(&self.eval_context, function, vec![self.api.clone()]).map_err(|error| {
                TaskError::new(format!(
                    "reflection effect function could not be applied: {error}"
                ))
            })?;
        let request = evaluate(&self.eval_context, request).map_err(|error| {
            TaskError::new(format!(
                "reflection effect application could not be evaluated: {error}"
            ))
        })?;
        parse_request(
            &self.eval_context,
            request,
            &self.tags,
            &self.specialized_requests,
            self.host.reasoning_session_id(),
        )
    }
}

struct AnnotationEffectTask<S: TaskSpecialization>(EffectTask<S>);

struct JoinableEffectTask<S: TaskSpecialization>(EffectTask<S>);

impl<S: TaskSpecialization> EvaluationTaskMachine for JoinableEffectTask<S> {
    fn poll(&mut self, step_budget: usize) -> EvaluationMachinePoll {
        match self.0.poll(step_budget) {
            EffectTaskPoll::Yielded => EvaluationMachinePoll::Yielded,
            EffectTaskPoll::Blocked(blocked) => {
                EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    lazy: blocked.lazy,
                    observed_generation: blocked.observed_generation,
                    error: blocked.error,
                })
            }
            EffectTaskPoll::Complete(value) => EvaluationMachinePoll::Complete(value.into_core()),
            EffectTaskPoll::Failed(error) => {
                EvaluationMachinePoll::Failed(Arc::from(error.to_string()))
            }
            EffectTaskPoll::Cancelled => EvaluationMachinePoll::Cancelled,
        }
    }

    fn cancel(&mut self) {
        self.0.finish(TaskTerminal::Cancelled);
    }
}

impl<S: TaskSpecialization> EvaluationTaskMachine for AnnotationEffectTask<S> {
    fn poll(&mut self, step_budget: usize) -> EvaluationMachinePoll {
        match self.0.poll(step_budget) {
            EffectTaskPoll::Yielded => EvaluationMachinePoll::Yielded,
            EffectTaskPoll::Blocked(blocked) => {
                EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    lazy: blocked.lazy,
                    observed_generation: blocked.observed_generation,
                    error: blocked.error,
                })
            }
            EffectTaskPoll::Complete(value) if value.as_core() == &*keys::UNIT_VALUE => {
                EvaluationMachinePoll::Complete((*keys::UNIT_VALUE).clone())
            }
            EffectTaskPoll::Complete(value) => EvaluationMachinePoll::Failed(Arc::from(format!(
                "reflection annotation requires its effect to return unit, got {:?}",
                value.as_core()
            ))),
            EffectTaskPoll::Failed(error) => {
                EvaluationMachinePoll::Failed(Arc::from(error.to_string()))
            }
            EffectTaskPoll::Cancelled => EvaluationMachinePoll::Cancelled,
        }
    }

    fn cancel(&mut self) {
        self.0.finish(TaskTerminal::Cancelled);
    }
}

#[derive(Clone)]
struct Branch<S: TaskSpecialization> {
    effect: Value,
    control: Control,
    state: Value,
    transaction: Option<Transaction<S>>,
    active_fixes: Vec<ActiveFix<S>>,
    fix_restarts: Vec<FixRestart<S>>,
    retry: Option<RetryCheckpoint<S>>,
}

impl<S: TaskSpecialization> Branch<S> {
    fn new(effect: Value, state: Value) -> Self {
        Self {
            effect,
            control: Control::default(),
            state,
            transaction: None,
            active_fixes: Vec::new(),
            fix_restarts: Vec::new(),
            retry: None,
        }
    }

    fn with_effect(&self, effect: Value) -> Self {
        let mut branch = self.clone();
        branch.effect = effect;
        branch
    }

    fn retry_candidate(&self) -> Option<Box<Self>> {
        if self.transaction.is_some() || self.retry.is_some() {
            return None;
        }
        let mut checkpoint = self.clone();
        checkpoint.retry = None;
        Some(Box::new(checkpoint))
    }

    fn observe(&mut self, checkpoint: Option<Box<Self>>, generation: u64) {
        if let Some(transaction) = self.transaction.as_mut() {
            transaction.observed = true;
        } else if self.retry.is_none()
            && let Some(branch) = checkpoint
        {
            self.retry = Some(RetryCheckpoint {
                generation: Some(generation),
                branch,
            });
        }
    }

    fn is_retryable(&self) -> bool {
        self.retry.is_some()
            || self
                .transaction
                .as_ref()
                .is_some_and(|transaction| transaction.observed)
    }

    fn into_failure(self) -> BranchOutcome<S> {
        if self.is_retryable() {
            BranchOutcome::Retry(self)
        } else {
            BranchOutcome::Fail(self)
        }
    }
}

#[derive(Clone)]
struct TaskExecution<S: TaskSpecialization> {
    work: MachineWork<S>,
    cuts: Vec<CutFrame<S>>,
}

#[derive(Clone)]
enum MachineWork<S: TaskSpecialization> {
    Drive {
        branch: Branch<S>,
        scope_depth: usize,
    },
    Deliver {
        value: Value,
        branch: Branch<S>,
        scope_depth: usize,
    },
    Apply {
        function: Value,
        arguments: Vec<Value>,
        branch: Branch<S>,
        scope_depth: usize,
    },
    Outcome {
        outcome: BranchOutcome<S>,
        scope_depth: usize,
    },
}

impl<S: TaskSpecialization> MachineWork<S> {
    fn branch(&self) -> Option<&Branch<S>> {
        match self {
            Self::Drive { branch, .. }
            | Self::Deliver { branch, .. }
            | Self::Apply { branch, .. } => Some(branch),
            Self::Outcome { outcome, .. } => outcome.branch(),
        }
    }

    fn branch_mut(&mut self) -> Option<&mut Branch<S>> {
        match self {
            Self::Drive { branch, .. }
            | Self::Deliver { branch, .. }
            | Self::Apply { branch, .. } => Some(branch),
            Self::Outcome { outcome, .. } => outcome.branch_mut(),
        }
    }

    fn scope_depth(&self) -> usize {
        match self {
            Self::Drive { scope_depth, .. }
            | Self::Deliver { scope_depth, .. }
            | Self::Apply { scope_depth, .. }
            | Self::Outcome { scope_depth, .. } => *scope_depth,
        }
    }
}

#[derive(Clone)]
enum BranchOutcome<S: TaskSpecialization> {
    Complete(Value, Branch<S>),
    Fork(Box<Branch<S>>, Box<Branch<S>>),
    Fail(Branch<S>),
    Retry(Branch<S>),
    Cancelled,
}

impl<S: TaskSpecialization> BranchOutcome<S> {
    fn branch(&self) -> Option<&Branch<S>> {
        match self {
            Self::Complete(_, branch) | Self::Fail(branch) | Self::Retry(branch) => Some(branch),
            Self::Fork(left, _) => Some(left),
            Self::Cancelled => None,
        }
    }

    fn branch_mut(&mut self) -> Option<&mut Branch<S>> {
        match self {
            Self::Complete(_, branch) | Self::Fail(branch) | Self::Retry(branch) => Some(branch),
            Self::Fork(left, _) => Some(left),
            Self::Cancelled => None,
        }
    }
}

#[derive(Clone)]
struct CutFrame<S: TaskSpecialization> {
    operation: Value,
    outer: Branch<S>,
    outer_sequence: Vec<Continuation>,
    parent_scope_depth: usize,
    scope_depth: usize,
    owns_transaction: bool,
    alternatives: Vec<Branch<S>>,
    retry: Option<Branch<S>>,
    observed_failure: bool,
}

impl<S: TaskSpecialization> CutFrame<S> {
    fn next_alternative(&mut self) -> MachineWork<S> {
        MachineWork::Drive {
            branch: self
                .alternatives
                .pop()
                .expect("cut attempt must have another alternative"),
            scope_depth: self.scope_depth,
        }
    }
}

// This value is short-lived on the Rust stack. Boxing `Continue` would add an
// allocation to every cooperative machine transition merely to shrink the two
// uncommon terminal variants.
#[allow(clippy::large_enum_variant)]
enum MachineStep<S: TaskSpecialization> {
    Continue(MachineWork<S>),
    Blocked(BlockedExecution<S>),
    Terminal(TaskTerminal),
}

struct BlockedExecution<S: TaskSpecialization> {
    reason: BlockReason,
    retry: Option<RetryWake<S>>,
}

impl<S: TaskSpecialization> BlockedExecution<S> {
    fn waiting_on(wait: EvaluationWaitToken, retry: Option<RetryWake<S>>) -> Self {
        Self {
            reason: BlockReason::WaitingOn(wait),
            retry,
        }
    }

    fn exhausted(retry: RetryWake<S>) -> Self {
        Self {
            reason: BlockReason::Exhausted,
            retry: Some(retry),
        }
    }

    fn evaluation_error(error: TaskError, retry: RetryWake<S>) -> Self {
        debug_assert!(error.blocked_on().is_none());
        Self {
            reason: BlockReason::EvaluationError(error),
            retry: Some(retry),
        }
    }

    fn lazy(&self) -> Option<EvaluationWaitToken> {
        match &self.reason {
            BlockReason::WaitingOn(wait) => Some(wait.clone()),
            BlockReason::Exhausted | BlockReason::EvaluationError(_) => None,
        }
    }

    fn observed_generation(&self) -> Option<u64> {
        self.retry.as_ref().map(|retry| retry.observed_generation)
    }

    fn error(&self) -> Option<Arc<str>> {
        match &self.reason {
            BlockReason::EvaluationError(error) => Some(Arc::from(error.to_string())),
            BlockReason::WaitingOn(_) | BlockReason::Exhausted => None,
        }
    }
}

enum BlockReason {
    WaitingOn(EvaluationWaitToken),
    Exhausted,
    EvaluationError(TaskError),
}

struct RetryWake<S: TaskSpecialization> {
    observed_generation: u64,
    action: WakeAction<S>,
}

enum WakeAction<S: TaskSpecialization> {
    ReplaceWork(Box<MachineWork<S>>),
    RestartCut(usize),
    RestartSearch,
}

struct TaskBlock {
    lazy: Option<EvaluationWaitToken>,
    observed_generation: Option<u64>,
    error: Option<Arc<str>>,
}

enum EffectTaskPoll {
    Yielded,
    Blocked(TaskBlock),
    Complete(PublicValue),
    Failed(TaskError),
    Cancelled,
}

#[derive(Clone)]
enum TaskTerminal {
    Complete(PublicValue),
    Failed(TaskError),
    Cancelled,
}

impl TaskTerminal {
    fn poll(&self) -> EffectTaskPoll {
        match self {
            Self::Complete(value) => EffectTaskPoll::Complete(value.clone()),
            Self::Failed(error) => EffectTaskPoll::Failed(error.clone()),
            Self::Cancelled => EffectTaskPoll::Cancelled,
        }
    }
}

#[derive(Clone)]
struct RetryCheckpoint<S: TaskSpecialization> {
    generation: Option<u64>,
    branch: Box<Branch<S>>,
}

#[derive(Clone)]
struct FixRoot<S: TaskSpecialization> {
    function: Value,
    entry: Branch<S>,
    scope_depth: usize,
}

#[derive(Clone)]
struct ActiveFix<S: TaskSpecialization> {
    root: Arc<FixRoot<S>>,
    choices: Vec<FixChoice>,
    next_choice: usize,
    handle: PromisedValue,
}

#[derive(Clone)]
struct FixRestart<S: TaskSpecialization> {
    root: Arc<FixRoot<S>>,
    choices: Vec<FixChoice>,
    inherited_restarts: Vec<FixRestart<S>>,
}

#[derive(Clone, Copy)]
enum FixChoice {
    Left,
    Right,
}

#[derive(Clone, Default)]
struct Control {
    sequence: Vec<Continuation>,
    delimiters: Vec<Delimiter>,
}

#[derive(Clone)]
enum Continuation {
    Glam(Value),
    RequireUnit,
    Fix(PromisedValue),
}

#[derive(Clone)]
enum Delimiter {
    Resume {
        outer_sequence: Vec<Continuation>,
        scope_depth: usize,
        order: usize,
    },
    Restore {
        outer: Box<Control>,
        reset_stack: Value,
        scope_depth: usize,
        order: usize,
    },
}

impl Delimiter {
    fn scope_depth(&self) -> usize {
        match self {
            Self::Resume { scope_depth, .. } | Self::Restore { scope_depth, .. } => *scope_depth,
        }
    }

    fn order(&self) -> usize {
        match self {
            Self::Resume { order, .. } | Self::Restore { order, .. } => *order,
        }
    }

    fn rebase(&mut self, scope_depth: usize, order: usize) {
        match self {
            Self::Resume {
                scope_depth: depth,
                order: position,
                ..
            }
            | Self::Restore {
                scope_depth: depth,
                order: position,
                ..
            } => {
                *depth = scope_depth;
                *position = order;
            }
        }
    }
}

#[derive(Clone)]
struct CapturedContinuation {
    sequence: Vec<Continuation>,
    delimiters: Vec<Delimiter>,
    reset_frames: Vec<ResetFrame>,
}

#[derive(Clone)]
struct ResetFrame {
    // Reset frames are encoded as ordinary Values under continuation_state.
    // scope_depth and order preserve nesting with the handler's temporary
    // cut/resume/fix control without creating a second authoritative stack.
    key: Key,
    continuation: Value,
    scope_depth: usize,
    order: usize,
}

enum CapturedLayer {
    Reset(ResetFrame),
    Delimiter(Delimiter),
}

impl CapturedLayer {
    fn order(&self) -> usize {
        match self {
            Self::Reset(frame) => frame.order,
            Self::Delimiter(delimiter) => delimiter.order(),
        }
    }
}

#[derive(Clone)]
struct Transaction<S: TaskSpecialization> {
    snapshot: HostSnapshot<S>,
    store: StoreJournal,
    journal: S::Journal,
    observed: bool,
}

impl<S: TaskSpecialization> Transaction<S> {
    fn new(snapshot: HostSnapshot<S>) -> Self {
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
    eval_context: &'a EvalContext,
    host: &'a Arc<S::Host>,
    transaction: Option<&'a mut Transaction<S>>,
    activity: &'a mut RequestActivity,
}

impl<'a, S: TaskSpecialization> RequestContext<'a, S> {
    pub(crate) fn eval_context(&self) -> &EvalContext {
        self.eval_context
    }

    pub fn host(&self) -> &S::Host {
        self.host.as_ref()
    }

    pub(crate) fn host_arc(&self) -> Arc<S::Host> {
        self.host.clone()
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

enum Request<R> {
    Return(Value),
    Seq(Value, Value),
    Alt(Value, Value),
    Fail,
    Cut(Value),
    Fix(Value),
    Get(Value),
    Set(Value, Value),
    HeapGet(Value),
    HeapSet(Value, Value),
    HeapRewrite(Value, Value),
    VolumeGet(VolumeId, Value),
    VolumeSet(VolumeId, Value, Value),
    VolumeRewrite(VolumeId, Value, Value),
    Reset(Value, Value),
    Shift(Value, Value),
    Resume(EvaluationTaskId, u64, Value),
    Specialized(R, Vec<PublicValue>),
}

struct SpecializedRequest<R> {
    tag: Key,
    arity: usize,
    request: R,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VolumeOperation {
    Get,
    Set,
    Rewrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VolumeRequestIdentity {
    reasoning_session: ReasoningSessionId,
    volume: VolumeId,
    operation: VolumeOperation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VolumeRequestError {
    ForeignVolume(ReasoningSessionId, VolumeId),
}

impl fmt::Display for VolumeRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignVolume(reasoning_session, volume) => write!(
                formatter,
                "foreign reflection volume {} from reasoning session {}",
                volume.get(),
                reasoning_session.get()
            ),
        }
    }
}

fn parse_request<R: Clone>(
    context: &EvalContext,
    value: Value,
    tags: &Tags,
    specialized: &[SpecializedRequest<R>],
    reasoning_session: Option<ReasoningSessionId>,
) -> Result<Request<R>, TaskError> {
    let Value::Dict(dict) = value else {
        return Err(TaskError::new("effect API returned a non-request value"));
    };
    let parse = |tag: &Key| -> Result<Option<Vec<Value>>, TaskError> {
        dict.get(tag)
            .map(|payload| {
                let Value::List(payload) = evaluate(context, payload.clone())? else {
                    return Err(TaskError::new("effect request payload must be a list"));
                };
                eval::list_to_value_items(context, &payload).map_err(task_eval_error)
            })
            .transpose()
    };
    macro_rules! args {
        ($tag:expr, $n:literal, $body:expr) => {
            if let Some(arguments) = parse($tag)? {
                let arguments: [Value; $n] = arguments.try_into().map_err(|_| {
                    TaskError::new("effect request contained the wrong number of arguments")
                })?;
                return Ok(($body)(arguments));
            }
        };
    }
    args!(&tags.r, 1, |[value]: [Value; 1]| Request::Return(value));
    args!(&tags.seq, 2, |[operation, continuation]: [Value; 2]| {
        Request::Seq(operation, continuation)
    });
    args!(&tags.alt, 2, |[left, right]: [Value; 2]| Request::Alt(
        left, right
    ));
    args!(&tags.fail, 0, |[]: [Value; 0]| Request::Fail);
    args!(&tags.cut, 1, |[operation]: [Value; 1]| Request::Cut(
        operation
    ));
    args!(&tags.fix, 1, |[function]: [Value; 1]| Request::Fix(
        function
    ));
    args!(&tags.get, 1, |[path]: [Value; 1]| Request::Get(path));
    args!(&tags.set, 2, |[path, value]: [Value; 2]| Request::Set(
        path, value
    ));
    args!(&tags.heap_get, 1, |[path]: [Value; 1]| {
        Request::HeapGet(path)
    });
    args!(&tags.heap_set, 2, |[path, value]: [Value; 2]| {
        Request::HeapSet(path, value)
    });
    args!(&tags.heap_rewrite, 2, |[path, updater]: [Value; 2]| {
        Request::HeapRewrite(path, updater)
    });
    args!(&tags.reset, 2, |[key, operation]: [Value; 2]| {
        Request::Reset(key, operation)
    });
    args!(
        &tags.shift,
        2,
        |[key, function]: [Value; 2]| Request::Shift(key, function)
    );
    if let Some(arguments) = parse(&tags.resume)? {
        let [task_id, continuation_id, value]: [Value; 3] = arguments.try_into().map_err(|_| {
            TaskError::new("resume request contained the wrong number of arguments")
        })?;
        let task_id = request_id(context, task_id, "task")?;
        let task_id = EvaluationTaskId::from_u64(task_id)
            .ok_or_else(|| TaskError::new("reflection task ID must be nonzero"))?;
        return Ok(Request::Resume(
            task_id,
            request_id(context, continuation_id, "continuation")?,
            value,
        ));
    }
    for specialized in specialized {
        if let Some(arguments) = parse(&specialized.tag)? {
            if arguments.len() != specialized.arity {
                return Err(TaskError::new(
                    "effect request contained the wrong number of arguments",
                ));
            }
            return Ok(Request::Specialized(
                specialized.request.clone(),
                arguments.into_iter().map(PublicValue::from_core).collect(),
            ));
        }
    }
    for (tag, _) in dict.iter() {
        let Some(identity) = parse_volume_request_tag(tag)? else {
            continue;
        };
        if reasoning_session != Some(identity.reasoning_session) {
            return Err(TaskError::new(
                VolumeRequestError::ForeignVolume(identity.reasoning_session, identity.volume)
                    .to_string(),
            ));
        }
        let arguments = parse(tag)?.expect("the request tag came from this dictionary");
        return match (identity.operation, arguments.as_slice()) {
            (VolumeOperation::Get, [path]) => Ok(Request::VolumeGet(identity.volume, path.clone())),
            (VolumeOperation::Set, [path, value]) => Ok(Request::VolumeSet(
                identity.volume,
                path.clone(),
                value.clone(),
            )),
            (VolumeOperation::Rewrite, [path, updater]) => Ok(Request::VolumeRewrite(
                identity.volume,
                path.clone(),
                updater.clone(),
            )),
            _ => Err(TaskError::new(
                "volume capability request contained the wrong number of arguments",
            )),
        };
    }
    Err(TaskError::new("effect API returned an unknown request"))
}

const VOLUME_REQUEST_PREFIX: [&str; 3] = ["reflection_runtime", "v0", "volume"];

fn volume_request_tag(
    reasoning_session: ReasoningSessionId,
    volume: VolumeId,
    operation: VolumeOperation,
) -> Key {
    let operation = match operation {
        VolumeOperation::Get => "get",
        VolumeOperation::Set => "set",
        VolumeOperation::Rewrite => "rewrite",
    };
    Key::abstract_global_path([
        VOLUME_REQUEST_PREFIX[0].to_owned(),
        VOLUME_REQUEST_PREFIX[1].to_owned(),
        VOLUME_REQUEST_PREFIX[2].to_owned(),
        reasoning_session.get().to_string(),
        volume.get().to_string(),
        operation.to_owned(),
    ])
}

fn parse_volume_request_tag(tag: &Key) -> Result<Option<VolumeRequestIdentity>, TaskError> {
    let Key::AbstractGlobalPath(parts) = tag else {
        return Ok(None);
    };
    if parts.len() < VOLUME_REQUEST_PREFIX.len()
        || !parts
            .iter()
            .zip(VOLUME_REQUEST_PREFIX)
            .all(|(actual, expected)| actual == expected)
    {
        return Ok(None);
    }
    let [_, _, _, reasoning_session, volume, operation] = parts.as_ref() else {
        return Err(TaskError::new(
            "malformed private volume capability request",
        ));
    };
    let reasoning_session = reasoning_session
        .parse::<u64>()
        .ok()
        .and_then(ReasoningSessionId::from_u64)
        .ok_or_else(|| TaskError::new("volume capability has an invalid reasoning session"))?;
    let volume = volume
        .parse::<u64>()
        .ok()
        .and_then(VolumeId::from_u64)
        .ok_or_else(|| TaskError::new("volume capability has an invalid volume ID"))?;
    let operation = match operation.as_str() {
        "get" => VolumeOperation::Get,
        "set" => VolumeOperation::Set,
        "rewrite" => VolumeOperation::Rewrite,
        _ => return Err(TaskError::new("volume capability has an invalid operation")),
    };
    Ok(Some(VolumeRequestIdentity {
        reasoning_session,
        volume,
        operation,
    }))
}

pub(crate) fn volume_effects(
    reasoning_session: ReasoningSessionId,
    volume: VolumeId,
) -> PublicValue {
    let entry = |name: &str, operation, arity| {
        (
            Key::atom_from_text(name),
            request_function(
                volume_request_tag(reasoning_session, volume, operation),
                arity,
                Vec::new(),
                true,
            ),
        )
    };
    PublicValue::from_core(Value::Dict(
        [
            entry("get", VolumeOperation::Get, 1),
            entry("set", VolumeOperation::Set, 2),
            entry("rewrite", VolumeOperation::Rewrite, 2),
        ]
        .into_iter()
        .fold(Dict::new_sync(), |dict, (key, value)| {
            dict.insert(key, value)
        }),
    ))
}

fn request_id(context: &EvalContext, value: Value, kind: &str) -> Result<u64, TaskError> {
    let Value::Number(value) = evaluate(context, value)? else {
        return Err(TaskError::new(format!(
            "resume request has an invalid {kind} ID"
        )));
    };
    value
        .to_u64_if_integer()
        .ok_or_else(|| TaskError::new(format!("resume request has an invalid {kind} ID")))
}

fn effect_api<R: Clone>(
    tags: &Tags,
    specs: Vec<EffectRequestSpec<R>>,
) -> Result<(Value, Vec<SpecializedRequest<R>>), TaskError> {
    let entry = |name: &str, value| (Key::atom_from_text(name), value);
    let heap_api = Value::Dict(
        [
            entry(
                "get",
                request_function(tags.heap_get.clone(), 1, Vec::new(), false),
            ),
            entry(
                "set",
                request_function(tags.heap_set.clone(), 2, Vec::new(), false),
            ),
            entry(
                "rewrite",
                request_function(tags.heap_rewrite.clone(), 2, Vec::new(), false),
            ),
        ]
        .into_iter()
        .fold(Dict::new_sync(), |dict, (key, value)| {
            dict.insert(key, value)
        }),
    );
    let mut api = [
        entry("r", request_function(tags.r.clone(), 1, Vec::new(), false)),
        entry(
            "seq",
            request_function(tags.seq.clone(), 2, Vec::new(), false),
        ),
        entry(
            "alt",
            request_function(tags.alt.clone(), 2, Vec::new(), false),
        ),
        entry("fail", nullary_request(tags.fail.clone())),
        entry(
            "cut",
            request_function(tags.cut.clone(), 1, Vec::new(), false),
        ),
        entry(
            "fix",
            request_function(tags.fix.clone(), 1, Vec::new(), false),
        ),
        entry(
            "get",
            request_function(tags.get.clone(), 1, Vec::new(), false),
        ),
        entry(
            "set",
            request_function(tags.set.clone(), 2, Vec::new(), false),
        ),
        entry("heap", heap_api),
        entry(
            "reset",
            request_function(tags.reset.clone(), 2, Vec::new(), false),
        ),
        entry(
            "shift",
            request_function(tags.shift.clone(), 2, Vec::new(), false),
        ),
    ]
    .into_iter()
    .fold(Dict::new_sync(), |dict, (key, value)| {
        dict.insert(key, value)
    });
    let mut requests = Vec::with_capacity(specs.len());
    for spec in specs {
        let tag = Key::abstract_global_path(spec.tag_path.iter().map(Arc::as_ref));
        let api_name = spec
            .api_path
            .iter()
            .map(Arc::as_ref)
            .collect::<Vec<_>>()
            .join(".");
        if requests
            .iter()
            .any(|request: &SpecializedRequest<R>| request.tag == tag)
        {
            return Err(TaskError::new(format!(
                "duplicate private tag for effect API name `{}`",
                api_name
            )));
        }
        let value = if spec.arity == 0 {
            nullary_request(tag.clone())
        } else {
            request_function(tag.clone(), spec.arity, Vec::new(), false)
        };
        api = insert_effect_api_path(api, &spec.api_path, value, &api_name)?;
        requests.push(SpecializedRequest {
            tag,
            arity: spec.arity,
            request: spec.request,
        });
    }
    Ok((Value::Dict(api), requests))
}

fn insert_effect_api_path(
    api: Dict,
    path: &[Arc<str>],
    value: Value,
    display_path: &str,
) -> Result<Dict, TaskError> {
    let Some((name, rest)) = path.split_first() else {
        return Err(TaskError::new("effect API path must not be empty"));
    };
    let key = Key::atom_from_text(name);
    if rest.is_empty() {
        if api.get(&key).is_some() {
            return Err(TaskError::new(format!(
                "duplicate effect API name `{display_path}`"
            )));
        }
        return Ok(api.insert(key, value));
    }

    let nested = match api.get(&key) {
        Some(Value::Dict(nested)) => nested.clone(),
        Some(_) => {
            return Err(TaskError::new(format!(
                "effect API path `{display_path}` crosses non-dictionary `{name}`"
            )));
        }
        None => Dict::new_sync(),
    };
    let nested = insert_effect_api_path(nested, rest, value, display_path)?;
    Ok(api.insert(key, Value::Dict(nested)))
}

fn request_function(tag: Key, arity: usize, supplied: Vec<Value>, wrap_effect: bool) -> Value {
    let remaining = arity - supplied.len();
    let mut net = NetBuilder::<CoreSpecialization>::new();
    let exposed = net.unary_operator(eval::request_operator(
        tag,
        arity,
        Arc::from(supplied),
        wrap_effect,
    ));
    Value::Function(FunctionValue::new(
        NetValue::new(net.finish(exposed).instantiate_shared()),
        remaining,
    ))
}

fn nullary_request(tag: Key) -> Value {
    Value::Dict(Dict::new_sync().insert(tag, Value::List(List::empty())))
}

fn apply(
    context: &EvalContext,
    function: Value,
    arguments: Vec<Value>,
) -> Result<Value, TaskError> {
    eval::apply_values(context, function, arguments).map_err(task_eval_error)
}

fn evaluate(context: &EvalContext, value: Value) -> Result<Value, TaskError> {
    let mut value = value;
    while matches!(value, Value::Lazy(_) | Value::Promised(_)) {
        value = eval::eval_value(context, &value).map_err(task_eval_error)?;
    }
    Ok(value)
}

fn task_eval_error(error: eval::EvalError) -> TaskError {
    match error.blocked_on() {
        Some(wait) => TaskError::blocked(wait.0),
        None => TaskError::new(error.to_string()),
    }
}

fn missing_volume_error(volume: VolumeId) -> TaskError {
    TaskError::new(format!(
        "reflection volume {} was revoked before its edits committed",
        volume.get()
    ))
}

fn missing_volume_value(volume: VolumeId) -> Value {
    Value::error(format!(
        "reflection volume {} has been revoked",
        volume.get()
    ))
}

fn value_key(context: &EvalContext, value: Value) -> Result<Key, TaskError> {
    Key::from_value(&evaluate(context, value)?)
        .ok_or_else(|| TaskError::new("effect index is not keyable"))
}

fn get_value_path(context: &EvalContext, value: &Value, path: &[Key]) -> Result<Value, TaskError> {
    let mut current = value.clone();
    for key in path {
        let Value::Dict(dict) = evaluate(context, current)? else {
            return Err(TaskError::new(
                "state path traverses a non-dictionary value",
            ));
        };
        current = dict
            .get(key)
            .cloned()
            .unwrap_or_else(|| Value::Dict(Dict::new_sync()));
    }
    Ok(current)
}

fn lazy_value_path(value: Value, path: &[Key]) -> Value {
    if path.is_empty() {
        return value;
    }
    Value::Lazy(LazyValue::from_access(
        Arc::from(
            path.iter()
                .cloned()
                .map(CoreDataKey::Key)
                .collect::<Vec<_>>(),
        ),
        Arc::from([value]),
    ))
}

fn set_state_path(
    context: &EvalContext,
    state: Value,
    path: &Value,
    value: Value,
) -> Result<Value, TaskError> {
    let path = eval::eval_key_path_list(context, path).map_err(task_eval_error)?;
    if path.is_empty() {
        return require_state_dict(context, value);
    }
    let path = Value::List(List::from_values(path.into_iter().map(key_value).collect()));
    evaluate(
        context,
        Value::builtin_call(
            crate::core::Builtin::DictUpdate,
            vec![path, value, require_state_dict(context, state)?],
        ),
    )
}

fn require_state_dict(context: &EvalContext, value: Value) -> Result<Value, TaskError> {
    match evaluate(context, value)? {
        value @ Value::Dict(_) => Ok(value),
        _ => Err(TaskError::new("reflection user state must be a dictionary")),
    }
}

fn reset_stack_value(
    context: &EvalContext,
    state: &Value,
    continuation_state: &Key,
) -> Result<Value, TaskError> {
    let Value::Dict(state) = state else {
        return Err(TaskError::new("reflection user state must be a dictionary"));
    };
    let stack = state
        .get(continuation_state)
        .cloned()
        .unwrap_or_else(|| Value::List(List::empty()));
    reset_frames_from_value(context, &stack)?;
    Ok(stack)
}

fn reset_frames(
    context: &EvalContext,
    state: &Value,
    continuation_state: &Key,
) -> Result<Vec<ResetFrame>, TaskError> {
    reset_frames_from_value(
        context,
        &reset_stack_value(context, state, continuation_state)?,
    )
}

fn reset_frames_from_value(
    context: &EvalContext,
    stack: &Value,
) -> Result<Vec<ResetFrame>, TaskError> {
    let Value::List(stack) = evaluate(context, stack.clone())? else {
        return Err(TaskError::new(
            "reflection continuation state must be a list",
        ));
    };
    eval::list_to_value_items(context, &stack)
        .map_err(task_eval_error)?
        .into_iter()
        .map(|frame| {
            let Value::List(frame) = evaluate(context, frame)? else {
                return Err(TaskError::new(
                    "reflection continuation frame must be a list",
                ));
            };
            let [key, continuation, scope_depth, order]: [Value; 4] =
                eval::list_to_value_items(context, &frame)
                    .map_err(task_eval_error)?
                    .try_into()
                    .map_err(|_| {
                        TaskError::new("reflection continuation frame has the wrong size")
                    })?;
            let Value::Number(scope_depth) = scope_depth else {
                return Err(TaskError::new(
                    "reflection continuation frame has an invalid scope",
                ));
            };
            let Value::Number(order) = order else {
                return Err(TaskError::new(
                    "reflection continuation frame has an invalid order",
                ));
            };
            Ok(ResetFrame {
                key: value_key(context, key)?,
                continuation,
                scope_depth: scope_depth.to_usize_if_integer().ok_or_else(|| {
                    TaskError::new("reflection continuation frame has an invalid scope")
                })?,
                order: order.to_usize_if_integer().ok_or_else(|| {
                    TaskError::new("reflection continuation frame has an invalid order")
                })?,
            })
        })
        .collect()
}

fn reset_frames_value(frames: &[ResetFrame]) -> Value {
    Value::List(List::from_values(
        frames
            .iter()
            .map(|frame| {
                Value::List(List::from_values(vec![
                    key_value(frame.key.clone()),
                    frame.continuation.clone(),
                    Value::Number(Number::from_usize(frame.scope_depth)),
                    Value::Number(Number::from_usize(frame.order)),
                ]))
            })
            .collect(),
    ))
}

fn with_reset_frames(
    context: &EvalContext,
    state: Value,
    continuation_state: &Key,
    frames: &[ResetFrame],
) -> Result<Value, TaskError> {
    with_reset_stack_value(
        context,
        state,
        continuation_state,
        reset_frames_value(frames),
    )
}

fn replace_reset_frames(state: Value, continuation_state: &Key, frames: &[ResetFrame]) -> Value {
    let Value::Dict(state) = state else {
        return Value::error("reflection user state must remain a dictionary");
    };
    Value::Dict(state.insert(continuation_state.clone(), reset_frames_value(frames)))
}

fn with_reset_stack_value(
    context: &EvalContext,
    state: Value,
    continuation_state: &Key,
    stack: Value,
) -> Result<Value, TaskError> {
    reset_frames_from_value(context, &stack)?;
    let Value::Dict(state) = require_state_dict(context, state)? else {
        unreachable!("require_state_dict returned a non-dictionary")
    };
    Ok(Value::Dict(state.insert(continuation_state.clone(), stack)))
}

fn key_value(key: Key) -> Value {
    match key {
        Key::Atom(atom) => Value::Atom(atom),
        Key::Number(number) => Value::Number(number),
        Key::Binary(bytes) => Value::Binary(bytes),
        Key::AbstractGlobalPath(parts) => {
            Value::Atom(Atom::from_key(&Key::AbstractGlobalPath(parts)))
        }
        Key::List(items) => Value::List(List::from_values(
            items.iter().cloned().map(key_value).collect(),
        )),
        Key::Dict(entries) => Value::Dict(
            entries
                .iter()
                .cloned()
                .fold(Dict::new_sync(), |dict, (key, value)| {
                    dict.insert(key, key_value(value))
                }),
        ),
    }
}

fn unit_value() -> Value {
    (*keys::UNIT_VALUE).clone()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use bytes::Bytes;

    use super::*;
    use crate::api::{Assembler, Diagnostic};
    use crate::evaluation::EvaluationTaskHandle;

    #[derive(Clone)]
    struct TestEffects;

    #[derive(Clone)]
    enum TestRequest {
        Reflection(ReflectionRequest),
        ReadLog,
        WriteStderr,
    }

    #[derive(Clone)]
    struct TestSnapshot {
        diagnostics: Arc<[Diagnostic]>,
        revision: u64,
    }

    #[derive(Clone, Default)]
    struct TestJournal {
        reflection: ReflectionJournal,
        consumed_diagnostics: usize,
        stderr: Vec<Bytes>,
    }

    impl ReflectionTransaction for TestJournal {
        fn reflection_journal(&mut self) -> &mut ReflectionJournal {
            &mut self.reflection
        }
    }

    impl TaskSpecialization for TestEffects {
        type Host = TestHost;
        type Request = TestRequest;
        type Snapshot = TestSnapshot;
        type Journal = TestJournal;

        fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
            reflection_request_specs()
                .into_iter()
                .map(|request| request.map_request(TestRequest::Reflection))
                .chain([
                    EffectRequestSpec::new(
                        "read_log",
                        ["reflection_test", "request", "read_log"],
                        0,
                        TestRequest::ReadLog,
                    ),
                    EffectRequestSpec::new(
                        "write_stderr",
                        ["reflection_test", "request", "write_stderr"],
                        1,
                        TestRequest::WriteStderr,
                    ),
                ])
                .collect()
        }

        fn handle_request(
            &self,
            request: Self::Request,
            arguments: Vec<PublicValue>,
            context: &mut RequestContext<'_, Self>,
        ) -> Result<RequestResult, TaskError> {
            match request {
                TestRequest::Reflection(request) => {
                    handle_reflection_request(request, arguments, context)
                }
                TestRequest::ReadLog => read_test_log(context),
                TestRequest::WriteStderr => {
                    let [value]: [PublicValue; 1] = arguments.try_into().map_err(|_| {
                        TaskError::new("test stderr request received the wrong arity")
                    })?;
                    let bytes = value_bytes(value.as_core())?;
                    if let Some(mut transaction) = context.transaction() {
                        transaction.parts().1.stderr.push(bytes);
                    } else {
                        context.host().write_stderr(bytes);
                        context.committed();
                    }
                    Ok(RequestResult::ReturnUnit)
                }
            }
        }
    }

    fn read_test_log(
        context: &mut RequestContext<'_, TestEffects>,
    ) -> Result<RequestResult, TaskError> {
        if let Some(generation) = context.transaction_generation() {
            context.observe_host_generation(generation);
            let mut transaction = context
                .transaction()
                .expect("checked active reflection transaction");
            let (snapshot, journal) = transaction.parts();
            let Some(diagnostic) = snapshot.diagnostics.get(journal.consumed_diagnostics) else {
                return Ok(RequestResult::Fail);
            };
            journal.consumed_diagnostics += 1;
            return diagnostic
                .enrich()
                .map(RequestResult::Return)
                .map_err(|error| TaskError::new(error.to_string()));
        }

        loop {
            let snapshot = <TestHost as TaskHost<TestEffects>>::snapshot(context.host());
            context.observe_host_generation(snapshot.generation());
            let Some(diagnostic) = snapshot.extra().diagnostics.first() else {
                return Ok(RequestResult::Fail);
            };
            let value = diagnostic
                .enrich()
                .map_err(|error| TaskError::new(error.to_string()))?;
            let commit = TaskCommit::new(
                StoreJournal::new(snapshot.store().clone()),
                snapshot.extra().clone(),
                TestJournal {
                    reflection: ReflectionJournal::default(),
                    consumed_diagnostics: 1,
                    stderr: Vec::new(),
                },
            );
            match <TestHost as TaskHost<TestEffects>>::commit(context.host(), commit) {
                CommitResult::Committed => {
                    context.committed();
                    return Ok(RequestResult::Return(value));
                }
                CommitResult::Conflict => {}
                CommitResult::MissingVolume(volume) => {
                    return Err(missing_volume_error(volume));
                }
                CommitResult::Closed => return Ok(RequestResult::Cancelled),
            }
        }
    }

    #[derive(Default)]
    struct TestHost {
        reasoning_session: Option<ReasoningSessionId>,
        state: Mutex<TestHostState>,
    }

    struct TestHostState {
        generation: u64,
        extra_revision: u64,
        store: ReflectionStore,
        diagnostics: Vec<Diagnostic>,
        stderr: Vec<Bytes>,
        wake_diagnostic: Option<Diagnostic>,
        wake_heap: Option<PublicValue>,
        wait_count: usize,
        closed: bool,
    }

    impl Default for TestHostState {
        fn default() -> Self {
            Self {
                generation: 1,
                extra_revision: 0,
                store: ReflectionStore::new(Arc::new(ExactConflictAnalysis)),
                diagnostics: Vec::new(),
                stderr: Vec::new(),
                wake_diagnostic: None,
                wake_heap: None,
                wait_count: 0,
                closed: false,
            }
        }
    }

    impl TestHost {
        fn with_diagnostics(diagnostics: Vec<Diagnostic>) -> Self {
            Self {
                reasoning_session: None,
                state: Mutex::new(TestHostState {
                    diagnostics,
                    ..TestHostState::default()
                }),
            }
        }

        fn with_wake_diagnostic(diagnostic: Diagnostic) -> Self {
            Self {
                reasoning_session: None,
                state: Mutex::new(TestHostState {
                    wake_diagnostic: Some(diagnostic),
                    ..TestHostState::default()
                }),
            }
        }

        fn with_wake_heap(heap: PublicValue) -> Self {
            Self {
                reasoning_session: None,
                state: Mutex::new(TestHostState {
                    wake_heap: Some(heap),
                    ..TestHostState::default()
                }),
            }
        }

        fn with_reasoning_session(reasoning_session: ReasoningSessionId) -> Self {
            Self {
                reasoning_session: Some(reasoning_session),
                state: Mutex::new(TestHostState::default()),
            }
        }

        fn stderr(&self) -> Vec<Bytes> {
            self.state.lock().unwrap().stderr.clone()
        }

        fn heap(&self) -> PublicValue {
            self.state.lock().unwrap().store.root().clone()
        }

        fn diagnostics(&self) -> Vec<Diagnostic> {
            self.state.lock().unwrap().diagnostics.clone()
        }

        fn wait_count(&self) -> usize {
            self.state.lock().unwrap().wait_count
        }

        fn emit_diagnostic(&self, diagnostic: Diagnostic) {
            let mut state = self.state.lock().unwrap();
            state.diagnostics.push(diagnostic);
            state.extra_revision += 1;
            state.generation += 1;
        }

        fn write_stderr(&self, bytes: Bytes) {
            self.state.lock().unwrap().stderr.push(bytes);
        }

        fn wait_for_change(&self, observed_generation: u64) -> bool {
            let mut state = self.state.lock().unwrap();
            state.wait_count += 1;
            if state.generation != observed_generation {
                return true;
            }
            if let Some(heap) = state.wake_heap.take() {
                state.store.replace_root(heap);
                state.generation += 1;
                return true;
            }
            let Some(diagnostic) = state.wake_diagnostic.take() else {
                return false;
            };
            state.diagnostics.push(diagnostic);
            state.extra_revision += 1;
            state.generation += 1;
            true
        }
    }

    impl TaskEnvironment for TestHost {
        fn reflection_environment(&self) -> PublicValue {
            let process_environment = PublicValue::dictionary([(
                PublicValue::text("GLAM_TEST_ENV"),
                PublicValue::text("present"),
            )])
            .expect("test process environment must be keyable");
            PublicValue::record([
                (
                    "glam",
                    PublicValue::record([(
                        "version",
                        PublicValue::text(env!("CARGO_PKG_VERSION")),
                    )]),
                ),
                (
                    "process",
                    PublicValue::record([
                        (
                            "args",
                            PublicValue::list(
                                ["glam", "--test"].into_iter().map(PublicValue::text),
                            ),
                        ),
                        ("env", process_environment),
                    ]),
                ),
            ])
        }
    }

    impl ReflectionServices for TestHost {
        fn emit_diagnostic(&self, diagnostic: Diagnostic) {
            TestHost::emit_diagnostic(self, diagnostic);
        }

        fn update_query(&self, handle: &Arc<EvaluationQueryHandle>, result: PublicValue) {
            let mut state = self.state.lock().unwrap();
            if state.store.update_query(handle, result) {
                state.generation += 1;
            }
        }
    }

    impl TaskHost<TestEffects> for TestHost {
        fn snapshot(&self) -> HostSnapshot<TestEffects> {
            let state = self.state.lock().unwrap();
            HostSnapshot::new(
                state.generation,
                state.store.snapshot(),
                TestSnapshot {
                    diagnostics: Arc::from(state.diagnostics.clone()),
                    revision: state.extra_revision,
                },
            )
        }

        fn commit(&self, commit: TaskCommit<TestEffects>) -> CommitResult {
            let (store, snapshot, journal) = commit.into_parts();
            {
                let mut state = self.state.lock().unwrap();
                if state.closed {
                    return CommitResult::Closed;
                }
                if (journal.consumed_diagnostics != 0 && state.extra_revision != snapshot.revision)
                    || state.diagnostics.len() < journal.consumed_diagnostics
                {
                    return CommitResult::Conflict;
                }
                match state.store.try_commit(&store) {
                    StoreCommitResult::Committed => {}
                    StoreCommitResult::Conflict => return CommitResult::Conflict,
                    StoreCommitResult::MissingVolume(volume) => {
                        return CommitResult::MissingVolume(volume);
                    }
                }
                let consumed = journal.consumed_diagnostics;
                state.diagnostics.drain(..consumed);
                state
                    .diagnostics
                    .extend(journal.reflection.diagnostics().iter().cloned());
                state.stderr.extend_from_slice(&journal.stderr);
                if consumed != 0 || !journal.reflection.diagnostics().is_empty() {
                    state.extra_revision += 1;
                }
                state.generation += 1;
            }
            journal.reflection.commit_updates();
            CommitResult::Committed
        }

        fn wait_for_change(&self, observed_generation: u64) -> bool {
            TestHost::wait_for_change(self, observed_generation)
        }
    }

    impl TaskHost<StandardEffects> for TestHost {
        fn snapshot(&self) -> HostSnapshot<StandardEffects> {
            let state = self.state.lock().unwrap();
            HostSnapshot::new(state.generation, state.store.snapshot(), ())
        }

        fn commit(&self, commit: TaskCommit<StandardEffects>) -> CommitResult {
            let (store, _snapshot, _journal) = commit.into_parts();
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return CommitResult::Closed;
            }
            match state.store.try_commit(&store) {
                StoreCommitResult::Committed => {}
                StoreCommitResult::Conflict => return CommitResult::Conflict,
                StoreCommitResult::MissingVolume(volume) => {
                    return CommitResult::MissingVolume(volume);
                }
            }
            state.generation += 1;
            CommitResult::Committed
        }

        fn wait_for_change(&self, observed_generation: u64) -> bool {
            TestHost::wait_for_change(self, observed_generation)
        }
    }

    impl TaskHost<ReflectionEffects> for TestHost {
        fn snapshot(&self) -> HostSnapshot<ReflectionEffects> {
            let state = self.state.lock().unwrap();
            HostSnapshot::new(state.generation, state.store.snapshot(), ())
        }

        fn commit(&self, commit: TaskCommit<ReflectionEffects>) -> CommitResult {
            let (store, _snapshot, journal) = commit.into_parts();
            {
                let mut state = self.state.lock().unwrap();
                if state.closed {
                    return CommitResult::Closed;
                }
                match state.store.try_commit(&store) {
                    StoreCommitResult::Committed => {}
                    StoreCommitResult::Conflict => return CommitResult::Conflict,
                    StoreCommitResult::MissingVolume(volume) => {
                        return CommitResult::MissingVolume(volume);
                    }
                }
                state
                    .diagnostics
                    .extend(journal.diagnostics().iter().cloned());
                if !journal.diagnostics().is_empty() {
                    state.extra_revision += 1;
                }
                state.generation += 1;
            }
            journal.commit_updates();
            CommitResult::Committed
        }

        fn reasoning_session_id(&self) -> Option<ReasoningSessionId> {
            self.reasoning_session
        }

        fn wait_for_change(&self, observed_generation: u64) -> bool {
            TestHost::wait_for_change(self, observed_generation)
        }
    }

    fn value_bytes(value: &Value) -> Result<Bytes, TaskError> {
        let context = EvalContext::standalone();
        match evaluate(&context, value.clone())? {
            Value::Binary(bytes) => Ok(bytes),
            Value::List(list) => eval::list_output_bytes(&context, &list)
                .map(Bytes::from)
                .map_err(TaskError::new),
            _ => Err(TaskError::new("test stderr request requires binary data")),
        }
    }

    fn run_log_test(effect: &PublicValue, host: Arc<TestHost>) -> Result<TaskOutcome, TaskError> {
        run(effect, TestEffects, host)
    }

    fn run_reflection_test(
        effect: &PublicValue,
        host: Arc<TestHost>,
    ) -> Result<TaskOutcome, TaskError> {
        let host: Arc<dyn ReflectionHost<ReflectionEffects>> = host;
        run(effect, ReflectionEffects, host)
    }

    fn run_standard_test(effect: &PublicValue) -> Result<TaskOutcome, TaskError> {
        run_standard(effect, Arc::new(TestHost::default()))
    }

    fn run_standard_on(
        effect: &PublicValue,
        host: Arc<TestHost>,
    ) -> Result<TaskOutcome, TaskError> {
        run_standard(effect, host)
    }

    fn compile_effect(source: &str) -> (Assembler, PublicValue) {
        let assembler = Assembler::default();
        let module = assembler
            .module(["reflection_test"])
            .script(
                "g",
                format!("language g0\nimport 'std\nrefl.effect = {source}\n"),
            )
            .build()
            .expect("effect fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("effect fixture should define effect");
        (assembler, effect)
    }

    fn assert_list_values(assembler: &Assembler, actual: &PublicValue, expected: &PublicValue) {
        let actual = assembler.evaluate(actual).unwrap();
        let Value::List(actual) = actual.as_core() else {
            panic!("actual value should be a list")
        };
        let Value::List(expected) = expected.as_core() else {
            panic!("expected value should be a list")
        };
        assert_eq!(
            eval::list_to_value_items(&assembler.eval_context(), actual).unwrap(),
            eval::list_to_value_items(&assembler.eval_context(), expected).unwrap(),
        );
    }

    fn completed(source: &str) -> (Assembler, PublicValue) {
        let (assembler, effect) = compile_effect(source);
        let TaskOutcome::Complete(value) = run_standard_test(&effect).unwrap() else {
            panic!("finite effect should complete")
        };
        (assembler, value)
    }

    fn poll_isolated_search<S: TaskSpecialization>(
        search: &mut IsolatedEffectSearch<S>,
    ) -> Arc<[IsolatedSearchResult<S>]> {
        loop {
            match search.poll(256) {
                IsolatedSearchPoll::Yielded => {}
                IsolatedSearchPoll::Complete(results) => return results,
                IsolatedSearchPoll::Blocked(blocked) => panic!(
                    "finite isolated search blocked: dependency={}, generation={:?}, error={:?}",
                    blocked.waiting_on_dependency(),
                    blocked.observed_generation(),
                    blocked.error()
                ),
                IsolatedSearchPoll::Failed(error) => {
                    panic!("finite isolated search failed: {error}")
                }
                IsolatedSearchPoll::Cancelled => panic!("finite isolated search was cancelled"),
            }
        }
    }

    fn isolated_standard_results(source: &str) -> (Assembler, Vec<PublicValue>) {
        let (assembler, effect) = compile_effect(source);
        let host: Arc<dyn TaskHost<StandardEffects>> = Arc::new(TestHost::default());
        let mut search = IsolatedEffectSearch::new(&effect, StandardEffects, host)
            .expect("isolated effect should initialize");
        let results = poll_isolated_search(&mut search);
        let values = results
            .iter()
            .map(|result| result.value().clone())
            .collect();
        (assembler, values)
    }

    fn assert_search_bytes(source: &str, expected: &[&[u8]]) {
        let (assembler, values) = isolated_standard_results(source);
        let actual = values
            .iter()
            .map(|value| assembler.to_binary(value).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn isolated_search_obeys_ordered_choice_laws() {
        assert_search_bytes(".r \"single\"", &[b"single"]);
        assert_search_bytes(".fail", &[]);
        assert_search_bytes(".cut .fail", &[]);
        assert_search_bytes(
            ".alt (.r \"left\") (.alt .fail (.r \"right\"))",
            &[b"left", b"right"],
        );
        assert_search_bytes(
            "(.alt (.r \"A\") (.r \"B\")) >>= (\\x -> .alt (.r (x ++ \"1\")) (.r (x ++ \"2\")))",
            &[b"A1", b"A2", b"B1", b"B2"],
        );
        assert_search_bytes(
            ".alt (.cut (.alt (.r \"first\") (.r \"discarded\"))) (.r \"outer\")",
            &[b"first", b"outer"],
        );
        assert_search_bytes(
            ".fix (\\self -> .alt (.r \"fix left\") (.r \"fix right\"))",
            &[b"fix left", b"fix right"],
        );
        assert_search_bytes(
            ".alt (.reset \"prompt\" (.shift \"prompt\" (\\continuation -> continuation \"resumed\"))) (.r \"outer\")",
            &[b"resumed", b"outer"],
        );
    }

    #[test]
    fn isolated_search_retains_branch_local_journals_without_committing() {
        let (assembler, effect) = compile_effect(
            ".alt ((.write_stderr \"left journal\") =>> .heap.set ['choice] \"left\" =>> .heap.get ['choice]) ((.write_stderr \"right journal\") =>> .heap.set ['choice] \"right\" =>> .heap.get ['choice])",
        );
        let host = Arc::new(TestHost::default());
        let mut search = IsolatedEffectSearch::new(&effect, TestEffects, host.clone())
            .expect("isolated effect should initialize");
        let results = poll_isolated_search(&mut search);

        assert_eq!(results.len(), 2);
        assert_eq!(
            assembler.to_binary(results[0].value()).unwrap(),
            b"left".as_slice()
        );
        assert_eq!(
            assembler.to_binary(results[1].value()).unwrap(),
            b"right".as_slice()
        );
        assert_eq!(
            results[0].journal().stderr,
            [Bytes::from_static(b"left journal")]
        );
        assert_eq!(
            results[1].journal().stderr,
            [Bytes::from_static(b"right journal")]
        );
        assert!(host.stderr().is_empty());
        assert_eq!(host.heap(), PublicValue::empty_record());
    }

    #[test]
    fn isolated_search_reports_and_resumes_retryable_state_observations() {
        let (assembler, effect) = compile_effect(
            ".heap.get ['answer] >>= (\\answer -> (answer == \"ready\") =>> .r answer)",
        );
        let host = Arc::new(TestHost::with_wake_heap(PublicValue::record([(
            "answer",
            PublicValue::text("ready"),
        )])));
        let mut search = IsolatedEffectSearch::new(&effect, TestEffects, host.clone())
            .expect("isolated effect should initialize");

        let generation = loop {
            match search.poll(256) {
                IsolatedSearchPoll::Yielded => {}
                IsolatedSearchPoll::Blocked(blocked) => {
                    assert!(!blocked.waiting_on_dependency());
                    break blocked
                        .observed_generation()
                        .expect("retryable search must retain its observed generation");
                }
                IsolatedSearchPoll::Complete(_) => {
                    panic!("search completed before its observed state changed")
                }
                IsolatedSearchPoll::Failed(error) => panic!("search failed: {error}"),
                IsolatedSearchPoll::Cancelled => panic!("search was cancelled"),
            }
        };
        assert!(host.wait_for_change(generation));

        let results = poll_isolated_search(&mut search);
        assert_eq!(results.len(), 1);
        assert_eq!(
            assembler.to_binary(results[0].value()).unwrap(),
            b"ready".as_slice()
        );
        assert_eq!(host.wait_count(), 1);
    }

    #[test]
    fn isolated_search_retries_observed_errors_without_advancing_choice() {
        let (assembler, effect) =
            compile_effect(".heap.get ['handler] >>= (\\handler -> handler ())");
        let (_, handler) = compile_effect("\\_ -> .r \"recovered\"");
        let host = Arc::new(TestHost::with_wake_heap(PublicValue::record([(
            "handler", handler,
        )])));
        let mut search = IsolatedEffectSearch::new(&effect, TestEffects, host.clone())
            .expect("isolated effect should initialize");

        let generation = loop {
            match search.poll(256) {
                IsolatedSearchPoll::Yielded => {}
                IsolatedSearchPoll::Blocked(blocked) => {
                    assert!(!blocked.waiting_on_dependency());
                    assert!(
                        blocked
                            .error()
                            .is_some_and(|error| error.contains("requires a function value"))
                    );
                    break blocked
                        .observed_generation()
                        .expect("observed error must retain its generation");
                }
                IsolatedSearchPoll::Complete(_) => {
                    panic!("search completed before its observed state changed")
                }
                IsolatedSearchPoll::Failed(error) => panic!("search failed: {error}"),
                IsolatedSearchPoll::Cancelled => panic!("search was cancelled"),
            }
        };
        assert!(host.wait_for_change(generation));

        let results = poll_isolated_search(&mut search);
        assert_eq!(results.len(), 1);
        assert_eq!(
            assembler.to_binary(results[0].value()).unwrap(),
            b"recovered".as_slice()
        );
    }

    #[test]
    fn isolated_search_keeps_unobserved_errors_terminal() {
        let (_, effect) = compile_effect(".alt (1 2) (.r \"not reached\")");
        let host: Arc<dyn TaskHost<StandardEffects>> = Arc::new(TestHost::default());
        let mut search = IsolatedEffectSearch::new(&effect, StandardEffects, host)
            .expect("isolated effect should initialize");

        loop {
            match search.poll(256) {
                IsolatedSearchPoll::Yielded => {}
                IsolatedSearchPoll::Failed(error) => {
                    assert!(error.to_string().contains("requires a function value"));
                    break;
                }
                IsolatedSearchPoll::Blocked(_) => panic!("unobserved error should not block"),
                IsolatedSearchPoll::Complete(_) => {
                    panic!("an evaluation error must not advance to another branch")
                }
                IsolatedSearchPoll::Cancelled => panic!("search was cancelled"),
            }
        }
    }

    #[test]
    fn isolated_search_can_be_cancelled_between_polls() {
        let (_, effect) = compile_effect(".r \"unused\"");
        let host: Arc<dyn TaskHost<StandardEffects>> = Arc::new(TestHost::default());
        let mut search = IsolatedEffectSearch::new(&effect, StandardEffects, host)
            .expect("isolated effect should initialize");
        search.cancel();
        assert!(matches!(search.poll(256), IsolatedSearchPoll::Cancelled));
    }

    #[test]
    fn isolated_search_reports_and_resumes_lazy_dependencies() {
        let (assembler, function) =
            compile_effect("\\value -> .eval value >>= (\\result -> .r result.ok)");
        let session = EvalContext::standalone();
        let owner = session.with_new_task().unwrap();
        let observer = session.with_new_task().unwrap();
        let promised = PromisedValue::fixpoint(&owner, "isolated search dependency").unwrap();
        let effect = eval::apply_values(
            &observer,
            function.as_core().clone(),
            vec![Value::Promised(promised.clone())],
        )
        .unwrap();
        let host = Arc::new(TestHost::default());
        let mut search = IsolatedEffectSearch::new_in_context(
            &PublicValue::from_core(effect),
            TestEffects,
            host,
            observer,
        )
        .expect("isolated effect should initialize");

        loop {
            match search.poll(256) {
                IsolatedSearchPoll::Yielded => {}
                IsolatedSearchPoll::Blocked(blocked) => {
                    assert!(blocked.waiting_on_dependency());
                    assert!(blocked.observed_generation().is_none());
                    break;
                }
                IsolatedSearchPoll::Complete(_) => {
                    panic!("search completed before its dependency")
                }
                IsolatedSearchPoll::Failed(error) => panic!("search failed: {error}"),
                IsolatedSearchPoll::Cancelled => panic!("search was cancelled"),
            }
        }
        promised
            .set(Value::Binary(Bytes::from_static(b"ready")))
            .expect("test dependency should resolve once");

        let results = poll_isolated_search(&mut search);
        assert_eq!(results.len(), 1);
        assert_eq!(
            assembler.to_binary(results[0].value()).unwrap(),
            b"ready".as_slice()
        );
    }

    #[test]
    fn effect_task_poll_yields_and_resumes_with_bounded_fuel() {
        let (assembler, effect) =
            compile_effect(".r \"A\" >>= (\\a -> .r \"B\" >>= (\\b -> .r (a ++ b)))");
        let host = Arc::new(TestHost::default());
        let mut task = EffectTask::new(effect.as_core().clone(), TestEffects, host).unwrap();

        assert!(matches!(task.poll(1), EffectTaskPoll::Yielded));
        let value = loop {
            match task.poll(1) {
                EffectTaskPoll::Yielded => {}
                EffectTaskPoll::Complete(value) => break value,
                EffectTaskPoll::Blocked(_) => panic!("finite task unexpectedly blocked"),
                EffectTaskPoll::Failed(error) => panic!("finite task failed: {error}"),
                EffectTaskPoll::Cancelled => panic!("finite task was cancelled"),
            }
        };
        assert_eq!(assembler.to_binary(&value).unwrap(), b"AB".as_slice());
    }

    #[test]
    fn evaluation_session_pumps_a_type_erased_effect_task() {
        let (_, effect) = compile_effect("(.write_stderr \"scheduled\") =>> .r ()");
        let context = EvalContext::standalone();
        let host = Arc::new(TestHost::default());
        let launcher = task_launcher(TestEffects, host.clone());
        let task = context
            .schedule_task(|task_context| {
                launcher.build(
                    task_context,
                    effect.as_core().clone(),
                    ReflectionTaskKind::Annotation,
                )
            })
            .expect("effect task should schedule");

        assert!(matches!(
            context.poll_reflection_task(&task),
            EvaluationTaskPoll::Pending(_)
        ));
        assert_eq!(
            context.pump_wait(task.wait(), 1),
            crate::evaluation::EvaluationPumpOutcome::BudgetExhausted
        );
        assert_eq!(
            context.pump_wait(task.wait(), 4096),
            crate::evaluation::EvaluationPumpOutcome::TargetReady
        );
        assert!(matches!(
            context.poll_reflection_task(&task),
            EvaluationTaskPoll::Complete(_)
        ));
        assert_eq!(host.stderr(), [Bytes::from_static(b"scheduled")]);
    }

    fn schedule_composed_test_task(
        effect: &PublicValue,
        host: Arc<TestHost>,
    ) -> (EvalContext, EvaluationTaskHandle) {
        let context = EvalContext::standalone();
        let reflection_host: Arc<dyn ReflectionHost<ReflectionEffects>> = host.clone();
        context
            .install_reflection_launcher(task_launcher(ReflectionEffects, reflection_host))
            .expect("fresh test session should accept a reflection launcher");
        let effect = effect.as_core().clone();
        let task = context
            .schedule_task(move |task_context| {
                EffectTask::new_in_context(effect, TestEffects, host, task_context)
                    .map(|task| {
                        Box::new(JoinableEffectTask(task)) as Box<dyn EvaluationTaskMachine>
                    })
                    .map_err(|error| Arc::from(error.to_string()))
            })
            .expect("test task should schedule");
        (context, task)
    }

    fn pump_composed_test_task(
        context: &EvalContext,
        task: &EvaluationTaskHandle,
    ) -> EvaluationTaskPoll {
        assert_eq!(
            context.pump_wait(task.wait(), 16_384),
            crate::evaluation::EvaluationPumpOutcome::TargetReady
        );
        context.poll_reflection_task(task)
    }

    #[test]
    fn effect_run_composes_runtime_children_and_unit_policy() {
        let (assembler, effect) = compile_effect(
            ".task.new (.log 'warn { msg:{ text:\"child\" } }) >>= (\\_task -> .r ())",
        );
        let host = Arc::new(TestHost::default());
        let reflection_host: Arc<dyn ReflectionHost<ReflectionEffects>> = host.clone();

        assert!(matches!(
            EffectRun::new(&effect, TestEffects, host.clone())
                .with_runtime(&assembler.evaluation_runtime())
                .with_reflection_children(reflection_host)
                .requiring_unit_result()
                .run()
                .unwrap(),
            TaskOutcome::Complete(_)
        ));
        assert_eq!(host.diagnostics().len(), 1);
    }

    #[test]
    fn reflection_task_returns_a_joinable_result() {
        let (assembler, effect) =
            compile_effect(".task.new (.r \"child\") >>= (\\task -> .task.join task)");
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&effect, host);
        let EvaluationTaskPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
            panic!("joined task should complete")
        };
        assert_eq!(
            assembler.to_binary(&PublicValue::from_core(value)).unwrap(),
            b"child".as_slice()
        );
    }

    #[test]
    fn dictionary_items_are_available_to_reflection_in_key_order() {
        let (assembler, effect) = compile_effect(".dict_items { b:2, a:1 }");
        let (context, task) = schedule_composed_test_task(&effect, Arc::new(TestHost::default()));
        let EvaluationTaskPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
            panic!("dict_items task should complete");
        };
        let Value::List(items) = value else {
            panic!("dict_items should return a list");
        };
        let items = eval::list_to_value_items(&assembler.eval_context(), &items).unwrap();
        assert_eq!(items.len(), 2);
        let keys = items
            .into_iter()
            .map(|item| {
                let Value::Dict(item) = item else {
                    panic!("dict_items entries should be records");
                };
                item.get(&*keys::KEY)
                    .cloned()
                    .expect("dict_items entries should include their key")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                Value::Atom(Atom::from_key(&Key::binary_from_text("a"))),
                Value::Atom(Atom::from_key(&Key::binary_from_text("b"))),
            ]
        );
    }

    #[test]
    fn reflection_eval_returns_a_tagged_whnf_result() {
        let (_, effect) = compile_effect(".eval (1 + 2)");
        let (context, task) = schedule_composed_test_task(&effect, Arc::new(TestHost::default()));
        let EvaluationTaskPoll::Complete(Value::Dict(result)) =
            pump_composed_test_task(&context, &task)
        else {
            panic!("eval should return an ok result");
        };
        assert_eq!(
            result.get(&*keys::OK),
            Some(&Value::Number(Number::integer(3)))
        );

        let (_, nested) = compile_effect(".eval { bad:1 / 0 }");
        let (context, task) = schedule_composed_test_task(&nested, Arc::new(TestHost::default()));
        let EvaluationTaskPoll::Complete(Value::Dict(result)) =
            pump_composed_test_task(&context, &task)
        else {
            panic!("eval should return a tagged dictionary");
        };
        let Some(Value::Dict(payload)) = result.get(&*keys::OK) else {
            panic!("eval should not recursively force a dictionary payload");
        };
        assert!(matches!(
            payload.get(&Key::atom_from_text("bad")),
            Some(Value::Lazy(_))
        ));
    }

    #[test]
    fn reflection_eval_returns_evaluator_errors_as_data() {
        let (assembler, effect) = compile_effect(".eval (1 / 0) >>= (\\result -> .r result.err)");
        let (context, task) = schedule_composed_test_task(&effect, Arc::new(TestHost::default()));
        let EvaluationTaskPoll::Complete(error) = pump_composed_test_task(&context, &task) else {
            panic!("eval should contain an evaluator error instead of failing its task");
        };
        let error = assembler
            .to_binary(&PublicValue::from_core(error))
            .expect("the provisional eval error value should be text");
        assert!(String::from_utf8_lossy(&error).contains("zero"));
    }

    #[test]
    fn reflection_eval_retries_terminal_lazy_dependencies() {
        let (assembler, success) = compile_effect(
            ".eval (anno { refl:(.r ()) } \"ready\") >>= (\\result -> .r result.ok)",
        );
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&success, host.clone());
        let EvaluationTaskPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
            panic!("eval should resume after a successful lazy dependency");
        };
        assert_eq!(
            assembler.to_binary(&PublicValue::from_core(value)).unwrap(),
            b"ready".as_slice()
        );

        let (_, failure) = compile_effect(
            ".eval (anno { refl:.fail } \"unreachable\") >>= (\\result -> .r result.err)",
        );
        let (context, task) = schedule_composed_test_task(&failure, host);
        let EvaluationTaskPoll::Complete(error) = pump_composed_test_task(&context, &task) else {
            panic!("eval should convert a failed lazy dependency to err");
        };
        let error = assembler
            .to_binary(&PublicValue::from_core(error))
            .expect("the eval error should remain observable after its producer fails");
        assert!(String::from_utf8_lossy(&error).contains("failed permanently"));
    }

    #[test]
    fn reflection_eval_suspends_instead_of_failing_around_a_pending_value() {
        let (_, function) = compile_effect("\\value -> .eval value");
        let session = EvalContext::standalone();
        let owner = session.with_new_task().unwrap();
        let observer = session.with_new_task().unwrap();
        let promised = PromisedValue::fixpoint(&owner, "eval test dependency").unwrap();
        let effect = eval::apply_values(
            &observer,
            function.as_core().clone(),
            vec![Value::Promised(promised.clone())],
        )
        .unwrap();
        let mut task = EffectTask::new_in_context(
            effect,
            TestEffects,
            Arc::new(TestHost::default()),
            observer,
        )
        .unwrap();

        let EffectTaskPoll::Blocked(blocked) = task.poll(256) else {
            panic!("eval should suspend on its value's pending dependency");
        };
        assert!(blocked.lazy.is_some());

        promised
            .fail("dependency failed")
            .expect("test promise should fail once");
        let poll = task.poll(256);
        let EffectTaskPoll::Complete(value) = poll else {
            panic!("eval should retry a terminal dependency and return err");
        };
        let Value::Dict(result) = value.into_core() else {
            panic!("eval should return a tagged result");
        };
        let Some(Value::Binary(error)) = result.get(&*keys::ERR) else {
            panic!("eval should return the dependency failure under err");
        };
        assert_eq!(error.as_ref(), b"dependency failed");
    }

    #[test]
    fn effect_map_runs_left_to_right_and_preserves_result_order() {
        let (_assembler, effect) =
            compile_effect("eff.map (\\item -> .r item) [\"A\",\"B\",\"C\"]");
        let (context, task) = schedule_composed_test_task(&effect, Arc::new(TestHost::default()));
        let EvaluationTaskPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
            panic!("effect map task should complete");
        };
        let Value::List(items) = value else {
            panic!("effect map should return a list");
        };
        let items = eval::list_to_value_items(&context, &items)
            .unwrap()
            .into_iter()
            .map(|mut item| {
                loop {
                    item = eval::eval_value(&context, &item).unwrap();
                    if !matches!(item, Value::Lazy(_) | Value::Promised(_)) {
                        break item;
                    }
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            items,
            [
                Value::binary_from_text("A"),
                Value::binary_from_text("B"),
                Value::binary_from_text("C")
            ]
        );
    }

    #[test]
    fn reflection_environment_is_available_as_plain_data() {
        let (assembler, version) = compile_effect(".env ['glam,'version]");
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&version, host.clone());
        let EvaluationTaskPoll::Complete(version) = pump_composed_test_task(&context, &task) else {
            panic!("environment version should complete")
        };
        assert_eq!(
            assembler
                .to_binary(&PublicValue::from_core(version))
                .unwrap(),
            env!("CARGO_PKG_VERSION").as_bytes()
        );

        let (_, environment) = compile_effect(
            ".env ['process,'env] >>= (\\environment -> (environment.[\"GLAM_TEST_ENV\"] == \"present\") =>> .r \"environment\")",
        );
        let (context, task) = schedule_composed_test_task(&environment, host.clone());
        let environment_poll = pump_composed_test_task(&context, &task);
        assert!(
            matches!(environment_poll, EvaluationTaskPoll::Complete(_)),
            "process environment lookup should complete, got {environment_poll:?}"
        );

        let (_, arguments) = compile_effect(".env ['process,'args]");
        let (context, task) = schedule_composed_test_task(&arguments, host.clone());
        let poll = pump_composed_test_task(&context, &task);
        let EvaluationTaskPoll::Complete(Value::List(arguments)) = poll else {
            panic!("process arguments should return a list, got {poll:?}")
        };
        assert_eq!(
            eval::list_to_value_items(&context, &arguments).unwrap(),
            [
                Value::binary_from_text("glam"),
                Value::binary_from_text("--test")
            ]
        );

        let (_, child_environment) =
            compile_effect(".task.new (.env ['process,'args]) >>= (\\task -> .task.join task)");
        let (context, task) = schedule_composed_test_task(&child_environment, host.clone());
        let EvaluationTaskPoll::Complete(Value::List(arguments)) =
            pump_composed_test_task(&context, &task)
        else {
            panic!("child reflection task should inherit its session environment")
        };
        assert_eq!(
            eval::list_to_value_items(&context, &arguments).unwrap(),
            [
                Value::binary_from_text("glam"),
                Value::binary_from_text("--test")
            ]
        );

        let (_, missing) = compile_effect(
            ".env ['process,'env] >>= (\\environment -> (environment.[\"GLAM_TEST_MISSING\"] == {}) =>> .r \"missing\")",
        );
        let (context, task) = schedule_composed_test_task(&missing, host);
        assert!(matches!(
            pump_composed_test_task(&context, &task),
            EvaluationTaskPoll::Complete(_)
        ));
    }

    #[test]
    fn task_value_is_symmetric_with_task_error() {
        let (assembler, effect) = compile_effect(
            ".task.new (.r \"result\") >>= (\\task -> .task.join task >>= (\\_value -> .task.value task))",
        );
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&effect, host);
        let EvaluationTaskPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
            panic!("task.value should return a completed task result")
        };
        assert_eq!(
            assembler.to_binary(&PublicValue::from_core(value)).unwrap(),
            b"result".as_slice()
        );
    }

    #[test]
    fn task_status_reports_launched_and_terminal_states() {
        let host = Arc::new(TestHost::default());
        for source in [
            ".task.new (.r ()) >>= (\\task -> .task.status task >>= (\\status -> (status == 'launched) =>> .r ()))",
            ".task.new (.r ()) >>= (\\task -> .task.join task >>= (\\_ -> .task.status task >>= (\\status -> .r status.ok)))",
            ".task.new .fail >>= (\\task -> .task.error task >>= (\\_ -> .task.status task >>= (\\status -> .r status.err)))",
            ".task.new (.r ()) >>= (\\task -> .task.cancel task =>> .task.error task >>= (\\_ -> .task.status task >>= (\\status -> (status == 'canceled) =>> .r ())))",
        ] {
            let (_, effect) = compile_effect(source);
            let (context, task) = schedule_composed_test_task(&effect, host.clone());
            let poll = pump_composed_test_task(&context, &task);
            assert!(
                matches!(poll, EvaluationTaskPoll::Complete(_)),
                "task status should match for {source}, got {poll:?}"
            );
        }
    }

    #[test]
    fn task_status_rejects_a_handle_from_another_session() {
        let (assembler, spawn) = compile_effect(".task.new (.r ())");
        let host = Arc::new(TestHost::default());
        let (first_context, first_task) = schedule_composed_test_task(&spawn, host.clone());
        let EvaluationTaskPoll::Complete(handle) =
            pump_composed_test_task(&first_context, &first_task)
        else {
            panic!("first session should return a task handle")
        };

        let (_, inspect) = compile_effect("\\task -> .task.status task");
        let inspect = assembler
            .apply(&inspect, [PublicValue::from_core(handle)])
            .expect("foreign task inspection should apply");
        let (second_context, second_task) = schedule_composed_test_task(&inspect, host);
        assert!(matches!(
            pump_composed_test_task(&second_context, &second_task),
            EvaluationTaskPoll::Failed(error)
                if error.as_ref() == "task handle does not belong to this evaluation session"
        ));
    }

    #[test]
    fn join_does_not_advance_an_alternative_while_the_child_is_nonterminal() {
        let (_, effect) = compile_effect(
            ".task.new (.cut (.heap.get ['never] >>= (\\_ -> .fail))) >>= (\\task -> .cut (.alt (.task.join task) (.r \"fallback\")))",
        );
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&effect, host);
        assert_eq!(
            context.pump_wait(task.wait(), 16_384),
            crate::evaluation::EvaluationPumpOutcome::NoProgress
        );
        assert!(matches!(
            context.poll_reflection_task(&task),
            EvaluationTaskPoll::Pending(_)
        ));
    }

    #[test]
    fn cancellation_is_transactional_and_late_cancellation_is_harmless() {
        let (assembler, rolled_back) = compile_effect(
            ".task.new (.r \"alive\") >>= (\\task -> (.cut (.alt ((.task.cancel task) =>> .fail) (.r ()))) =>> .task.join task)",
        );
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&rolled_back, host.clone());
        let EvaluationTaskPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
            panic!("rolled-back cancellation should not cancel the child")
        };
        assert_eq!(
            assembler.to_binary(&PublicValue::from_core(value)).unwrap(),
            b"alive".as_slice()
        );

        let (_, committed) = compile_effect(
            ".cut (.task.new (.r ()) >>= (\\task -> (.task.cancel task) =>> .r task)) >>= (\\task -> .task.status task >>= (\\status -> (status == 'canceled) =>> .r ()))",
        );
        let (context, task) = schedule_composed_test_task(&committed, host.clone());
        assert!(matches!(
            pump_composed_test_task(&context, &task),
            EvaluationTaskPoll::Complete(_)
        ));

        let (_, spawn_foreign) = compile_effect(".task.new (.r ())");
        let (source_context, source_task) =
            schedule_composed_test_task(&spawn_foreign, host.clone());
        let EvaluationTaskPoll::Complete(foreign_handle) =
            pump_composed_test_task(&source_context, &source_task)
        else {
            panic!("source session should produce a foreign task handle")
        };
        let (_, cancel_foreign) = compile_effect("\\task -> .task.cancel task");
        let cancel_foreign = assembler
            .apply(&cancel_foreign, [PublicValue::from_core(foreign_handle)])
            .expect("foreign cancellation should apply");
        let (foreign_context, foreign_task) =
            schedule_composed_test_task(&cancel_foreign, host.clone());
        assert!(matches!(
            pump_composed_test_task(&foreign_context, &foreign_task),
            EvaluationTaskPoll::Complete(_)
        ));

        let (_, late) = compile_effect(
            ".task.new (.r \"done\") >>= (\\task -> .task.join task >>= (\\value -> (.task.cancel task) =>> .r value))",
        );
        let (context, task) = schedule_composed_test_task(&late, host);
        assert!(matches!(
            pump_composed_test_task(&context, &task),
            EvaluationTaskPoll::Complete(_)
        ));
    }

    #[test]
    fn reflection_task_launch_is_buffered_until_cut_commit() {
        let (assembler, effect) =
            compile_effect(".cut (.task.new (.r \"committed\")) >>= (\\task -> .task.join task)");
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&effect, host);
        let EvaluationTaskPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
            panic!("committed child task should complete")
        };
        assert_eq!(
            assembler.to_binary(&PublicValue::from_core(value)).unwrap(),
            b"committed".as_slice()
        );
    }

    #[test]
    fn failed_transaction_discards_its_reflection_task_launch() {
        let (_, effect) = compile_effect(
            ".cut (.alt (.task.new (.log 'error { msg:{ text:\"discarded\" } }) >>= (\\task -> .fail)) (.r \"kept\"))",
        );
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&effect, host.clone());
        let poll = pump_composed_test_task(&context, &task);
        assert!(
            matches!(poll, EvaluationTaskPoll::Complete(_)),
            "winning alternative should complete, got {poll:?}"
        );
        assert!(host.diagnostics().is_empty());
        assert_eq!(context.reflection_task_count(), 1);
    }

    #[test]
    fn join_propagates_task_error_and_task_error_extracts_it() {
        let (_, join) = compile_effect(".task.new .fail >>= (\\task -> .task.join task)");
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&join, host.clone());
        let EvaluationTaskPoll::Failed(error) = pump_composed_test_task(&context, &task) else {
            panic!("join should propagate its child task error")
        };
        assert!(error.contains("failed permanently"));

        let (assembler, extract) =
            compile_effect(".task.new .fail >>= (\\task -> .task.error task)");
        let (context, task) = schedule_composed_test_task(&extract, host);
        let poll = pump_composed_test_task(&context, &task);
        let EvaluationTaskPoll::Complete(value) = poll else {
            panic!("task_error should return the child task error, got {poll:?}")
        };
        let text = assembler
            .to_binary(&PublicValue::from_core(value))
            .expect("task error should be text");
        assert!(String::from_utf8_lossy(&text).contains("failed permanently"));
    }

    #[test]
    fn failed_and_cancelled_joins_retain_retryable_errors() {
        let host = Arc::new(TestHost::default());
        for (source, expected) in [
            (
                ".task.new .fail >>= (\\task -> .cut (.heap.get ['observed] >>= (\\_ -> .task.join task)))",
                "failed permanently",
            ),
            (
                ".task.new (.r ()) >>= (\\task -> (.task.cancel task) =>> .cut (.heap.get ['observed] >>= (\\_ -> .task.join task)))",
                "was cancelled",
            ),
        ] {
            let (_, effect) = compile_effect(source);
            let (context, parent) = schedule_composed_test_task(&effect, host.clone());
            let EvaluationSessionRun::Deadlocked(report) = context.run_until_quiescent() else {
                panic!("retryable joined task error should remain unfinished for {source}")
            };
            let blocked = report
                .unfinished
                .iter()
                .find(|task| task.task == parent.id())
                .expect("parent task should retain its joined error");
            assert!(blocked.wait.is_none());
            assert!(blocked.observed_generation.is_some());
            assert!(
                blocked
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains(expected)),
                "unexpected retained error for {source}: {:?}",
                blocked.error
            );
        }
    }

    #[test]
    fn observed_evaluation_error_restarts_without_advancing_alternatives() {
        let (assembler, effect) = compile_effect(".cut (.alt .read_log (1 2))");
        let host = Arc::new(TestHost::default());
        let mut task =
            EffectTask::new(effect.as_core().clone(), TestEffects, host.clone()).unwrap();

        let EffectTaskPoll::Blocked(blocked) = task.poll(512) else {
            panic!("error after an observed alternative should remain retryable")
        };
        assert!(blocked.lazy.is_none());
        assert!(blocked.observed_generation.is_some());
        assert!(
            blocked
                .error
                .as_deref()
                .is_some_and(|error| error.contains("requires a function value")),
            "unexpected retained error: {:?}",
            blocked.error
        );

        host.emit_diagnostic(Diagnostic::new(
            crate::diagnostic::Severity::Info,
            "available after retry",
        ));
        let value = loop {
            match task.poll(512) {
                EffectTaskPoll::Yielded => {}
                EffectTaskPoll::Complete(value) => break value,
                EffectTaskPoll::Blocked(_) => panic!("changed observation did not retry the cut"),
                EffectTaskPoll::Failed(error) => panic!("retryable task failed: {error}"),
                EffectTaskPoll::Cancelled => panic!("retryable task was cancelled"),
            }
        };
        assert_eq!(
            assembler.get(&value, "msg.text").unwrap(),
            PublicValue::text("available after retry")
        );
    }

    #[test]
    fn synchronous_error_recovery_waits_for_observed_state_change() {
        let (assembler, effect) =
            compile_effect(".cut (.heap.get ['handler] >>= (\\handler -> handler ()))");
        let (_, handler) = compile_effect("\\_ -> .r \"recovered\"");
        let host = Arc::new(TestHost::with_wake_heap(PublicValue::record([(
            "handler", handler,
        )])));
        let mut task =
            EffectTask::new(effect.as_core().clone(), TestEffects, host.clone()).unwrap();

        let TaskOutcome::Complete(value) = task.run().unwrap() else {
            panic!("state change should recover the evaluation error")
        };
        assert_eq!(
            assembler.to_binary(&value).unwrap(),
            b"recovered".as_slice()
        );
        assert_eq!(host.wait_count(), 1);
    }

    #[test]
    fn unobserved_evaluation_error_remains_terminal_inside_cut() {
        let (_, effect) = compile_effect(".cut (.alt (1 2) (.r \"fallback\"))");
        let error = run_standard_test(&effect).expect_err("unobserved error should be terminal");
        assert!(error.to_string().contains("requires a function value"));
    }

    #[test]
    fn task_error_fails_for_a_successful_task() {
        let (_, effect) = compile_effect(".task.new (.r ()) >>= (\\task -> .task.error task)");
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&effect, host);
        let EvaluationTaskPoll::Failed(error) = pump_composed_test_task(&context, &task) else {
            panic!("task_error should fail for a successful task")
        };
        assert!(error.contains("failed permanently"));
    }

    #[test]
    fn pending_task_error_is_an_effect_failure_before_it_is_a_wait() {
        let (assembler, effect) = compile_effect(
            ".task.new (.r \"child\") >>= (\\task -> .cut (.alt (.task.error task) (.r \"fallback\")))",
        );
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&effect, host);
        let EvaluationTaskPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
            panic!("task_error alternative should fall through")
        };
        assert_eq!(
            assembler.to_binary(&PublicValue::from_core(value)).unwrap(),
            b"fallback".as_slice()
        );
    }

    #[test]
    fn spawned_tasks_receive_only_reusable_reflection_capabilities() {
        let (assembler, effect) = compile_effect(
            ".task.new (.write_stderr \"forbidden\") >>= (\\task -> .task.error task)",
        );
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&effect, host.clone());
        let EvaluationTaskPoll::Complete(error) = pump_composed_test_task(&context, &task) else {
            panic!("child capability error should be observable through task_error")
        };
        let error = assembler
            .to_binary(&PublicValue::from_core(error))
            .expect("task error should be text");
        assert!(
            String::from_utf8_lossy(&error).contains("could not be applied"),
            "unexpected capability error: {}",
            String::from_utf8_lossy(&error)
        );
        assert!(host.stderr().is_empty());
    }

    #[test]
    fn polling_reports_state_block_without_waiting_in_the_machine() {
        let (_, effect) = compile_effect(".read_log");
        let host = Arc::new(TestHost::default());
        let mut task =
            EffectTask::new(effect.as_core().clone(), TestEffects, host.clone()).unwrap();

        let EffectTaskPoll::Blocked(blocked) = task.poll(256) else {
            panic!("empty queue should suspend the task")
        };
        assert!(blocked.lazy.is_none());
        assert!(blocked.observed_generation.is_some());
        assert_eq!(host.wait_count(), 0);

        host.emit_diagnostic(Diagnostic::new(
            crate::diagnostic::Severity::Info,
            "available now",
        ));
        assert!(matches!(task.poll(256), EffectTaskPoll::Complete(_)));
        assert_eq!(host.wait_count(), 0);
    }

    #[test]
    fn lazy_suspension_preserves_cut_choice_and_does_not_repeat_prior_commit() {
        let (assembler, build_effect) = compile_effect(
            "\\x -> (.write_stderr \"once\") =>> .cut (.alt (.r x >>= (\\value -> (value == \"done\") =>> .r value)) ((.write_stderr \"wrong\") =>> .r \"wrong\"))",
        );
        let gate = PublicValue::from_core(Value::Lazy(LazyValue::from_reflection_gate(
            Value::Number(Number::from_u64(0)),
            Value::binary_from_text("done"),
        )));
        let effect = assembler.apply(&build_effect, [gate]).unwrap();
        let host = Arc::new(TestHost::default());
        let mut task =
            EffectTask::new(effect.as_core().clone(), TestEffects, host.clone()).unwrap();

        let blocked = match task.poll(512) {
            EffectTaskPoll::Blocked(blocked) => blocked,
            EffectTaskPoll::Yielded => panic!("task exhausted an unexpectedly large poll budget"),
            EffectTaskPoll::Complete(value) => panic!(
                "annotation dependency completed early with {:?}",
                assembler.to_binary(&value)
            ),
            EffectTaskPoll::Failed(error) => panic!("annotation dependency failed: {error}"),
            EffectTaskPoll::Cancelled => panic!("annotation dependency was cancelled"),
        };
        let wait = blocked
            .lazy
            .expect("lazy suspension should retain its wait token");
        assert_eq!(host.stderr(), [Bytes::from_static(b"once")]);

        task.eval_context.complete_wait(&wait);
        let value = loop {
            match task.poll(512) {
                EffectTaskPoll::Yielded => {}
                EffectTaskPoll::Complete(value) => break value,
                EffectTaskPoll::Blocked(_) => panic!("completed dependency remained blocked"),
                EffectTaskPoll::Failed(error) => panic!("resumed task failed: {error}"),
                EffectTaskPoll::Cancelled => panic!("resumed task was cancelled"),
            }
        };
        assert_eq!(assembler.to_binary(&value).unwrap(), b"done".as_slice());
        assert_eq!(host.stderr(), [Bytes::from_static(b"once")]);
    }

    #[test]
    fn changed_observation_restarts_a_cut_before_its_lazy_dependency() {
        let (assembler, build_effect) = compile_effect(
            "\\x -> .cut (.alt (.read_log >>= (\\message -> .r message.msg.text)) (.r x >>= (\\value -> (value == \"unused\") =>> .r value)))",
        );
        let gate = PublicValue::from_core(Value::Lazy(LazyValue::from_reflection_gate(
            Value::Number(Number::from_u64(0)),
            Value::binary_from_text("unused"),
        )));
        let effect = assembler.apply(&build_effect, [gate]).unwrap();
        let host = Arc::new(TestHost::default());
        let mut task =
            EffectTask::new(effect.as_core().clone(), TestEffects, host.clone()).unwrap();

        let EffectTaskPoll::Blocked(blocked) = task.poll(512) else {
            panic!("right alternative should retain the failed queue observation")
        };
        assert!(blocked.lazy.is_some());
        assert!(blocked.observed_generation.is_some());

        host.emit_diagnostic(Diagnostic::new(
            crate::diagnostic::Severity::Info,
            "state won",
        ));
        let value = loop {
            match task.poll(512) {
                EffectTaskPoll::Yielded => {}
                EffectTaskPoll::Complete(value) => break value,
                EffectTaskPoll::Blocked(_) => panic!("changed observation did not restart cut"),
                EffectTaskPoll::Failed(error) => panic!("restarted cut failed: {error}"),
                EffectTaskPoll::Cancelled => panic!("restarted cut was cancelled"),
            }
        };
        assert_eq!(
            assembler.to_binary(&value).unwrap(),
            b"state won".as_slice()
        );
    }

    #[test]
    fn runs_return_sequence_and_fixpoint_requests() {
        let (assembler, value) =
            completed(".fix (\\self -> .r \"A\") >>= (\\x -> .r (x ++ \"B\"))");
        assert_eq!(assembler.to_binary(&value).unwrap(), b"AB".as_slice());
    }

    #[test]
    fn unobserved_failure_is_permanent_with_or_without_cut() {
        for source in [".fail", ".cut .fail"] {
            let (_, effect) = compile_effect(source);
            let host = Arc::new(TestHost::default());
            assert!(
                run_log_test(&effect, host.clone())
                    .unwrap_err()
                    .to_string()
                    .contains("failed permanently")
            );
            assert_eq!(host.wait_count(), 0, "`{source}` must not wait");
        }
    }

    #[test]
    fn empty_log_read_outside_cut_retries_after_its_observation_changes() {
        let (assembler, effect) =
            compile_effect(".read_log >>= (\\message -> .r message.msg.text)");
        let host = Arc::new(TestHost::with_wake_diagnostic(Diagnostic::new(
            crate::diagnostic::Severity::Warning,
            "arrived later",
        )));
        let TaskOutcome::Complete(value) = run_log_test(&effect, host.clone()).unwrap() else {
            panic!("observed queue change should resume the log read")
        };
        assert_eq!(
            assembler.to_binary(&value).unwrap(),
            b"arrived later".as_slice()
        );
        assert_eq!(host.wait_count(), 1);
        assert!(host.diagnostics().is_empty());
    }

    #[test]
    fn committed_log_read_clears_its_retry_checkpoint() {
        let (_, effect) = compile_effect(".read_log >>= (\\_message -> .fail)");
        let host = Arc::new(TestHost::with_diagnostics(vec![Diagnostic::new(
            crate::diagnostic::Severity::Warning,
            "consumed once",
        )]));
        assert!(
            run_log_test(&effect, host.clone())
                .unwrap_err()
                .to_string()
                .contains("failed permanently")
        );
        assert_eq!(host.wait_count(), 0);
        assert!(host.diagnostics().is_empty());
    }

    #[test]
    fn cut_retries_only_after_a_failed_alternative_observes_changeable_state() {
        let (assembler, effect) =
            compile_effect(".cut (.alt (.read_log >>= (\\message -> .r message.msg.text)) .fail)");
        let host = Arc::new(TestHost::with_wake_diagnostic(Diagnostic::new(
            crate::diagnostic::Severity::Warning,
            "cut resumed",
        )));
        let TaskOutcome::Complete(value) = run_log_test(&effect, host.clone()).unwrap() else {
            panic!("observed queue change should restart the exhausted cut")
        };
        assert_eq!(
            assembler.to_binary(&value).unwrap(),
            b"cut resumed".as_slice()
        );
        assert_eq!(host.wait_count(), 1);
    }

    #[test]
    fn standard_task_does_not_expose_specialized_requests() {
        let (_, effect) = compile_effect(".read_log");
        assert!(run_standard_test(&effect).is_err());

        let (_, effect) = compile_effect(".log 'info { msg:{ text:\"hidden\" } }");
        assert!(run_standard_test(&effect).is_err());
    }

    #[test]
    fn reusable_reflection_api_does_not_expose_internal_queries() {
        let (_, effect) = compile_effect(".query.result {}");
        assert!(run_reflection_test(&effect, Arc::new(TestHost::default())).is_err());
    }

    #[test]
    fn reusable_reflection_log_emits_raw_diagnostics_transactionally() {
        let (assembler, effect) =
            compile_effect(".cut ((.log 'warn { msg:{ text:\"reflection warning\" } }) =>> .r ())");
        let host = Arc::new(TestHost::default());
        assert!(matches!(
            run_reflection_test(&effect, host.clone()).unwrap(),
            TaskOutcome::Complete(_)
        ));
        let diagnostics = host.diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].severity(),
            crate::diagnostic::Severity::Warning
        );
        let enriched = diagnostics[0].enrich().unwrap();
        let text = assembler.get(&enriched, "msg.text").unwrap();
        assert_eq!(
            assembler.to_binary(&text).unwrap(),
            b"reflection warning".as_slice()
        );

        let (_, invalid) = compile_effect(".log 'verbose { msg:{ text:\"wrong\" } }");
        assert!(
            run_reflection_test(&invalid, host)
                .unwrap_err()
                .to_string()
                .contains("severity must be")
        );
    }

    #[test]
    fn fixpoint_alternatives_receive_independent_futures() {
        let (assembler, value) = completed(
            ".cut (.fix (\\self -> .alt (.alt (.r \"left\") (.r \"middle\")) (.r \"right\")) >>= (\\value -> (value == \"right\") =>> .r value))",
        );
        assert_eq!(assembler.to_binary(&value).unwrap(), b"right".as_slice());

        let (assembler, value) =
            completed(".fix (\\self -> .cut (.alt .fail (.r \"nested cut\")))");
        assert_eq!(
            assembler.to_binary(&value).unwrap(),
            b"nested cut".as_slice()
        );

        let (assembler, value) = completed(
            ".cut (.fix (\\outer -> .fix (\\inner -> .alt (.r \"nested left\") (.r \"nested right\"))) >>= (\\value -> (value == \"nested right\") =>> .r value))",
        );
        assert_eq!(
            assembler.to_binary(&value).unwrap(),
            b"nested right".as_slice()
        );
    }

    #[test]
    fn reflection_fixpoint_reports_recursive_self_observation() {
        let (_, effect) = compile_effect(".fix (\\recur -> recur)");
        let error = run_standard_test(&effect).unwrap_err();
        assert!(
            error.to_string().contains("recursively observed itself"),
            "{error}"
        );
    }

    #[test]
    fn fixpoint_hides_then_restores_the_reset_stack() {
        let (_, hidden) = compile_effect(
            ".reset \"prompt\" (.fix (\\self -> .shift \"prompt\" (\\continuation -> continuation \"wrong\")))",
        );
        assert!(
            run_standard_test(&hidden)
                .unwrap_err()
                .to_string()
                .contains("not in reset scope")
        );

        let (assembler, value) = completed(
            ".reset \"prompt\" ((.fix (\\self -> .r ())) =>> .shift \"prompt\" (\\continuation -> continuation \"restored\"))",
        );
        assert_eq!(assembler.to_binary(&value).unwrap(), b"restored".as_slice());
    }

    #[test]
    fn cut_rolls_back_failed_alternative_user_state() {
        let (assembler, value) = completed(
            ".cut (.alt ((.set [\"x\"] \"bad\") =>> .fail) ((.get [\"x\"]) >>= (\\x -> (x == {}) =>> .r \"clean\")))",
        );
        assert_eq!(assembler.to_binary(&value).unwrap(), b"clean".as_slice());
    }

    #[test]
    fn shift_captures_only_a_matching_task_local_reset() {
        let (assembler, value) = completed(
            ".reset \"prompt\" (.shift \"prompt\" (\\continuation -> continuation \"resumed\"))",
        );
        assert_eq!(assembler.to_binary(&value).unwrap(), b"resumed".as_slice());

        let (assembler, value) = completed(
            ".reset \"prompt\" ((.cut (.r ())) =>> .shift \"prompt\" (\\continuation -> continuation \"after cut\"))",
        );
        assert_eq!(
            assembler.to_binary(&value).unwrap(),
            b"after cut".as_slice()
        );
    }

    #[test]
    fn continuation_task_identity_prevents_cross_task_aliasing() {
        let (assembler, effect) = compile_effect(
            ".reset \"prompt\" (.shift \"prompt\" (\\continuation -> .r continuation))",
        );
        let TaskOutcome::Complete(continuation) = run_standard_test(&effect).unwrap() else {
            panic!("continuation capture should complete")
        };
        let foreign_invocation = assembler
            .apply(&continuation, [PublicValue::text("foreign")])
            .expect("continuation should remain an applicable value");

        assert!(
            run_standard_test(&foreign_invocation)
                .unwrap_err()
                .to_string()
                .contains("belongs to another reflection task")
        );
    }

    #[test]
    fn replacing_root_state_replaces_the_active_reset_stack() {
        let (_, effect) = compile_effect(
            ".reset \"prompt\" ((.set [] {}) =>> .shift \"prompt\" (\\continuation -> continuation \"wrong\"))",
        );
        assert!(
            run_standard_test(&effect)
                .unwrap_err()
                .to_string()
                .contains("not in reset scope")
        );
    }

    #[test]
    fn restoring_root_state_restores_its_reset_stack() {
        let (assembler, value) = completed(
            ".reset \"prompt\" (.get [] >>= (\\saved -> (.set [] {}) =>> (.set [] saved) =>> .shift \"prompt\" (\\continuation -> continuation \"resumed\")))",
        );
        assert_eq!(assembler.to_binary(&value).unwrap(), b"resumed".as_slice());
    }

    #[test]
    fn reading_all_local_state_does_not_observe_shared_heap() {
        let (_, effect) = compile_effect(".get [] >>= (\\_state -> .fail)");
        let host = Arc::new(TestHost::with_wake_heap(PublicValue::record([(
            "changed",
            PublicValue::text("later"),
        )])));
        let error = run_standard_on(&effect, host.clone()).unwrap_err();
        assert!(error.to_string().contains("failed permanently"));
        assert_eq!(host.wait_count(), 0);
    }

    #[test]
    fn cut_rolls_back_log_reads_and_stderr_writes_before_trying_an_alternative() {
        let (_, effect) = compile_effect(
            ".cut (.alt (.read_log >>= (\\message -> (.write_stderr \"bad\") =>> .fail)) (.read_log >>= (\\message -> (.write_stderr message.msg.text) =>> .r ())))",
        );
        let host = Arc::new(TestHost::with_diagnostics(vec![Diagnostic::new(
            crate::diagnostic::Severity::Warning,
            "good",
        )]));
        assert!(matches!(
            run_log_test(&effect, host.clone()).unwrap(),
            TaskOutcome::Complete(_)
        ));
        assert_eq!(host.stderr(), [Bytes::from_static(b"good")]);
        assert!(
            <TestHost as TaskHost<TestEffects>>::snapshot(host.as_ref())
                .extra()
                .diagnostics
                .is_empty()
        );
    }

    #[test]
    fn composed_logging_does_not_read_its_own_reflection_writes() {
        let (assembler, effect) = compile_effect(
            ".cut (.alt ((.log 'error { msg:{ text:\"bad\" } }) =>> (.read_log >>= (\\message -> (.write_stderr message.msg.text) =>> .r ()))) ((.log 'warn { msg:{ text:\"good\" } }) =>> .r ()))",
        );
        let host = Arc::new(TestHost::default());
        assert!(matches!(
            run_log_test(&effect, host.clone()).unwrap(),
            TaskOutcome::Complete(_)
        ));
        assert!(host.stderr().is_empty());
        let diagnostics = host.diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].severity(),
            crate::diagnostic::Severity::Warning
        );
        let text = assembler
            .get(&diagnostics[0].enrich().unwrap(), "msg.text")
            .unwrap();
        assert_eq!(assembler.to_binary(&text).unwrap(), b"good".as_slice());
    }

    #[test]
    fn root_local_state_replacement_does_not_replace_shared_heap() {
        let (assembler, effect) = compile_effect(
            "(.set [] { heap:{ answer:\"local\" }, local:\"owned\" }) =>> .heap.get []",
        );
        let host = Arc::new(TestHost::default());
        let TaskOutcome::Complete(value) = run_standard_on(&effect, host.clone()).unwrap() else {
            panic!("state effect should complete")
        };
        assert_eq!(value, PublicValue::empty_record());
        assert_eq!(host.heap(), PublicValue::empty_record());
        assert!(assembler.get(&value, "answer").is_err());
    }

    #[test]
    fn child_tasks_start_with_fresh_local_state_and_share_heap() {
        let (assembler, effect) = compile_effect(
            ".heap.set ['shared] \"visible\" =>> .set ['local] \"private\" =>> .task.new (.get ['local] >>= (\\local -> .heap.get ['shared] >>= (\\shared -> .r { local:local, shared:shared }))) >>= (\\task -> .task.join task)",
        );
        let host = Arc::new(TestHost::default());
        let (context, task) = schedule_composed_test_task(&effect, host);
        let EvaluationTaskPoll::Complete(value) = pump_composed_test_task(&context, &task) else {
            panic!("child task should complete")
        };
        let value = PublicValue::from_core(value);
        assert_eq!(
            assembler
                .evaluate(&assembler.get(&value, "local").unwrap())
                .unwrap(),
            PublicValue::empty_record()
        );
        assert_eq!(
            assembler
                .to_binary(&assembler.get(&value, "shared").unwrap())
                .unwrap(),
            b"visible".as_slice()
        );
    }

    #[test]
    fn child_tasks_inherit_same_session_volume_capabilities() {
        let reasoning_session = ReasoningSessionId::from_u64(41).unwrap();
        let host = Arc::new(TestHost::with_reasoning_session(reasoning_session));
        let volume = host
            .state
            .lock()
            .unwrap()
            .store
            .create_volume(PublicValue::text("initial"))
            .unwrap();
        let assembler = Assembler::default();
        let module = assembler
            .module(["reflection_volume_child"])
            .script(
                "g",
                "language g0\nimport 'std\nlaunch = \\cap -> .task.new (cap.set [] \"child\") >>= (\\task -> .task.join task)\n",
            )
            .build()
            .expect("volume child fixture should compile");
        let launch = assembler
            .get(module.value(), "launch")
            .expect("volume child fixture should define launch");
        let effect = assembler
            .apply(&launch, [volume_effects(reasoning_session, volume)])
            .expect("volume capability should apply to the child launcher");
        let reflection_host: Arc<dyn ReflectionHost<ReflectionEffects>> = host.clone();

        assert!(matches!(
            EffectRun::new(&effect, ReflectionEffects, reflection_host.clone())
                .with_reflection_children(reflection_host)
                .run()
                .unwrap(),
            TaskOutcome::Complete(_)
        ));
        let final_value = host
            .state
            .lock()
            .unwrap()
            .store
            .snapshot()
            .volume(volume)
            .cloned()
            .expect("child write must not remove the volume");
        assert_eq!(
            assembler.to_binary(&final_value).unwrap(),
            b"child".as_slice()
        );
    }

    #[test]
    fn failed_alternative_rolls_back_local_and_heap_changes() {
        let (assembler, effect) = compile_effect(
            ".cut (.alt ((.set ['local] \"bad\") =>> .heap.set ['shared] \"bad\" =>> .fail) (.get ['local] >>= (\\local -> .heap.get ['shared] >>= (\\shared -> .r { local:local, shared:shared }))))",
        );
        let host = Arc::new(TestHost::default());
        let TaskOutcome::Complete(value) = run_standard_on(&effect, host.clone()).unwrap() else {
            panic!("clean alternative should complete")
        };
        assert_eq!(
            assembler
                .evaluate(&assembler.get(&value, "local").unwrap())
                .unwrap(),
            PublicValue::empty_record()
        );
        assert_eq!(
            assembler
                .evaluate(&assembler.get(&value, "shared").unwrap())
                .unwrap(),
            PublicValue::empty_record()
        );
        assert_eq!(host.heap(), PublicValue::empty_record());
    }

    #[test]
    fn blind_heap_write_does_not_make_failure_retryable() {
        let (_, effect) = compile_effect(".cut ((.heap.set ['discarded] \"value\") =>> .fail)");
        let host = Arc::new(TestHost::default());
        let error = run_standard_on(&effect, host.clone()).unwrap_err();
        assert!(error.to_string().contains("failed permanently"));
        assert_eq!(host.wait_count(), 0);
        assert_eq!(host.heap(), PublicValue::empty_record());
    }

    #[test]
    fn reading_a_covering_own_write_does_not_make_failure_retryable() {
        let (_, effect) = compile_effect(
            ".cut ((.heap.set ['owned] \"value\") =>> .heap.get ['owned] >>= (\\_value -> .fail))",
        );
        let host = Arc::new(TestHost::with_wake_heap(PublicValue::record([(
            "changed",
            PublicValue::text("later"),
        )])));
        let error = run_standard_on(&effect, host.clone()).unwrap_err();

        assert!(error.to_string().contains("failed permanently"));
        assert_eq!(host.wait_count(), 0);
        assert_eq!(host.heap(), PublicValue::empty_record());
    }

    #[test]
    fn heap_rewrite_lazily_updates_the_transactional_view() {
        let (assembler, effect) = compile_effect(
            ".heap.set ['items] [\"base\"] =>> .heap.rewrite ['items] (\\items -> items ++ [\"next\"]) =>> .heap.get ['items]",
        );
        let host = Arc::new(TestHost::default());
        let TaskOutcome::Complete(value) = run_standard_on(&effect, host.clone()).unwrap() else {
            panic!("heap rewrite should complete")
        };
        let expected = PublicValue::list([PublicValue::text("base"), PublicValue::text("next")]);

        assert_list_values(&assembler, &value, &expected);
        let heap_items = assembler.get(&host.heap(), "items").unwrap();
        assert_list_values(&assembler, &heap_items, &expected);
    }

    #[test]
    fn blind_heap_rewrite_does_not_make_failure_retryable() {
        let (_, effect) =
            compile_effect(".cut (.heap.rewrite ['counter] (\\value -> value + 1) =>> .fail)");
        let host = Arc::new(TestHost::default());
        let error = run_standard_on(&effect, host.clone()).unwrap_err();

        assert!(error.to_string().contains("failed permanently"));
        assert_eq!(host.wait_count(), 0);
        assert_eq!(host.heap(), PublicValue::empty_record());
    }

    #[test]
    fn heap_root_get_and_set_are_explicit_whole_heap_operations() {
        let (assembler, effect) =
            compile_effect(".heap.set [] { answer:\"shared\" } =>> .heap.get []");
        let host = Arc::new(TestHost::default());
        let TaskOutcome::Complete(value) = run_standard_on(&effect, host.clone()).unwrap() else {
            panic!("whole-heap operations should complete")
        };
        assert_eq!(
            assembler
                .to_binary(&assembler.get(&value, "answer").unwrap())
                .unwrap(),
            b"shared".as_slice()
        );
        assert_eq!(value, host.heap());
    }

    #[test]
    fn heap_root_replacement_and_path_errors_remain_lazy() {
        let (assembler, effect) = compile_effect(".cut ((.heap.set [] 42) =>> .heap.get ['x])");
        let host = Arc::new(TestHost::default());
        let TaskOutcome::Complete(value) = run_standard_on(&effect, host.clone()).unwrap() else {
            panic!("heap access should return its latent error value")
        };
        assert!(matches!(value.as_core(), Value::Lazy(_)));
        assert_eq!(host.heap(), PublicValue::integer(42));
        assert!(
            assembler
                .evaluate(&value)
                .unwrap_err()
                .to_string()
                .contains("not a dictionary")
        );

        let (_, caught) = compile_effect(
            ".cut ((.heap.set [] 42) =>> .heap.get ['x] >>= (\\value -> .eval value >>= (\\result -> .r result.err)))",
        );
        let TaskOutcome::Complete(error) =
            run_reflection_test(&caught, Arc::new(TestHost::default())).unwrap()
        else {
            panic!("reflection eval should catch a latent heap access error")
        };
        assert!(
            String::from_utf8_lossy(&assembler.to_binary(&error).unwrap())
                .contains("not a dictionary")
        );
    }

    #[test]
    fn malformed_nested_heap_updates_do_not_poison_unrelated_reads() {
        let (assembler, update) = compile_effect(
            ".cut ((.heap.set [] { safe:\"ok\", x:3 }) =>> (.heap.set ['x, 'b] 7) =>> .r ())",
        );
        let host = Arc::new(TestHost::default());
        assert!(matches!(
            run_standard_on(&update, host.clone()).unwrap(),
            TaskOutcome::Complete(_)
        ));

        let (_, safe) = compile_effect(".heap.get ['safe]");
        let TaskOutcome::Complete(safe) = run_standard_on(&safe, host.clone()).unwrap() else {
            panic!("unrelated heap access should complete")
        };
        assert_eq!(assembler.to_binary(&safe).unwrap(), b"ok".as_slice());

        let (_, bad) = compile_effect(
            ".heap.get ['x, 'b] >>= (\\value -> .eval value >>= (\\result -> .r result.err))",
        );
        let TaskOutcome::Complete(error) = run_reflection_test(&bad, host).unwrap() else {
            panic!("reflection eval should catch the nested update error")
        };
        assert!(
            String::from_utf8_lossy(&assembler.to_binary(&error).unwrap())
                .contains("non-dictionary")
        );
    }

    #[test]
    fn only_heap_reads_make_later_failure_retryable() {
        let ready_heap = PublicValue::record([("answer", PublicValue::text("ready"))]);
        let (assembler, heap_effect) = compile_effect(
            ".heap.get ['answer] >>= (\\answer -> (answer == \"ready\") =>> .r answer)",
        );
        let heap_host = Arc::new(TestHost::with_wake_heap(ready_heap));
        let TaskOutcome::Complete(value) =
            run_standard_on(&heap_effect, heap_host.clone()).unwrap()
        else {
            panic!("heap observation should retry after the heap changes")
        };
        assert_eq!(assembler.to_binary(&value).unwrap(), b"ready".as_slice());
        assert_eq!(heap_host.wait_count(), 1);

        let (_, local_effect) =
            compile_effect(".get ['answer] >>= (\\answer -> (answer == \"ready\") =>> .r answer)");
        let local_host = Arc::new(TestHost::with_wake_heap(PublicValue::record([(
            "answer",
            PublicValue::text("ready"),
        )])));
        let error = run_standard_on(&local_effect, local_host.clone()).unwrap_err();
        assert!(error.to_string().contains("failed permanently"));
        assert_eq!(local_host.wait_count(), 0);
    }

    #[test]
    fn top_level_alternative_and_unmatched_shift_are_rejected() {
        let (_, alternative) = compile_effect(".alt (.r 1) (.r 2)");
        assert!(
            run_standard_test(&alternative)
                .unwrap_err()
                .to_string()
                .contains("requires an enclosing `.cut`")
        );

        let (_, fixpoint_alternative) = compile_effect(".fix (\\self -> .alt (.r 1) (.r 2))");
        assert!(
            run_standard_test(&fixpoint_alternative)
                .unwrap_err()
                .to_string()
                .contains("requires an enclosing `.cut`")
        );

        let (_, shift) = compile_effect(".shift \"missing\" (\\continuation -> .r continuation)");
        assert!(
            run_standard_test(&shift)
                .unwrap_err()
                .to_string()
                .contains("not in reset scope")
        );
    }
}
