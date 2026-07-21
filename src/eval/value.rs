use std::fmt;
use std::sync::Arc;

use crate::core::{
    EvaluatedValue, FixpointComputation, Key, LazyFailure, LazySource, LazyValue, List, ListThunk,
    PromisedValue, Value, keys,
};
use crate::core_net::CoreWaitToken;
use crate::evaluation::{
    EvalContext, EvaluationMachinePoll, EvaluationPumpOutcome, EvaluationTaskBlock,
    EvaluationTaskMachine, EvaluationTaskPoll,
};
use crate::list::ListItem;
use crate::number::Number;

use super::application::{apply_value, apply_values};
use super::builtins::{apply_builtin, construct_fixpoint_object, is_undefined_value};
use super::net::*;
use super::sequence::list_to_key_items;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalError {
    kind: EvalErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvalErrorKind {
    Message(String),
    LazyFailure(Arc<LazyFailure>),
    Blocked(CoreWaitToken),
    UnassignedPromise(PromisedValue),
}

impl EvalError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            kind: EvalErrorKind::Message(message.into()),
        }
    }

    pub(super) fn blocked(wait: CoreWaitToken) -> Self {
        Self {
            kind: EvalErrorKind::Blocked(wait),
        }
    }

    fn lazy_failure(failure: Arc<LazyFailure>) -> Self {
        Self {
            kind: EvalErrorKind::LazyFailure(failure),
        }
    }

    fn into_lazy_failure(self) -> Arc<LazyFailure> {
        match self.kind {
            EvalErrorKind::LazyFailure(failure) => failure,
            other => Arc::new(LazyFailure::evaluation(Self { kind: other }.to_string())),
        }
    }

    pub(crate) fn blocked_on(&self) -> Option<CoreWaitToken> {
        match &self.kind {
            EvalErrorKind::Blocked(wait) => Some(wait.clone()),
            EvalErrorKind::Message(_)
            | EvalErrorKind::LazyFailure(_)
            | EvalErrorKind::UnassignedPromise(_) => None,
        }
    }

    fn unassigned_promise(&self) -> Option<&PromisedValue> {
        match &self.kind {
            EvalErrorKind::UnassignedPromise(promise) => Some(promise),
            EvalErrorKind::Message(_)
            | EvalErrorKind::LazyFailure(_)
            | EvalErrorKind::Blocked(_) => None,
        }
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            EvalErrorKind::Message(message) => f.write_str(message),
            EvalErrorKind::LazyFailure(failure) => failure.fmt(f),
            EvalErrorKind::Blocked(wait) => {
                write!(f, "evaluation is blocked on wait token {}", wait.wait_id())
            }
            EvalErrorKind::UnassignedPromise(_) => {
                f.write_str("promised value was observed before initialization")
            }
        }
    }
}

impl std::error::Error for EvalError {}

pub fn eval_value(context: &EvalContext, value: &Value) -> Result<Value, EvalError> {
    match value {
        Value::Lazy(lazy) => eval_lazy(context, lazy),
        Value::Promised(promise) => eval_promised(context, promise),
        other => Ok(other.clone()),
    }
}

enum LazyTaskWork {
    Produce,
    Follow(Value),
}

struct LazyTaskMachine {
    context: EvalContext,
    lazy: LazyValue,
    work: LazyTaskWork,
}

impl LazyTaskMachine {
    fn poll_source(&mut self) -> Result<Value, EvalError> {
        match &self.work {
            LazyTaskWork::Produce => produce_lazy_source(&self.context, &self.lazy),
            LazyTaskWork::Follow(target) => eval_value(&self.context, target),
        }
    }

    fn complete(&self, value: Value) -> EvaluationMachinePoll {
        let value = EvaluatedValue::try_from(value)
            .expect("WHNF demand must eliminate the outer deferred variant");
        match self.lazy.cache(Ok(value)) {
            Ok(value) => EvaluationMachinePoll::Complete(value.into_value()),
            Err(error) => EvaluationMachinePoll::LazyFailed(error),
        }
    }
}

impl EvaluationTaskMachine for LazyTaskMachine {
    fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
        if let Some(result) = self.lazy.cached() {
            return match result {
                Ok(value) => EvaluationMachinePoll::Complete(value.into_value()),
                Err(error) => EvaluationMachinePoll::LazyFailed(error),
            };
        }

