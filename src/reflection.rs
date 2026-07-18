//! External freer-effect task interpreter used by reflection clients.
//!
//! Effect requests are ordinary core values sealed by private abstract-global
//! tags. Interaction-net operators only construct those values; this module
//! performs the state, control, transaction, and host operations.

mod requests;

pub use requests::{
    ReflectionHost, ReflectionJournal, ReflectionRequest, ReflectionTransaction,
    handle_reflection_request, reflection_request_specs,
};

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::api::Value as PublicValue;
use crate::core::{Atom, Dict, FunctionValue, Key, LazyValue, List, NetValue, Value, keys};
use crate::core_net::CoreSpecialization;
use crate::eval;
use crate::evaluation::EvalContext;
use crate::interaction_net::NetBuilder;
use crate::number::Number;

/// One additional effect constructor contributed by a task specialization.
pub struct EffectRequestSpec<R> {
    api_name: Arc<str>,
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
        Self {
            api_name: api_name.into(),
            tag_path: tag_path.into_iter().map(Into::into).collect(),
            arity,
            request,
        }
    }

    pub fn map_request<T>(self, map: impl FnOnce(R) -> T) -> EffectRequestSpec<T> {
        EffectRequestSpec {
            api_name: self.api_name,
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

/// Immutable host state observed at the start of an optimistic transaction.
pub struct HostSnapshot<S: TaskSpecialization> {
    generation: u64,
    heap: PublicValue,
    extra: S::Snapshot,
}

impl<S: TaskSpecialization> HostSnapshot<S> {
    pub fn new(generation: u64, heap: PublicValue, extra: S::Snapshot) -> Self {
        Self {
            generation,
            heap,
            extra,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn heap(&self) -> &PublicValue {
        &self.heap
    }

    pub fn extra(&self) -> &S::Snapshot {
        &self.extra
    }
}

impl<S: TaskSpecialization> Clone for HostSnapshot<S> {
    fn clone(&self) -> Self {
        Self {
            generation: self.generation,
            heap: self.heap.clone(),
            extra: self.extra.clone(),
        }
    }
}

/// Changes to host-owned resources produced by one successful outer cut.
pub struct TaskCommit<S: TaskSpecialization> {
    generation: u64,
    heap: PublicValue,
    extra: S::Journal,
}

impl<S: TaskSpecialization> TaskCommit<S> {
    pub fn new(generation: u64, heap: PublicValue, extra: S::Journal) -> Self {
        Self {
            generation,
            heap,
            extra,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn heap(&self) -> &PublicValue {
        &self.heap
    }

    pub fn extra(&self) -> &S::Journal {
        &self.extra
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitResult {
    Committed,
    Conflict,
    Closed,
}

/// Host-owned transactional resources available to a reflection task.
pub trait TaskHost<S: TaskSpecialization>: Send + Sync {
    fn snapshot(&self) -> HostSnapshot<S>;
    fn commit(&self, commit: TaskCommit<S>) -> CommitResult;

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
pub struct TaskError(Arc<str>);

impl TaskError {
    pub fn new(message: impl Into<Arc<str>>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TaskError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskId(u64);

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

fn allocate_task_id() -> Result<TaskId, TaskError> {
    NEXT_TASK_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map(TaskId)
        .map_err(|_| TaskError::new("reflection task IDs exhausted"))
}

/// Runs one reflection effect with a statically selected set of extra effects.
pub fn run<S: TaskSpecialization>(
    effect: &PublicValue,
    specialization: S,
    host: Arc<S::Host>,
) -> Result<TaskOutcome, TaskError> {
    EffectTask::new(specialization, host)?.run(effect.as_core().clone())
}

/// Runs a task with standard effects and no specialization-owned requests.
pub fn run_standard(
    effect: &PublicValue,
    host: Arc<dyn TaskHost<StandardEffects>>,
) -> Result<TaskOutcome, TaskError> {
    run(effect, StandardEffects, host)
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
    id: TaskId,
    specialization: S,
    host: Arc<S::Host>,
    tags: Tags,
    specialized_requests: Vec<SpecializedRequest<S::Request>>,
    api: Value,
    local_state: Value,
    next_continuation: u64,
    next_control_order: usize,
    continuations: HashMap<u64, CapturedContinuation>,
}

impl<S: TaskSpecialization> EffectTask<S> {
    fn new(specialization: S, host: Arc<S::Host>) -> Result<Self, TaskError> {
        let tags = Tags::new();
        let (api, specialized_requests) = effect_api(&tags, specialization.requests())?;
        Ok(Self {
            eval_context: EvalContext::standalone(),
            id: allocate_task_id()?,
            specialization,
            host,
            tags,
            specialized_requests,
            api,
            local_state: Value::Dict(Dict::new_sync()),
            next_continuation: 1,
            next_control_order: 1,
            continuations: HashMap::new(),
        })
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
                Value::Number(Number::from_u64(self.id.0)),
                Value::Number(Number::from_u64(id)),
            ],
            true,
        ))
    }

    fn start_fixpoint(
        &mut self,
        root: Arc<FixRoot<S>>,
        choices: Vec<FixChoice>,
    ) -> Result<Branch<S>, TaskError> {
        let mut branch = root.entry.clone();
        let handle = LazyValue::pending("reflection effect fixpoint");
        let marker = Value::Lazy(handle.clone());
        let operation = apply(&self.eval_context, root.function.clone(), vec![marker])?;
        let outer_control = std::mem::take(&mut branch.control);
        let reset_stack = reset_stack_value(
            &self.eval_context,
            &branch.state,
            &self.tags.continuation_state,
        )?;
        branch.state = with_reset_frames(
            &self.eval_context,
            branch.state,
            &self.tags.continuation_state,
            &[],
        )?;
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
            order: self.allocate_control_order()?,
        });
        branch.effect = operation;
        Ok(branch)
    }

    fn restart_fixpoint_at_scope(
        &mut self,
        branch: &mut Branch<S>,
        scope_depth: usize,
    ) -> Result<Option<Branch<S>>, TaskError> {
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
        restarted.fix_restarts = restart.inherited_restarts;
        Ok(Some(restarted))
    }

    fn install_captured_control(
        &mut self,
        branch: &mut Branch<S>,
        captured: &mut CapturedContinuation,
        scope_depth: usize,
    ) -> Result<(), TaskError> {
        let mut layers = captured
            .reset_frames
            .drain(..)
            .map(CapturedLayer::Reset)
            .chain(captured.delimiters.drain(..).map(CapturedLayer::Delimiter))
            .collect::<Vec<_>>();
        layers.sort_by_key(CapturedLayer::order);

        let mut reset_frames = reset_frames(
            &self.eval_context,
            &branch.state,
            &self.tags.continuation_state,
        )?;
        for layer in layers {
            let order = self.allocate_control_order()?;
            match layer {
                CapturedLayer::Reset(mut frame) => {
                    frame.scope_depth = scope_depth;
                    frame.order = order;
                    reset_frames.push(frame);
                }
                CapturedLayer::Delimiter(mut delimiter) => {
                    delimiter.rebase(scope_depth, order);
                    branch.control.delimiters.push(delimiter);
                }
            }
        }
        branch.state = with_reset_frames(
            &self.eval_context,
            branch.state.clone(),
            &self.tags.continuation_state,
            &reset_frames,
        )?;
        Ok(())
    }

    fn run(&mut self, effect: Value) -> Result<TaskOutcome, TaskError> {
        let mut branch = Branch::new(effect, self.visible_state(self.host.snapshot().heap()));
        loop {
            match self.drive(branch, 0)? {
                Drive::Complete(value, completed) => {
                    self.local_state = split_user_state(completed.state).0;
                    return Ok(TaskOutcome::Complete(PublicValue::from_core(value)));
                }
                Drive::Fail(_) => {
                    return Err(TaskError::new("reflection task failed permanently"));
                }
                Drive::Fork(_, _) => {
                    return Err(TaskError::new("`.alt` requires an enclosing `.cut`"));
                }
                Drive::Retry(mut failed) => {
                    let checkpoint = failed.retry.take().ok_or_else(|| {
                        TaskError::new("retryable reflection failure lost its observation")
                    })?;
                    if !self.host.wait_for_change(checkpoint.generation) {
                        return Ok(TaskOutcome::Cancelled);
                    }
                    branch = *checkpoint.branch;
                }
                Drive::Cancelled => return Ok(TaskOutcome::Cancelled),
            }
        }
    }

    fn drive(&mut self, mut branch: Branch<S>, scope_depth: usize) -> Result<Drive<S>, TaskError> {
        macro_rules! deliver_value {
            ($value:expr) => {
                match deliver(
                    &self.eval_context,
                    $value,
                    branch,
                    scope_depth,
                    &self.tags.continuation_state,
                )? {
                    Delivery::Continue(next) => branch = next,
                    Delivery::Complete(value, completed) => {
                        return Ok(Drive::Complete(value, completed));
                    }
                }
            };
        }
        loop {
            let request = self.effect_request(branch.effect.clone())?;
            match request {
                Request::Return(value) => {
                    match deliver(
                        &self.eval_context,
                        value,
                        branch,
                        scope_depth,
                        &self.tags.continuation_state,
                    )? {
                        Delivery::Continue(next) => branch = next,
                        Delivery::Complete(value, completed) => {
                            return Ok(Drive::Complete(value, completed));
                        }
                    }
                }
                Request::Seq(operation, continuation) => {
                    branch
                        .control
                        .sequence
                        .push(Continuation::Glam(continuation));
                    branch.effect = operation;
                }
                Request::Alt(left, right) => {
                    if scope_depth > 0 && !branch.active_fixes.is_empty() {
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
                            continue;
                        }

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
                        continue;
                    }
                    return Ok(Drive::Fork(
                        Box::new(branch.with_effect(left)),
                        Box::new(branch.with_effect(right)),
                    ));
                }
                Request::Fail => return Ok(branch.into_failure()),
                Request::Cut(operation) => {
                    let outer_sequence = std::mem::take(&mut branch.control.sequence);
                    match self.run_cut(operation, branch, scope_depth + 1)? {
                        CutResult::Success(value, mut completed) => {
                            completed.control.sequence = outer_sequence;
                            match deliver(
                                &self.eval_context,
                                value,
                                completed,
                                scope_depth,
                                &self.tags.continuation_state,
                            )? {
                                Delivery::Continue(next) => branch = next,
                                Delivery::Complete(value, completed) => {
                                    return Ok(Drive::Complete(value, completed));
                                }
                            }
                        }
                        CutResult::Retry(mut failed) => {
                            if let Some(restarted) =
                                self.restart_fixpoint_at_scope(&mut failed, scope_depth)?
                            {
                                branch = restarted;
                                continue;
                            }
                            return Ok(Drive::Retry(failed));
                        }
                        CutResult::Fail(failed) => return Ok(failed.into_failure()),
                        CutResult::Cancelled => return Ok(Drive::Cancelled),
                    }
                }
                Request::Get(path) => {
                    let path = eval::eval_key_path_list(&self.eval_context, &path)
                        .map_err(task_eval_error)?;
                    let checkpoint = branch.retry_candidate();
                    if branch.transaction.is_none() {
                        let snapshot = self.host.snapshot();
                        if path_observes_heap(&path) {
                            branch.observe(checkpoint, snapshot.generation());
                        }
                        let (local, _) = split_user_state(branch.state);
                        self.local_state = local;
                        branch.state = self.visible_state(snapshot.heap());
                    } else if path_observes_heap(&path) {
                        branch.observe(checkpoint, 0);
                    }
                    let value = get_state_path(&self.eval_context, &branch.state, &path)?;
                    deliver_value!(value);
                }
                Request::Set(path, value) => {
                    if branch.transaction.is_some() {
                        branch.state =
                            set_state_path(&self.eval_context, branch.state, &path, value)?;
                        deliver_value!(unit_value());
                        continue;
                    }
                    loop {
                        let snapshot = self.host.snapshot();
                        let (local, _) = split_user_state(branch.state.clone());
                        self.local_state = local;
                        let state = set_state_path(
                            &self.eval_context,
                            self.visible_state(snapshot.heap()),
                            &path,
                            value.clone(),
                        )?;
                        let (local, heap) = split_user_state(state);
                        let commit = TaskCommit::new(
                            snapshot.generation(),
                            PublicValue::from_core(heap.clone()),
                            S::Journal::default(),
                        );
                        match self.host.commit(commit) {
                            CommitResult::Committed => {
                                self.local_state = local;
                                branch.state = self.visible_state(&PublicValue::from_core(heap));
                                branch.retry = None;
                                break;
                            }
                            CommitResult::Conflict => {}
                            CommitResult::Closed => return Ok(Drive::Cancelled),
                        }
                    }
                    deliver_value!(unit_value());
                }
                Request::Reset(key, operation) => {
                    let key = value_key(&self.eval_context, key)?;
                    let continuation = self.capture_continuation(CapturedContinuation {
                        sequence: std::mem::take(&mut branch.control.sequence),
                        delimiters: Vec::new(),
                        reset_frames: Vec::new(),
                    })?;
                    let frame = ResetFrame {
                        key,
                        continuation,
                        scope_depth,
                        order: self.allocate_control_order()?,
                    };
                    let mut reset_frames = reset_frames(
                        &self.eval_context,
                        &branch.state,
                        &self.tags.continuation_state,
                    )?;
                    reset_frames.push(frame);
                    branch.state = with_reset_frames(
                        &self.eval_context,
                        branch.state,
                        &self.tags.continuation_state,
                        &reset_frames,
                    )?;
                    branch.effect = operation;
                }
                Request::Shift(key, function) => {
                    let key = value_key(&self.eval_context, key)?;
                    let mut reset_frames = reset_frames(
                        &self.eval_context,
                        &branch.state,
                        &self.tags.continuation_state,
                    )?;
                    let Some(index) = reset_frames.iter().rposition(|frame| frame.key == key)
                    else {
                        return Err(TaskError::new("`.shift` key is not in reset scope"));
                    };
                    let inner_reset_frames = reset_frames.split_off(index + 1);
                    let target = reset_frames.pop().expect("matching reset frame must exist");
                    let first_inner_delimiter = branch
                        .control
                        .delimiters
                        .iter()
                        .position(|delimiter| delimiter.order() > target.order)
                        .unwrap_or(branch.control.delimiters.len());
                    let inner_delimiters =
                        branch.control.delimiters.split_off(first_inner_delimiter);
                    branch.state = with_reset_frames(
                        &self.eval_context,
                        branch.state,
                        &self.tags.continuation_state,
                        &reset_frames,
                    )?;
                    let continuation = self.capture_continuation(CapturedContinuation {
                        sequence: std::mem::take(&mut branch.control.sequence),
                        delimiters: inner_delimiters,
                        reset_frames: inner_reset_frames,
                    })?;
                    branch
                        .control
                        .sequence
                        .push(Continuation::Glam(target.continuation));
                    branch.effect = apply(&self.eval_context, function, vec![continuation])?;
                }
                Request::Resume(task_id, id, value) => {
                    if task_id != self.id {
                        return Err(TaskError::new(
                            "captured continuation belongs to another reflection task",
                        ));
                    }
                    let mut captured = self
                        .continuations
                        .get(&id)
                        .cloned()
                        .ok_or_else(|| TaskError::new("unknown reflection continuation"))?;
                    let caller_sequence = std::mem::take(&mut branch.control.sequence);
                    branch.control.delimiters.push(Delimiter::Resume {
                        outer_sequence: caller_sequence,
                        scope_depth,
                        order: self.allocate_control_order()?,
                    });
                    self.install_captured_control(&mut branch, &mut captured, scope_depth)?;
                    branch.control.sequence = captured.sequence;
                    deliver_value!(value);
                }
                Request::Fix(function) => {
                    let root = Arc::new(FixRoot {
                        function,
                        entry: branch,
                        scope_depth,
                    });
                    branch = self.start_fixpoint(root, Vec::new())?;
                }
                Request::Specialized(request, arguments) => {
                    let checkpoint = branch.retry_candidate();
                    let mut activity = RequestActivity::default();
                    let result = self.specialization.handle_request(
                        request,
                        arguments,
                        &mut RequestContext {
                            eval_context: &self.eval_context,
                            host: self.host.as_ref(),
                            transaction: branch.transaction.as_mut(),
                            activity: &mut activity,
                        },
                    )?;
                    if let Some(generation) = activity.observed_generation {
                        branch.observe(checkpoint, generation);
                    }
                    if activity.committed {
                        branch.retry = None;
                    }
                    match result {
                        RequestResult::Return(value) => deliver_value!(value.into_core()),
                        RequestResult::ReturnUnit => deliver_value!(unit_value()),
                        RequestResult::Fail => return Ok(branch.into_failure()),
                        RequestResult::Cancelled => return Ok(Drive::Cancelled),
                    }
                }
            }
        }
    }

    fn run_cut(
        &mut self,
        operation: Value,
        mut outer: Branch<S>,
        scope_depth: usize,
    ) -> Result<CutResult<S>, TaskError> {
        let owns_transaction = outer.transaction.is_none();
        loop {
            let snapshot = owns_transaction.then(|| self.host.snapshot());
            if let Some(snapshot) = &snapshot {
                self.local_state = split_user_state(outer.state).0;
                outer.state = self.visible_state(snapshot.heap());
                outer.transaction = Some(Transaction::new(snapshot.clone()));
            }
            let mut initial = outer.clone().with_effect(operation.clone());
            initial.control.sequence.clear();
            let mut alternatives = vec![initial];
            let mut retry = None;
            let mut observed_failure = false;

            while let Some(branch) = alternatives.pop() {
                match self.drive(branch, scope_depth)? {
                    Drive::Complete(value, completed) => {
                        if !owns_transaction {
                            return Ok(CutResult::Success(value, completed));
                        }
                        let (local_state, heap) = split_user_state(completed.state.clone());
                        let transaction = completed
                            .transaction
                            .as_ref()
                            .expect("outer cut must own a transaction");
                        let commit = TaskCommit::new(
                            transaction.snapshot.generation(),
                            PublicValue::from_core(heap),
                            transaction.journal.clone(),
                        );
                        match self.host.commit(commit) {
                            CommitResult::Committed => {
                                self.local_state = local_state;
                                let mut completed = completed;
                                completed.transaction = None;
                                completed.state = self.visible_state(&PublicValue::from_core(
                                    split_user_state(completed.state).1,
                                ));
                                return Ok(CutResult::Success(value, completed));
                            }
                            CommitResult::Conflict => {
                                observed_failure = true;
                                retry = Some(completed);
                                break;
                            }
                            CommitResult::Closed => return Ok(CutResult::Cancelled),
                        }
                    }
                    Drive::Fork(left, right) => {
                        alternatives.push(*right);
                        alternatives.push(*left);
                    }
                    Drive::Fail(mut failed) | Drive::Retry(mut failed) => {
                        if let Some(restarted) =
                            self.restart_fixpoint_at_scope(&mut failed, scope_depth)?
                        {
                            alternatives.push(restarted);
                        } else {
                            observed_failure |= failed
                                .transaction
                                .as_ref()
                                .is_some_and(|transaction| transaction.observed);
                            retry = Some(failed);
                        }
                    }
                    Drive::Cancelled => return Ok(CutResult::Cancelled),
                }
            }

            let mut failed = retry.unwrap_or_else(|| outer.clone());
            if observed_failure && let Some(transaction) = failed.transaction.as_mut() {
                transaction.observed = true;
            }
            if failed
                .fix_restarts
                .last()
                .is_some_and(|restart| restart.root.scope_depth < scope_depth)
            {
                return Ok(CutResult::Retry(failed));
            }
            if !owns_transaction {
                return Ok(if failed.is_retryable() {
                    CutResult::Retry(failed)
                } else {
                    CutResult::Fail(failed)
                });
            }
            if !observed_failure {
                failed.transaction = None;
                return Ok(if failed.is_retryable() {
                    CutResult::Retry(failed)
                } else {
                    CutResult::Fail(failed)
                });
            }
            let generation = failed
                .transaction
                .as_ref()
                .map(|transaction| transaction.snapshot.generation())
                .unwrap_or_else(|| self.host.snapshot().generation());
            if !self.host.wait_for_change(generation) {
                return Ok(CutResult::Cancelled);
            }
            outer.transaction = None;
        }
    }

    fn visible_state(&self, heap: &PublicValue) -> Value {
        let Value::Dict(local) = &self.local_state else {
            return Value::error("reflection user state must remain a dictionary");
        };
        Value::Dict(local.insert((*keys::HEAP).clone(), heap.as_core().clone()))
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
        let function = evaluate(&self.eval_context, function)?;
        let request = apply(&self.eval_context, function, vec![self.api.clone()])?;
        let request = evaluate(&self.eval_context, request)?;
        parse_request(
            &self.eval_context,
            request,
            &self.tags,
            &self.specialized_requests,
        )
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
            self.retry = Some(RetryCheckpoint { generation, branch });
        }
    }

    fn is_retryable(&self) -> bool {
        self.retry.is_some()
            || self
                .transaction
                .as_ref()
                .is_some_and(|transaction| transaction.observed)
    }

    fn into_failure(self) -> Drive<S> {
        if self.is_retryable() {
            Drive::Retry(self)
        } else {
            Drive::Fail(self)
        }
    }
}

#[derive(Clone)]
struct RetryCheckpoint<S: TaskSpecialization> {
    generation: u64,
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
    handle: LazyValue,
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
    Fix(LazyValue),
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
    journal: S::Journal,
    observed: bool,
}

impl<S: TaskSpecialization> Transaction<S> {
    fn new(snapshot: HostSnapshot<S>) -> Self {
        Self {
            snapshot,
            journal: S::Journal::default(),
            observed: false,
        }
    }
}

/// Restricted access to the host and current transaction for extra effects.
pub struct RequestContext<'a, S: TaskSpecialization> {
    eval_context: &'a EvalContext,
    host: &'a S::Host,
    transaction: Option<&'a mut Transaction<S>>,
    activity: &'a mut RequestActivity,
}

impl<'a, S: TaskSpecialization> RequestContext<'a, S> {
    pub(crate) fn eval_context(&self) -> &EvalContext {
        self.eval_context
    }

    pub fn host(&self) -> &S::Host {
        self.host
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
}

enum Drive<S: TaskSpecialization> {
    Complete(Value, Branch<S>),
    Fork(Box<Branch<S>>, Box<Branch<S>>),
    Fail(Branch<S>),
    Retry(Branch<S>),
    Cancelled,
}

enum CutResult<S: TaskSpecialization> {
    Success(Value, Branch<S>),
    Retry(Branch<S>),
    Fail(Branch<S>),
    Cancelled,
}

enum Delivery<S: TaskSpecialization> {
    Continue(Branch<S>),
    Complete(Value, Branch<S>),
}

fn deliver<S: TaskSpecialization>(
    context: &EvalContext,
    value: Value,
    mut branch: Branch<S>,
    scope_depth: usize,
    continuation_state: &Key,
) -> Result<Delivery<S>, TaskError> {
    loop {
        if let Some(continuation) = branch.control.sequence.pop() {
            match continuation {
                Continuation::Glam(function) => {
                    branch.effect = apply(context, evaluate(context, function)?, vec![value])?;
                    return Ok(Delivery::Continue(branch));
                }
                Continuation::Fix(handle) => {
                    let active = branch.active_fixes.pop().ok_or_else(|| {
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
                }
            }
            continue;
        }

        let mut resets = reset_frames(context, &branch.state, continuation_state)?;
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
            branch.state = with_reset_frames(context, branch.state, continuation_state, &resets)?;
            branch.effect = apply(context, frame.continuation, vec![value])?;
            return Ok(Delivery::Continue(branch));
        }

        let Some(_) = delimiter_order else {
            return Ok(Delivery::Complete(value, branch));
        };
        match branch
            .control
            .delimiters
            .pop()
            .expect("delimiter order came from a delimiter")
        {
            Delimiter::Resume { outer_sequence, .. } => {
                branch.control.sequence = outer_sequence;
            }
            Delimiter::Restore {
                outer, reset_stack, ..
            } => {
                branch.state =
                    with_reset_stack_value(context, branch.state, continuation_state, reset_stack)?;
                branch.control = *outer;
            }
        }
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
    Reset(Value, Value),
    Shift(Value, Value),
    Resume(TaskId, u64, Value),
    Specialized(R, Vec<PublicValue>),
}

struct SpecializedRequest<R> {
    tag: Key,
    arity: usize,
    request: R,
}

fn parse_request<R: Clone>(
    context: &EvalContext,
    value: Value,
    tags: &Tags,
    specialized: &[SpecializedRequest<R>],
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
        return Ok(Request::Resume(
            TaskId(request_id(context, task_id, "task")?),
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
    Err(TaskError::new("effect API returned an unknown request"))
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
        let api_key = Key::atom_from_text(&spec.api_name);
        if api.get(&api_key).is_some() {
            return Err(TaskError::new(format!(
                "duplicate effect API name `{}`",
                spec.api_name
            )));
        }
        if requests
            .iter()
            .any(|request: &SpecializedRequest<R>| request.tag == tag)
        {
            return Err(TaskError::new(format!(
                "duplicate private tag for effect API name `{}`",
                spec.api_name
            )));
        }
        let value = if spec.arity == 0 {
            nullary_request(tag.clone())
        } else {
            request_function(tag.clone(), spec.arity, Vec::new(), false)
        };
        api = api.insert(api_key, value);
        requests.push(SpecializedRequest {
            tag,
            arity: spec.arity,
            request: spec.request,
        });
    }
    Ok((Value::Dict(api), requests))
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
    while matches!(value, Value::Lazy(_)) {
        value = eval::eval_value(context, &value).map_err(task_eval_error)?;
    }
    Ok(value)
}

fn task_eval_error(error: eval::EvalError) -> TaskError {
    TaskError::new(error.to_string())
}

fn value_key(context: &EvalContext, value: Value) -> Result<Key, TaskError> {
    Key::from_value(&evaluate(context, value)?)
        .ok_or_else(|| TaskError::new("effect index is not keyable"))
}

fn get_state_path(context: &EvalContext, state: &Value, path: &[Key]) -> Result<Value, TaskError> {
    let mut current = state.clone();
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

fn split_user_state(state: Value) -> (Value, Value) {
    let Value::Dict(state) = state else {
        return (
            Value::error("reflection user state must be a dictionary"),
            Value::Dict(Dict::new_sync()),
        );
    };
    let heap = state
        .get(&*keys::HEAP)
        .cloned()
        .unwrap_or_else(|| Value::Dict(Dict::new_sync()));
    (Value::Dict(state.remove(&*keys::HEAP)), heap)
}

fn path_observes_heap(path: &[Key]) -> bool {
    path.first().is_none_or(|key| key == &*keys::HEAP)
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

    #[derive(Clone)]
    struct TestEffects;

    #[derive(Clone)]
    struct ReflectionOnlyEffects;

    impl TaskSpecialization for ReflectionOnlyEffects {
        type Host = TestHost;
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

    #[derive(Clone)]
    enum TestRequest {
        Reflection(ReflectionRequest),
        ReadLog,
        WriteStderr,
    }

    #[derive(Clone)]
    struct TestSnapshot {
        diagnostics: Arc<[Diagnostic]>,
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
                snapshot.generation(),
                snapshot.heap().clone(),
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
                CommitResult::Closed => return Ok(RequestResult::Cancelled),
            }
        }
    }

    #[derive(Default)]
    struct TestHost {
        state: Mutex<TestHostState>,
    }

    struct TestHostState {
        generation: u64,
        heap: PublicValue,
        diagnostics: Vec<Diagnostic>,
        stderr: Vec<Bytes>,
        wake_diagnostic: Option<Diagnostic>,
        wait_count: usize,
        closed: bool,
    }

    impl Default for TestHostState {
        fn default() -> Self {
            Self {
                generation: 1,
                heap: PublicValue::empty_record(),
                diagnostics: Vec::new(),
                stderr: Vec::new(),
                wake_diagnostic: None,
                wait_count: 0,
                closed: false,
            }
        }
    }

    impl TestHost {
        fn with_diagnostics(diagnostics: Vec<Diagnostic>) -> Self {
            Self {
                state: Mutex::new(TestHostState {
                    diagnostics,
                    ..TestHostState::default()
                }),
            }
        }

        fn with_wake_diagnostic(diagnostic: Diagnostic) -> Self {
            Self {
                state: Mutex::new(TestHostState {
                    wake_diagnostic: Some(diagnostic),
                    ..TestHostState::default()
                }),
            }
        }

        fn stderr(&self) -> Vec<Bytes> {
            self.state.lock().unwrap().stderr.clone()
        }

        fn heap(&self) -> PublicValue {
            self.state.lock().unwrap().heap.clone()
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
            let Some(diagnostic) = state.wake_diagnostic.take() else {
                return false;
            };
            state.diagnostics.push(diagnostic);
            state.generation += 1;
            true
        }
    }

    impl ReflectionHost<TestEffects> for TestHost {
        fn emit_diagnostic(&self, diagnostic: Diagnostic) {
            TestHost::emit_diagnostic(self, diagnostic);
        }
    }

    impl ReflectionHost<ReflectionOnlyEffects> for TestHost {
        fn emit_diagnostic(&self, diagnostic: Diagnostic) {
            TestHost::emit_diagnostic(self, diagnostic);
        }
    }

    impl TaskHost<TestEffects> for TestHost {
        fn snapshot(&self) -> HostSnapshot<TestEffects> {
            let state = self.state.lock().unwrap();
            HostSnapshot::new(
                state.generation,
                state.heap.clone(),
                TestSnapshot {
                    diagnostics: Arc::from(state.diagnostics.clone()),
                },
            )
        }

        fn commit(&self, commit: TaskCommit<TestEffects>) -> CommitResult {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return CommitResult::Closed;
            }
            if state.generation != commit.generation() {
                return CommitResult::Conflict;
            }
            state.heap = commit.heap().clone();
            let consumed = commit
                .extra()
                .consumed_diagnostics
                .min(state.diagnostics.len());
            state.diagnostics.drain(..consumed);
            state
                .diagnostics
                .extend(commit.extra().reflection.diagnostics().iter().cloned());
            state.stderr.extend_from_slice(&commit.extra().stderr);
            state.generation += 1;
            CommitResult::Committed
        }

        fn wait_for_change(&self, observed_generation: u64) -> bool {
            TestHost::wait_for_change(self, observed_generation)
        }
    }

    impl TaskHost<StandardEffects> for TestHost {
        fn snapshot(&self) -> HostSnapshot<StandardEffects> {
            let state = self.state.lock().unwrap();
            HostSnapshot::new(state.generation, state.heap.clone(), ())
        }

        fn commit(&self, commit: TaskCommit<StandardEffects>) -> CommitResult {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return CommitResult::Closed;
            }
            if state.generation != commit.generation() {
                return CommitResult::Conflict;
            }
            state.heap = commit.heap().clone();
            state.generation += 1;
            CommitResult::Committed
        }

        fn wait_for_change(&self, observed_generation: u64) -> bool {
            TestHost::wait_for_change(self, observed_generation)
        }
    }

    impl TaskHost<ReflectionOnlyEffects> for TestHost {
        fn snapshot(&self) -> HostSnapshot<ReflectionOnlyEffects> {
            let state = self.state.lock().unwrap();
            HostSnapshot::new(state.generation, state.heap.clone(), ())
        }

        fn commit(&self, commit: TaskCommit<ReflectionOnlyEffects>) -> CommitResult {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return CommitResult::Closed;
            }
            if state.generation != commit.generation() {
                return CommitResult::Conflict;
            }
            state.heap = commit.heap().clone();
            state
                .diagnostics
                .extend(commit.extra().diagnostics().iter().cloned());
            state.generation += 1;
            CommitResult::Committed
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
        run(effect, ReflectionOnlyEffects, host)
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
            .script("g", format!("language g0\neffect = {source}\n"))
            .build()
            .expect("effect fixture should compile");
        let effect = assembler
            .get(module.value(), "effect")
            .expect("effect fixture should define effect");
        (assembler, effect)
    }

    fn completed(source: &str) -> (Assembler, PublicValue) {
        let (assembler, effect) = compile_effect(source);
        let TaskOutcome::Complete(value) = run_standard_test(&effect).unwrap() else {
            panic!("finite effect should complete")
        };
        (assembler, value)
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
    fn root_user_state_replacement_can_replace_the_shared_heap_subtree() {
        let (assembler, effect) = compile_effect(
            ".cut ((.set [] { heap:{ answer:\"shared\" }, local:\"owned\" }) =>> .get ['heap,'answer])",
        );
        let host = Arc::new(TestHost::default());
        let TaskOutcome::Complete(value) = run_standard_on(&effect, host.clone()).unwrap() else {
            panic!("state effect should complete")
        };
        assert_eq!(assembler.to_binary(&value).unwrap(), b"shared".as_slice());
        assert_eq!(
            assembler.get(&host.heap(), "answer").unwrap().as_binary(),
            Some(b"shared".as_slice())
        );
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
