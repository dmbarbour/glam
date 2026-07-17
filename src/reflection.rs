//! External freer-effect task interpreter used by reflection clients.
//!
//! Effect requests are ordinary core values sealed by private abstract-global
//! tags. Interaction-net operators only construct those values; this module
//! performs the state, control, transaction, and host operations.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use bytes::Bytes;

use crate::api::{Diagnostic, Value as PublicValue};
use crate::core::{Atom, Dict, FunctionValue, Key, LazyValue, List, NetValue, Value, keys};
use crate::core_net::CoreSpecialization;
use crate::eval;
use crate::interaction_net::NetBuilder;
use crate::number::Number;

/// Immutable host state observed at the start of an optimistic transaction.
#[derive(Debug, Clone)]
pub struct HostSnapshot {
    generation: u64,
    heap: PublicValue,
    diagnostics: Arc<[Diagnostic]>,
    closed: bool,
}

impl HostSnapshot {
    pub fn new(
        generation: u64,
        heap: PublicValue,
        diagnostics: impl Into<Arc<[Diagnostic]>>,
        closed: bool,
    ) -> Self {
        Self {
            generation,
            heap,
            diagnostics: diagnostics.into(),
            closed,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn heap(&self) -> &PublicValue {
        &self.heap
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

/// Changes to host-owned resources produced by one successful outer cut.
#[derive(Debug)]
pub struct TaskCommit {
    generation: u64,
    heap: PublicValue,
    consumed_diagnostics: usize,
    stderr: Vec<Bytes>,
}

impl TaskCommit {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn heap(&self) -> &PublicValue {
        &self.heap
    }

    pub fn consumed_diagnostics(&self) -> usize {
        self.consumed_diagnostics
    }

    pub fn stderr(&self) -> &[Bytes] {
        &self.stderr
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitResult {
    Committed,
    Conflict,
    Closed,
}

/// Host-owned transactional resources available to a reflection task.
pub trait TaskHost: Send + Sync {
    fn snapshot(&self) -> HostSnapshot;
    fn commit(&self, commit: TaskCommit) -> CommitResult;

    /// Waits until the observed generation changes. Returns false when the
    /// task should stop rather than retry.
    fn wait_for_change(&self, observed_generation: u64) -> bool;

    /// Enqueues output outside an explicit cut as a single-operation commit.
    fn write_stderr(&self, bytes: Bytes);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutcome {
    Complete(PublicValue),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskError(Arc<str>);

impl TaskError {
    fn new(message: impl Into<Arc<str>>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TaskError {}

/// Runs one reflection effect until it returns or its host closes.
pub fn run(effect: &PublicValue, host: Arc<dyn TaskHost>) -> Result<TaskOutcome, TaskError> {
    EffectTask::new(host).run(effect.as_core().clone())
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
    read_log: Key,
    write_stderr: Key,
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
            read_log: tag("read_log"),
            write_stderr: tag("write_stderr"),
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

struct EffectTask {
    host: Arc<dyn TaskHost>,
    tags: Tags,
    api: Value,
    local_state: Value,
    next_continuation: u64,
    next_control_order: usize,
    continuations: HashMap<u64, CapturedContinuation>,
}

impl EffectTask {
    fn new(host: Arc<dyn TaskHost>) -> Self {
        let tags = Tags::new();
        let api = effect_api(&tags);
        Self {
            host,
            tags,
            api,
            local_state: Value::Dict(Dict::new_sync()),
            next_continuation: 1,
            next_control_order: 1,
            continuations: HashMap::new(),
        }
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
            2,
            vec![Value::Number(Number::from_u64(id))],
            true,
        ))
    }

    fn install_captured_control(
        &mut self,
        branch: &mut Branch,
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

        let mut reset_frames = reset_frames(&branch.state, &self.tags.continuation_state)?;
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
            branch.state.clone(),
            &self.tags.continuation_state,
            &reset_frames,
        )?;
        Ok(())
    }

    fn run(&mut self, effect: Value) -> Result<TaskOutcome, TaskError> {
        let branch = Branch::new(effect, self.visible_state(self.host.snapshot().heap()));
        match self.drive(branch, 0)? {
            Drive::Complete(value, completed) => {
                self.local_state = split_user_state(completed.state).0;
                Ok(TaskOutcome::Complete(PublicValue::from_core(value)))
            }
            Drive::Fail(_) => Err(TaskError::new("reflection task failed outside `.cut`")),
            Drive::Fork(_, _) => Err(TaskError::new("`.alt` requires an enclosing `.cut`")),
            Drive::Retry(_) => Err(TaskError::new("retry escaped an enclosing `.cut`")),
            Drive::Cancelled => Ok(TaskOutcome::Cancelled),
        }
    }

    fn drive(&mut self, mut branch: Branch, scope_depth: usize) -> Result<Drive, TaskError> {
        macro_rules! deliver_value {
            ($value:expr) => {
                match deliver($value, branch, scope_depth, &self.tags.continuation_state)? {
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
                    match deliver(value, branch, scope_depth, &self.tags.continuation_state)? {
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
                    return Ok(Drive::Fork(
                        Box::new(branch.with_effect(left)),
                        Box::new(branch.with_effect(right)),
                    ));
                }
                Request::Fail => return Ok(Drive::Fail(branch)),
                Request::Cut(operation) => {
                    let outer_sequence = std::mem::take(&mut branch.control.sequence);
                    match self.run_cut(operation, branch, scope_depth + 1)? {
                        CutResult::Success(value, mut completed) => {
                            completed.control.sequence = outer_sequence;
                            match deliver(
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
                        CutResult::Retry(branch) => return Ok(Drive::Retry(branch)),
                        CutResult::Cancelled => return Ok(Drive::Cancelled),
                    }
                }
                Request::Get(path) => {
                    if branch.transaction.is_none() {
                        let snapshot = self.host.snapshot();
                        let (local, _) = split_user_state(branch.state);
                        self.local_state = local;
                        branch.state = self.visible_state(snapshot.heap());
                    }
                    let value = get_state_path(&branch.state, &path)?;
                    deliver_value!(value);
                }
                Request::Set(path, value) => {
                    if branch.transaction.is_some() {
                        branch.state = set_state_path(branch.state, &path, value)?;
                        deliver_value!(unit_value());
                        continue;
                    }
                    loop {
                        let snapshot = self.host.snapshot();
                        let (local, _) = split_user_state(branch.state.clone());
                        self.local_state = local;
                        let state = set_state_path(
                            self.visible_state(snapshot.heap()),
                            &path,
                            value.clone(),
                        )?;
                        let (local, heap) = split_user_state(state);
                        let commit = TaskCommit {
                            generation: snapshot.generation(),
                            heap: PublicValue::from_core(heap.clone()),
                            consumed_diagnostics: 0,
                            stderr: Vec::new(),
                        };
                        match self.host.commit(commit) {
                            CommitResult::Committed => {
                                self.local_state = local;
                                branch.state = self.visible_state(&PublicValue::from_core(heap));
                                break;
                            }
                            CommitResult::Conflict => {}
                            CommitResult::Closed => return Ok(Drive::Cancelled),
                        }
                    }
                    deliver_value!(unit_value());
                }
                Request::Reset(key, operation) => {
                    let key = value_key(key)?;
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
                    let mut reset_frames =
                        reset_frames(&branch.state, &self.tags.continuation_state)?;
                    reset_frames.push(frame);
                    branch.state = with_reset_frames(
                        branch.state,
                        &self.tags.continuation_state,
                        &reset_frames,
                    )?;
                    branch.effect = operation;
                }
                Request::Shift(key, function) => {
                    let key = value_key(key)?;
                    let mut reset_frames =
                        reset_frames(&branch.state, &self.tags.continuation_state)?;
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
                    branch.effect = apply(function, vec![continuation])?;
                }
                Request::Resume(id, value) => {
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
                    // TODO(reflection backtracking): if a fixpoint result is
                    // initialized and a later continuation fails, a sibling
                    // alternative needs its own future rather than this
                    // already-initialized handle.
                    let handle = LazyValue::pending("reflection effect fixpoint");
                    let marker = Value::Lazy(handle.clone());
                    let operation = apply(function, vec![marker])?;
                    let outer_control = std::mem::take(&mut branch.control);
                    let reset_stack =
                        reset_stack_value(&branch.state, &self.tags.continuation_state)?;
                    branch.state =
                        with_reset_frames(branch.state, &self.tags.continuation_state, &[])?;
                    branch.control.sequence.push(Continuation::Fix(handle));
                    branch.control.delimiters.push(Delimiter::Restore {
                        outer: Box::new(outer_control),
                        reset_stack,
                        scope_depth,
                        order: self.allocate_control_order()?,
                    });
                    branch.effect = operation;
                }
                Request::ReadLog => {
                    let Some(transaction) = branch.transaction.as_mut() else {
                        match self.read_log_autocommit()? {
                            Some(value) => deliver_value!(value),
                            None => return Ok(Drive::Cancelled),
                        }
                        continue;
                    };
                    if let Some(diagnostic) = transaction
                        .snapshot
                        .diagnostics()
                        .get(transaction.consumed_diagnostics)
                    {
                        let value = diagnostic
                            .enrich()
                            .map_err(|error| TaskError::new(error.to_string()))?
                            .into_core();
                        transaction.consumed_diagnostics += 1;
                        deliver_value!(value);
                    } else if transaction.snapshot.is_closed() {
                        return Ok(Drive::Cancelled);
                    } else {
                        return Ok(Drive::Fail(branch));
                    }
                }
                Request::WriteStderr(value) => {
                    let bytes = value_bytes(&value)?;
                    if let Some(transaction) = branch.transaction.as_mut() {
                        transaction.stderr.push(bytes);
                    } else {
                        self.host.write_stderr(bytes);
                    }
                    deliver_value!(unit_value());
                }
            }
        }
    }

    fn run_cut(
        &mut self,
        operation: Value,
        mut outer: Branch,
        scope_depth: usize,
    ) -> Result<CutResult, TaskError> {
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
                        let commit = TaskCommit {
                            generation: transaction.snapshot.generation(),
                            heap: PublicValue::from_core(heap),
                            consumed_diagnostics: transaction.consumed_diagnostics,
                            stderr: transaction.stderr.clone(),
                        };
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
                    Drive::Fail(failed) | Drive::Retry(failed) => retry = Some(failed),
                    Drive::Cancelled => return Ok(CutResult::Cancelled),
                }
            }

            let failed = retry.unwrap_or_else(|| outer.clone());
            if !owns_transaction {
                return Ok(CutResult::Retry(failed));
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

    fn read_log_autocommit(&mut self) -> Result<Option<Value>, TaskError> {
        loop {
            let snapshot = self.host.snapshot();
            let Some(diagnostic) = snapshot.diagnostics().first() else {
                if snapshot.is_closed() || !self.host.wait_for_change(snapshot.generation()) {
                    return Ok(None);
                }
                continue;
            };
            let value = diagnostic
                .enrich()
                .map_err(|error| TaskError::new(error.to_string()))?
                .into_core();
            let commit = TaskCommit {
                generation: snapshot.generation(),
                heap: snapshot.heap().clone(),
                consumed_diagnostics: 1,
                stderr: Vec::new(),
            };
            match self.host.commit(commit) {
                CommitResult::Committed => return Ok(Some(value)),
                CommitResult::Conflict => {}
                CommitResult::Closed => return Ok(None),
            }
        }
    }

    fn visible_state(&self, heap: &PublicValue) -> Value {
        let Value::Dict(local) = &self.local_state else {
            return Value::error("reflection user state must remain a dictionary");
        };
        Value::Dict(local.insert((*keys::HEAP).clone(), heap.as_core().clone()))
    }

    fn effect_request(&self, effect: Value) -> Result<Request, TaskError> {
        let effect = evaluate(effect)?;
        let Value::Dict(effect) = effect else {
            return Err(TaskError::new(format!(
                "reflection task requires an effect object, got {effect:?}"
            )));
        };
        let function = effect
            .get(&*keys::EFF)
            .cloned()
            .ok_or_else(|| TaskError::new("reflection effect has no `eff` member"))?;
        let request = evaluate(apply(evaluate(function)?, vec![self.api.clone()])?)?;
        parse_request(request, &self.tags)
    }
}

#[derive(Clone)]
struct Branch {
    effect: Value,
    control: Control,
    state: Value,
    transaction: Option<Transaction>,
}

impl Branch {
    fn new(effect: Value, state: Value) -> Self {
        Self {
            effect,
            control: Control::default(),
            state,
            transaction: None,
        }
    }

    fn with_effect(&self, effect: Value) -> Self {
        let mut branch = self.clone();
        branch.effect = effect;
        branch
    }
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
struct Transaction {
    snapshot: HostSnapshot,
    consumed_diagnostics: usize,
    stderr: Vec<Bytes>,
}

impl Transaction {
    fn new(snapshot: HostSnapshot) -> Self {
        Self {
            snapshot,
            consumed_diagnostics: 0,
            stderr: Vec::new(),
        }
    }
}

enum Drive {
    Complete(Value, Branch),
    Fork(Box<Branch>, Box<Branch>),
    Fail(Branch),
    Retry(Branch),
    Cancelled,
}

enum CutResult {
    Success(Value, Branch),
    Retry(Branch),
    Cancelled,
}

enum Delivery {
    Continue(Branch),
    Complete(Value, Branch),
}

fn deliver(
    value: Value,
    mut branch: Branch,
    scope_depth: usize,
    continuation_state: &Key,
) -> Result<Delivery, TaskError> {
    loop {
        if let Some(continuation) = branch.control.sequence.pop() {
            match continuation {
                Continuation::Glam(function) => {
                    branch.effect = apply(evaluate(function)?, vec![value])?;
                    return Ok(Delivery::Continue(branch));
                }
                Continuation::Fix(handle) => {
                    handle
                        .set(value.clone())
                        .map_err(|_| TaskError::new("reflection fixpoint initialized twice"))?;
                }
            }
            continue;
        }

        let mut resets = reset_frames(&branch.state, continuation_state)?;
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
            branch.state = with_reset_frames(branch.state, continuation_state, &resets)?;
            branch.effect = apply(frame.continuation, vec![value])?;
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
                    with_reset_stack_value(branch.state, continuation_state, reset_stack)?;
                branch.control = *outer;
            }
        }
    }
}

enum Request {
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
    Resume(u64, Value),
    ReadLog,
    WriteStderr(Value),
}

fn parse_request(value: Value, tags: &Tags) -> Result<Request, TaskError> {
    let Value::Dict(dict) = value else {
        return Err(TaskError::new("effect API returned a non-request value"));
    };
    let parse = |tag: &Key| -> Result<Option<Vec<Value>>, TaskError> {
        dict.get(tag)
            .map(|payload| {
                let Value::List(payload) = evaluate(payload.clone())? else {
                    return Err(TaskError::new("effect request payload must be a list"));
                };
                eval::list_to_value_items(&payload).map_err(task_eval_error)
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
    args!(&tags.resume, 2, |[id, value]: [Value; 2]| {
        let Value::Number(id) = id else {
            return Request::Resume(0, Value::error("invalid continuation ID"));
        };
        let id = id
            .to_i64_if_integer()
            .and_then(|id| u64::try_from(id).ok())
            .unwrap_or(0);
        Request::Resume(id, value)
    });
    args!(&tags.read_log, 0, |[]: [Value; 0]| Request::ReadLog);
    args!(&tags.write_stderr, 1, |[value]: [Value; 1]| {
        Request::WriteStderr(value)
    });
    Err(TaskError::new("effect API returned an unknown request"))
}

fn effect_api(tags: &Tags) -> Value {
    let entry = |name: &str, value| (Key::atom_from_text(name), value);
    Value::Dict(
        [
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
            entry("read_log", nullary_request(tags.read_log.clone())),
            entry(
                "write_stderr",
                request_function(tags.write_stderr.clone(), 1, Vec::new(), false),
            ),
        ]
        .into_iter()
        .fold(Dict::new_sync(), |dict, (key, value)| {
            dict.insert(key, value)
        }),
    )
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

fn apply(function: Value, arguments: Vec<Value>) -> Result<Value, TaskError> {
    eval::apply_values(function, arguments).map_err(task_eval_error)
}

fn evaluate(value: Value) -> Result<Value, TaskError> {
    let mut value = value;
    while matches!(value, Value::Lazy(_)) {
        value = eval::eval_value(&value).map_err(task_eval_error)?;
    }
    Ok(value)
}

fn task_eval_error(error: eval::EvalError) -> TaskError {
    TaskError::new(error.to_string())
}

fn value_key(value: Value) -> Result<Key, TaskError> {
    Key::from_value(&evaluate(value)?).ok_or_else(|| TaskError::new("effect index is not keyable"))
}

fn get_state_path(state: &Value, path: &Value) -> Result<Value, TaskError> {
    let path = eval::eval_key_path_list(path).map_err(task_eval_error)?;
    let mut current = state.clone();
    for key in path {
        let Value::Dict(dict) = evaluate(current)? else {
            return Err(TaskError::new(
                "state path traverses a non-dictionary value",
            ));
        };
        current = dict
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Value::Dict(Dict::new_sync()));
    }
    Ok(current)
}

fn set_state_path(state: Value, path: &Value, value: Value) -> Result<Value, TaskError> {
    let path = eval::eval_key_path_list(path).map_err(task_eval_error)?;
    if path.is_empty() {
        return require_state_dict(value);
    }
    let path = Value::List(List::from_values(path.into_iter().map(key_value).collect()));
    evaluate(Value::builtin_call(
        crate::core::Builtin::DictUpdate,
        vec![path, value, require_state_dict(state)?],
    ))
}

fn require_state_dict(value: Value) -> Result<Value, TaskError> {
    match evaluate(value)? {
        value @ Value::Dict(_) => Ok(value),
        _ => Err(TaskError::new("reflection user state must be a dictionary")),
    }
}

fn reset_stack_value(state: &Value, continuation_state: &Key) -> Result<Value, TaskError> {
    let Value::Dict(state) = state else {
        return Err(TaskError::new("reflection user state must be a dictionary"));
    };
    let stack = state
        .get(continuation_state)
        .cloned()
        .unwrap_or_else(|| Value::List(List::empty()));
    reset_frames_from_value(&stack)?;
    Ok(stack)
}

fn reset_frames(state: &Value, continuation_state: &Key) -> Result<Vec<ResetFrame>, TaskError> {
    reset_frames_from_value(&reset_stack_value(state, continuation_state)?)
}

fn reset_frames_from_value(stack: &Value) -> Result<Vec<ResetFrame>, TaskError> {
    let Value::List(stack) = evaluate(stack.clone())? else {
        return Err(TaskError::new(
            "reflection continuation state must be a list",
        ));
    };
    eval::list_to_value_items(&stack)
        .map_err(task_eval_error)?
        .into_iter()
        .map(|frame| {
            let Value::List(frame) = evaluate(frame)? else {
                return Err(TaskError::new(
                    "reflection continuation frame must be a list",
                ));
            };
            let [key, continuation, scope_depth, order]: [Value; 4] =
                eval::list_to_value_items(&frame)
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
                key: value_key(key)?,
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
    state: Value,
    continuation_state: &Key,
    frames: &[ResetFrame],
) -> Result<Value, TaskError> {
    with_reset_stack_value(state, continuation_state, reset_frames_value(frames))
}

fn with_reset_stack_value(
    state: Value,
    continuation_state: &Key,
    stack: Value,
) -> Result<Value, TaskError> {
    reset_frames_from_value(&stack)?;
    let Value::Dict(state) = require_state_dict(state)? else {
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

fn value_bytes(value: &Value) -> Result<Bytes, TaskError> {
    match evaluate(value.clone())? {
        Value::Binary(bytes) => Ok(bytes),
        Value::List(list) => eval::list_output_bytes(&list)
            .map(Bytes::from)
            .map_err(TaskError::new),
        _ => Err(TaskError::new("`.write_stderr` requires binary data")),
    }
}

fn unit_value() -> Value {
    (*keys::UNIT_VALUE).clone()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::api::Assembler;

    #[derive(Default)]
    struct TestHost {
        state: Mutex<TestHostState>,
    }

    struct TestHostState {
        generation: u64,
        heap: PublicValue,
        diagnostics: Vec<Diagnostic>,
        stderr: Vec<Bytes>,
        closed: bool,
    }

    impl Default for TestHostState {
        fn default() -> Self {
            Self {
                generation: 1,
                heap: PublicValue::empty_record(),
                diagnostics: Vec::new(),
                stderr: Vec::new(),
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

        fn stderr(&self) -> Vec<Bytes> {
            self.state.lock().unwrap().stderr.clone()
        }

        fn heap(&self) -> PublicValue {
            self.state.lock().unwrap().heap.clone()
        }
    }

    impl TaskHost for TestHost {
        fn snapshot(&self) -> HostSnapshot {
            let state = self.state.lock().unwrap();
            HostSnapshot::new(
                state.generation,
                state.heap.clone(),
                Arc::from(state.diagnostics.clone()),
                state.closed,
            )
        }

        fn commit(&self, commit: TaskCommit) -> CommitResult {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return CommitResult::Closed;
            }
            if state.generation != commit.generation() {
                return CommitResult::Conflict;
            }
            state.heap = commit.heap().clone();
            let consumed = commit.consumed_diagnostics().min(state.diagnostics.len());
            state.diagnostics.drain(..consumed);
            state.stderr.extend_from_slice(commit.stderr());
            state.generation += 1;
            CommitResult::Committed
        }

        fn wait_for_change(&self, _observed_generation: u64) -> bool {
            false
        }

        fn write_stderr(&self, bytes: Bytes) {
            self.state.lock().unwrap().stderr.push(bytes);
        }
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
        let TaskOutcome::Complete(value) = run(&effect, Arc::new(TestHost::default())).unwrap()
        else {
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
    fn fixpoint_hides_then_restores_the_reset_stack() {
        let (_, hidden) = compile_effect(
            ".reset \"prompt\" (.fix (\\self -> .shift \"prompt\" (\\continuation -> continuation \"wrong\")))",
        );
        assert!(
            run(&hidden, Arc::new(TestHost::default()))
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
    fn replacing_root_state_replaces_the_active_reset_stack() {
        let (_, effect) = compile_effect(
            ".reset \"prompt\" ((.set [] {}) =>> .shift \"prompt\" (\\continuation -> continuation \"wrong\"))",
        );
        assert!(
            run(&effect, Arc::new(TestHost::default()))
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
            run(&effect, host.clone()).unwrap(),
            TaskOutcome::Complete(_)
        ));
        assert_eq!(host.stderr(), [Bytes::from_static(b"good")]);
        assert!(host.snapshot().diagnostics().is_empty());
    }

    #[test]
    fn root_user_state_replacement_can_replace_the_shared_heap_subtree() {
        let (assembler, effect) = compile_effect(
            ".cut ((.set [] { heap:{ answer:\"shared\" }, local:\"owned\" }) =>> .get ['heap,'answer])",
        );
        let host = Arc::new(TestHost::default());
        let TaskOutcome::Complete(value) = run(&effect, host.clone()).unwrap() else {
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
            run(&alternative, Arc::new(TestHost::default()))
                .unwrap_err()
                .to_string()
                .contains("requires an enclosing `.cut`")
        );

        let (_, shift) = compile_effect(".shift \"missing\" (\\continuation -> .r continuation)");
        assert!(
            run(&shift, Arc::new(TestHost::default()))
                .unwrap_err()
                .to_string()
                .contains("not in reset scope")
        );
    }
}