        match self.poll_source() {
            Ok(value) if is_deferred(&value) => {
                self.work = LazyTaskWork::Follow(value);
                EvaluationMachinePoll::Yielded
            }
            Ok(value) => self.complete(value),
            Err(error) => {
                if let Some(wait) = error.blocked_on() {
                    return EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                        lazy: Some(wait.0),
                        observed_generation: None,
                        error: None,
                    });
                }
                if let Some(promise) = error.unassigned_promise() {
                    let wait = match promise_wait(&self.context, promise) {
                        Ok(wait) => wait,
                        Err(error) => return EvaluationMachinePoll::Failed(error),
                    };
                    return EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                        lazy: Some(wait),
                        observed_generation: None,
                        error: None,
                    });
                }
                let failure = error.into_lazy_failure();
                match self.lazy.cache(Err(failure)) {
                    Ok(value) => EvaluationMachinePoll::Complete(value.into_value()),
                    Err(error) => EvaluationMachinePoll::LazyFailed(error),
                }
            }
        }
    }
}

enum PromiseFollowerState {
    AwaitAssignment,
    FollowAssignment(Value),
}

struct PromiseFollower {
    context: EvalContext,
    promise: PromisedValue,
    state: PromiseFollowerState,
}

impl EvaluationTaskMachine for PromiseFollower {
    fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
        let result = match &self.state {
            PromiseFollowerState::AwaitAssignment => match self.promise.assignment() {
                Some(result) => result.map_err(|error| EvalError::new(error.as_ref())),
                None => {
                    let Some(task) = self.promise.task() else {
                        return EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                            lazy: None,
                            observed_generation: None,
                            error: None,
                        });
                    };
                    return match self.context.poll_wait(task.wait()) {
                        EvaluationTaskPoll::Pending(wait) => {
                            EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                                lazy: Some(wait),
                                observed_generation: None,
                                error: None,
                            })
                        }
                        EvaluationTaskPoll::Complete(_) => match self.promise.assignment() {
                            Some(result) => match result {
                                Ok(value) => {
                                    self.state = PromiseFollowerState::FollowAssignment(value);
                                    EvaluationMachinePoll::Yielded
                                }
                                Err(error) => EvaluationMachinePoll::Failed(error),
                            },
                            None => EvaluationMachinePoll::Failed(Arc::from(
                                "promised value's producer completed without assigning it",
                            )),
                        },
                        EvaluationTaskPoll::Failed(error) => EvaluationMachinePoll::Failed(error),
                        EvaluationTaskPoll::Cancelled => EvaluationMachinePoll::Failed(Arc::from(
                            "promised value's producer was cancelled",
                        )),
                        EvaluationTaskPoll::ForeignSession => EvaluationMachinePoll::Failed(
                            Arc::from("promised value belongs to another evaluation session"),
                        ),
                    };
                }
            },
            PromiseFollowerState::FollowAssignment(target) => eval_value(&self.context, target),
        };

        match result {
            Ok(value) if is_deferred(&value) => {
                self.state = PromiseFollowerState::FollowAssignment(value);
                EvaluationMachinePoll::Yielded
            }
            Ok(value) => EvaluationMachinePoll::Complete(value),
            Err(error) => block_or_fail(&self.context, error),
        }
    }
}

fn is_deferred(value: &Value) -> bool {
    matches!(value, Value::Lazy(_) | Value::Promised(_))
}

fn promise_wait(
    context: &EvalContext,
    promise: &PromisedValue,
) -> Result<crate::evaluation::EvaluationWaitToken, Arc<str>> {
    context.promise_task(promise, |task_context| {
        Box::new(PromiseFollower {
            context: task_context,
            promise: promise.clone(),
            state: PromiseFollowerState::AwaitAssignment,
        })
    })
}

fn block_or_fail(context: &EvalContext, error: EvalError) -> EvaluationMachinePoll {
    if let Some(wait) = error.blocked_on() {
        return EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
            lazy: Some(wait.0),
            observed_generation: None,
            error: None,
        });
    }
    if let Some(promise) = error.unassigned_promise() {
        return match promise_wait(context, promise) {
            Ok(wait) => EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                lazy: Some(wait),
                observed_generation: None,
                error: None,
            }),
            Err(error) => EvaluationMachinePoll::Failed(error),
        };
    }
    EvaluationMachinePoll::LazyFailed(error.into_lazy_failure())
}

pub(super) fn eval_lazy(context: &EvalContext, lazy: &LazyValue) -> Result<Value, EvalError> {
    if let Some(result) = lazy.cached() {
        return result
            .map(EvaluatedValue::into_value)
            .map_err(EvalError::lazy_failure);
    }
    let wait = context
        .lazy_task(lazy, |task_context| {
            Box::new(LazyTaskMachine {
                context: task_context,
                lazy: lazy.clone(),
                work: LazyTaskWork::Produce,
            })
        })
        .map_err(|error| EvalError::new(error.as_ref()))?;
    await_deferred_task(context, wait, "lazy value")
}

fn await_deferred_task(
    context: &EvalContext,
    wait: crate::evaluation::EvaluationWaitToken,
    kind: &str,
) -> Result<Value, EvalError> {
    match context.poll_wait(&wait) {
        EvaluationTaskPoll::Complete(value) => return Ok(value),
        EvaluationTaskPoll::Failed(error) => {
            return Err(deferred_task_failure(context, &wait, error));
        }
        EvaluationTaskPoll::Cancelled => {
            return Err(EvalError::new(format!("{kind} evaluation was cancelled")));
        }
        EvaluationTaskPoll::ForeignSession => {
            return Err(EvalError::new(format!(
                "{kind} belongs to another evaluation session"
            )));
        }
        EvaluationTaskPoll::Pending(_) => {}
    }
    if context.runs_scheduled_task() {
        return match context.pump_wait(&wait, 256) {
            EvaluationPumpOutcome::TargetReady => match context.poll_wait(&wait) {
                EvaluationTaskPoll::Complete(value) => Ok(value),
                EvaluationTaskPoll::Failed(error) => {
                    Err(deferred_task_failure(context, &wait, error))
                }
                EvaluationTaskPoll::Pending(wait) => Err(EvalError::blocked(CoreWaitToken(wait))),
                EvaluationTaskPoll::Cancelled => {
                    Err(EvalError::new(format!("{kind} evaluation was cancelled")))
                }
                EvaluationTaskPoll::ForeignSession => Err(EvalError::new(format!(
                    "{kind} belongs to another evaluation session"
                ))),
            },
            EvaluationPumpOutcome::NoProgress | EvaluationPumpOutcome::BudgetExhausted => {
                Err(EvalError::blocked(CoreWaitToken(wait)))
            }
        };
    }
    loop {
        match context.pump_wait(&wait, 256) {
            EvaluationPumpOutcome::TargetReady => break,
            EvaluationPumpOutcome::NoProgress => {
                return Err(EvalError::blocked(CoreWaitToken(wait)));
            }
            EvaluationPumpOutcome::BudgetExhausted => {}
        }
    }
    match context.poll_wait(&wait) {
        EvaluationTaskPoll::Complete(value) => Ok(value),
        EvaluationTaskPoll::Failed(error) => Err(deferred_task_failure(context, &wait, error)),
        EvaluationTaskPoll::Pending(wait) => Err(EvalError::blocked(CoreWaitToken(wait))),
        EvaluationTaskPoll::Cancelled => {
            Err(EvalError::new(format!("{kind} evaluation was cancelled")))
        }
        EvaluationTaskPoll::ForeignSession => Err(EvalError::new(format!(
            "{kind} belongs to another evaluation session"
        ))),
    }
}

fn deferred_task_failure(
    context: &EvalContext,
    wait: &crate::evaluation::EvaluationWaitToken,
    message: Arc<str>,
) -> EvalError {
    context
        .lazy_failure_for_wait(wait)
        .map(EvalError::lazy_failure)
        .unwrap_or_else(|| EvalError::new(message.as_ref()))
}

fn produce_lazy_source(context: &EvalContext, lazy: &LazyValue) -> Result<Value, EvalError> {
    match lazy.source() {
        LazySource::Error => Err(EvalError::new(
            "initialized lazy errors must be returned from their result cache",
        )),
        LazySource::ComputedFixpoint(fixpoint) => eval_computed_fixpoint(context, lazy, fixpoint),
        LazySource::Deferred(thunk) => thunk(context).map_err(EvalError::new),
        LazySource::ReflectionGate(gate) => eval_reflection_gate_source(context, gate),
        LazySource::Access { path, arguments } => resolve_core_access(context, arguments, path),
        LazySource::Application(application) => apply_values(
            context,
            application.function().clone(),
            application.arguments().to_vec(),
        ),
        LazySource::Builtin(call) => {
            let mut arguments = call.arguments.iter().cloned().collect::<Vec<_>>();
            let argument = arguments
                .pop()
                .expect("saturated builtin thunk must contain an argument");
            apply_builtin(context, call.builtin, arguments, argument)
        }
        LazySource::NetComputation(net) => {
            let runtime = net.runtime().clone();
            let exposed = runtime.with(|runtime| runtime.exposed());
            extract_net_data(context, runtime, exposed, "lazy net computation")
        }
        LazySource::FunctionCall {
            function,
            arguments,
        } => evaluate_function_call(context, function, arguments),
    }
}

fn eval_promised(context: &EvalContext, promise: &PromisedValue) -> Result<Value, EvalError> {
    if let Some(assignment) = promise.assignment() {
        let value = assignment.map_err(|message| EvalError::new(message.as_ref()))?;
        if !is_deferred(&value) {
            return Ok(value);
        }
        let wait =
            promise_wait(context, promise).map_err(|error| EvalError::new(error.as_ref()))?;
        return await_deferred_task(context, wait, "promised value");
    }
    if let Some(task) = promise.task() {
        if context.observes_as_task(task.owner()) {
            return Err(EvalError::new(format!(
                "reflection promise {} recursively observed itself in task {}",
                promise.id().get(),
                task.owner().get()
            )));
        }
        let wait =
            promise_wait(context, promise).map_err(|error| EvalError::new(error.as_ref()))?;
        return await_deferred_task(context, wait, "promised value");
    }
    Err(EvalError {
        kind: EvalErrorKind::UnassignedPromise(promise.clone()),
    })
}

fn eval_reflection_gate_source(
    context: &EvalContext,
    gate: &crate::core::ReflectionGate,
) -> Result<Value, EvalError> {
    let task = gate
        .task(context)
        .map_err(|error| EvalError::new(error.as_ref()))?;
    match context.poll_reflection_task(task) {
        EvaluationTaskPoll::Pending(wait) => Err(EvalError::blocked(CoreWaitToken(wait))),
        EvaluationTaskPoll::Complete(_) => Ok(gate.target().clone()),
        EvaluationTaskPoll::Failed(error) => Err(EvalError::new(error.as_ref())),
        EvaluationTaskPoll::Cancelled => {
            Err(EvalError::new("reflection annotation task was cancelled"))
        }
        EvaluationTaskPoll::ForeignSession => Err(EvalError::new(
            "reflection annotation task belongs to another evaluation session",
        )),
    }
}

fn eval_computed_fixpoint(
    context: &EvalContext,
    lazy: &LazyValue,
    computation: &FixpointComputation,
) -> Result<Value, EvalError> {
    let marker = Value::Lazy(lazy.clone());
    match computation {
        FixpointComputation::Function(function) => apply_value(context, function.clone(), marker)
            .and_then(|application| eval_value(context, &application)),
        FixpointComputation::ObjectInstance(spec) => {
            construct_fixpoint_object(context, spec, marker)
        }
    }
}

pub(super) fn format_name_part(key: &Key) -> String {
    match key {
        Key::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Key::AbstractGlobalPath(parts) => parts.join("."),
        Key::Atom(atom) => match atom.key() {
            Key::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            Key::AbstractGlobalPath(parts) => parts.join("."),
            other => format!("{other:?}"),
        },
        other => format!("{other:?}"),
    }
}

pub(super) fn value_to_key(context: &EvalContext, value: &Value) -> Result<Key, EvalError> {
    let value = eval_value(context, value)?;
    match &value {
        Value::Atom(atom) => Ok(Key::Atom(*atom)),
        Value::Number(number) => Ok(Key::Number(number.clone())),
        Value::Binary(bytes) => Ok(Key::Binary(bytes.clone())),
        Value::List(list) => Ok(Key::List(list_to_key_items(context, list)?)),
        Value::Dict(dict) => Ok(Key::Dict(Arc::from(
            dict.iter()
                .map(|(key, value)| {
                    let value = value_to_key(context, value)?;
                    if matches!(&value, Key::Dict(entries) if entries.is_empty()) {
                        return Ok(None);
                    }
                    Ok(Some((key.clone(), value)))
                })
                .collect::<Result<Vec<_>, EvalError>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>(),
        ))),
        Value::Builtin(_)
        | Value::PartialBuiltin(_)
        | Value::Function(_)
        | Value::Net(_)
        | Value::Lazy(_)
        | Value::Promised(_)
        | Value::Opaque(_) => Err(EvalError::new(
            "dictionary keys must evaluate to keyable values",
        )),
    }
}

pub(super) fn force_list_thunk(
    context: &EvalContext,
    thunk: &ListThunk,
) -> Result<List, EvalError> {
    let thunk = match thunk {
        ListThunk::Lazy(lazy) => Value::Lazy(lazy.clone()),
        ListThunk::Promised(promise) => Value::Promised(promise.clone()),
    };
    match eval_value(context, &thunk)? {
        Value::Binary(bytes) => Ok(List::from_bytes(bytes)),
        Value::List(list) => Ok(list),
        other => Err(EvalError::new(format!(
            "lazy list chunk must evaluate to a list or binary value, got {other:?}"
        ))),
    }
}

pub(super) fn pop_list_front(
    context: &EvalContext,
    list: &List,
) -> Result<Option<(Value, List)>, EvalError> {
    Ok(list
        .try_pop_front(&mut |thunk| force_list_thunk(context, thunk))?
        .map(|(item, tail)| {
            let value = match item {
                ListItem::Byte(byte) => Value::Number(Number::from_u8(byte)),
                ListItem::Value(value) => value,
            };
            (value, tail)
        }))
}

pub(super) fn split_result_value(left: Value, right: Value) -> Value {
    Value::Dict(
        crate::core::Dict::new_sync()
            .insert((*keys::LEFT).clone(), left)
            .insert((*keys::RIGHT).clone(), right),
    )
}

pub(super) fn eval_number(
    context: &EvalContext,
    value: &Value,
    builtin_name: &str,
) -> Result<Number, EvalError> {
    let value = eval_value(context, value)?;
    let Value::Number(number) = value else {
        return Err(EvalError::new(format!(
            "{builtin_name} builtin requires number values"
        )));
    };
    Ok(number)
}

pub(super) fn eval_index_number(
    context: &EvalContext,
    value: &Value,
    builtin_name: &str,
) -> Result<usize, EvalError> {
    let number = eval_number(context, value, builtin_name)?;
    number.to_usize_if_integer().ok_or_else(|| {
        EvalError::new(format!(
            "{builtin_name} builtin requires non-negative integer indices"
        ))
    })
}

pub(super) fn is_deferred_value(value: &Value) -> bool {
    matches!(value, Value::Lazy(_) | Value::Promised(_))
}

pub(super) fn is_error_lazy_value(value: &Value) -> bool {
    matches!(value, Value::Lazy(lazy) if lazy.cached().is_some_and(|result| result.is_err()))
}

pub(super) fn is_undefined_dict_value(value: &Value) -> bool {
    is_undefined_value(value)
}

/// Evaluator semantics for extracting the payload of a singleton tagged value.
///
/// Other dictionary entries are ignored only when their values recursively
/// evaluate to undefined dictionaries. The tagged payload must itself be
/// semantically defined.
pub(super) trait TaggedDictExt {
    fn tagged_payload(&self, context: &EvalContext, tag: &Key) -> Result<Option<Value>, EvalError>;
}

impl TaggedDictExt for crate::core::Dict {
    fn tagged_payload(&self, context: &EvalContext, tag: &Key) -> Result<Option<Value>, EvalError> {
        let Some(payload) = self.get(tag) else {
            return Ok(None);
        };
        if is_semantically_undefined(context, payload)? {
            return Ok(None);
        }

        for (key, value) in self.iter() {
            if key != tag && !is_semantically_undefined(context, value)? {
                return Ok(None);
            }
        }
        Ok(Some(payload.clone()))
    }
}

fn is_semantically_undefined(context: &EvalContext, value: &Value) -> Result<bool, EvalError> {
    let value = eval_value(context, value)?;
    let Value::Dict(dict) = value else {
        return Ok(false);
    };
    for (_, value) in dict.iter() {
        if !is_semantically_undefined(context, value)? {
            return Ok(false);
        }
    }
    Ok(true)
}
