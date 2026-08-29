use std::sync::Arc;

use crate::core::{
    Dict, EvaluatedValue, EvaluationFailure, EvaluationHalt, FixpointComputation, Key, LazySource,
    LazyValue, List, ListThunk, PromisedValue, Value, keys,
};
use crate::core_net::CoreWaitToken;
use crate::evaluation::{
    EvalContext, EvaluationMachinePoll, EvaluationPumpOutcome, EvaluationTaskBlock,
    EvaluationTaskMachine, EvaluationWaitPoll, EvaluatorStepContext, WorkDependency,
};
use crate::list::ListItem;
use crate::number::Number;

use super::application::{apply_value_in, apply_values_in};
use super::builtins::{
    NetConstructionMachine, apply_builtin, construct_fixpoint_object, is_undefined_value,
};
use super::net::*;
use super::sequence::list_to_key_items_in;

pub(crate) fn failure_diagnostic_value(failure: &EvaluationFailure) -> Value {
    let emission = match failure.emission_value() {
        Some(Value::Binary(text)) => {
            crate::diagnostic::text_message(None, String::from_utf8_lossy(text))
        }
        Some(Value::Dict(_)) => failure
            .emission_value()
            .expect("matched failure emission")
            .clone(),
        Some(other) => {
            return fallback_failure_diagnostic(
                failure,
                Some(other.clone()),
                Value::List(List::from_values(failure.contexts().to_vec())),
            );
        }
        None => crate::diagnostic::text_message(None, failure.to_string()),
    };

    crate::diagnostic::prepend_contexts(emission.clone(), failure.contexts()).unwrap_or_else(|_| {
        fallback_failure_diagnostic(
            failure,
            Some(emission),
            Value::List(List::from_values(failure.contexts().to_vec())),
        )
    })
}

pub(crate) fn failure_diagnostic_value_with(
    values: &crate::core::CoreValueFactory,
    failure: &EvaluationFailure,
) -> Value {
    let emission = match failure.emission_value() {
        Some(Value::Binary(text)) => {
            crate::diagnostic::text_message(None, String::from_utf8_lossy(text))
        }
        Some(emission) => emission.clone(),
        None => crate::diagnostic::text_message(None, failure.to_string()),
    };

    crate::diagnostic::prepend_contexts_with(values, emission.clone(), failure.contexts())
        .unwrap_or_else(|_| {
            fallback_failure_diagnostic(
                failure,
                Some(emission),
                Value::List(List::from_values(failure.contexts().to_vec())),
            )
        })
}

#[cfg(test)]
pub(crate) fn halt_diagnostic_value(halt: &EvaluationHalt) -> Option<Value> {
    halt.permanent_failure()
        .map(|failure| failure_diagnostic_value(failure))
}

pub(crate) fn halt_diagnostic_value_with(
    values: &crate::core::CoreValueFactory,
    halt: &EvaluationHalt,
) -> Option<Value> {
    halt.permanent_failure()
        .map(|failure| failure_diagnostic_value_with(values, failure))
}

pub(crate) fn evaluation_context_frame(operation: &str) -> Value {
    evaluation_context_frame_with_args(operation, Dict::new_sync())
}

pub(crate) fn evaluation_context_frame_with_args(operation: &str, args: Dict) -> Value {
    let operation = Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
        operation,
    )));
    let mut detail = Dict::new_sync().insert((*keys::OP).clone(), operation);
    if !args.is_empty() {
        detail = detail.insert((*keys::ARGS).clone(), Value::Dict(args));
    }
    Value::Dict(Dict::new_sync().insert((*keys::EVAL).clone(), Value::Dict(detail)))
}

fn fallback_failure_diagnostic(
    failure: &EvaluationFailure,
    emission: Option<Value>,
    contexts: Value,
) -> Value {
    let mut message = Dict::new_sync()
        .insert(
            (*keys::TEXT).clone(),
            Value::binary_from_text(&failure.to_string()),
        )
        .insert((*keys::CONTEXT).clone(), contexts);
    if let Some(emission) = emission {
        message = message.insert((*keys::VALUE).clone(), emission);
    }
    Value::Dict(Dict::new_sync().insert((*keys::MSG).clone(), Value::Dict(message)))
}

pub fn eval_value(context: &EvalContext, value: &Value) -> Result<Value, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| eval_value_in(evaluator, value))
}

pub(crate) fn eval_value_in(
    context: &EvaluatorStepContext<'_>,
    value: &Value,
) -> Result<Value, EvaluationHalt> {
    match value {
        Value::Lazy(lazy) => eval_lazy_in(context, lazy),
        Value::Promised(promise) => eval_promised_in(context, promise),
        other => Ok(other.clone()),
    }
}

enum LazyTaskWork {
    Produce,
    Follow(Value),
    NetConstruction(Box<NetConstructionMachine>),
}

struct LazyTaskMachine {
    context: EvalContext,
    lazy: LazyValue,
    work: LazyTaskWork,
}

impl LazyTaskMachine {
    fn complete(&self, context: &EvaluatorStepContext<'_>, value: Value) -> EvaluationMachinePoll {
        let value = EvaluatedValue::try_from(value)
            .expect("WHNF demand must eliminate the outer deferred variant");
        match self.lazy.cache(Ok(value)) {
            Ok(value) => EvaluationMachinePoll::Complete(context.root_value(value.into_value())),
            Err(error) => EvaluationMachinePoll::Failed(error),
        }
    }

    fn cached_poll(&self, context: &EvaluatorStepContext<'_>) -> EvaluationMachinePoll {
        match self
            .lazy
            .cached()
            .expect("a released lazy source must have a terminal cache")
        {
            Ok(value) => EvaluationMachinePoll::Complete(context.root_value(value.into_value())),
            Err(error) => EvaluationMachinePoll::Failed(error),
        }
    }

    fn finish_poll(
        &mut self,
        context: &EvaluatorStepContext<'_>,
        result: Result<Value, EvaluationHalt>,
    ) -> EvaluationMachinePoll {
        match result {
            Ok(value) if is_deferred(&value) => {
                self.work = LazyTaskWork::Follow(value);
                EvaluationMachinePoll::Yielded
            }
            Ok(value) => self.complete(context, value),
            Err(error) => self.fail(context, error),
        }
    }
}

impl EvaluationTaskMachine for LazyTaskMachine {
    fn poll(
        &mut self,
        poll_context: &crate::evaluation::EvaluationPollContext,
        step_budget: usize,
    ) -> EvaluationMachinePoll {
        let durable_context = self.context.clone();
        let context = poll_context.evaluator(&durable_context);
        if let Some(result) = self.lazy.cached() {
            return match result {
                Ok(value) => {
                    EvaluationMachinePoll::Complete(context.root_value(value.into_value()))
                }
                Err(error) => EvaluationMachinePoll::Failed(error),
            };
        }

        if matches!(self.work, LazyTaskWork::Produce) {
            let Some(source) = self.lazy.source_snapshot() else {
                return self.cached_poll(&context);
            };
            if let LazySource::NetConstruction(effect) = source {
                let machine = match NetConstructionMachine::new(
                    durable_context.clone(),
                    effect.as_ref().clone(),
                ) {
                    Ok(machine) => machine,
                    Err(error) => return self.fail(&context, error),
                };
                self.work = LazyTaskWork::NetConstruction(Box::new(machine));
                return EvaluationMachinePoll::Yielded;
            }
            let result = produce_lazy_source_in(&context, &self.lazy, &source);
            return self.finish_poll(&context, result);
        }

        if let LazyTaskWork::NetConstruction(machine) = &mut self.work {
            return match machine.poll(context.context(), step_budget) {
                Ok(Some(value)) => self.complete(&context, value),
                Ok(None) => EvaluationMachinePoll::Yielded,
                Err(error) => self.fail(&context, error),
            };
        }

        let LazyTaskWork::Follow(target) = &self.work else {
            unreachable!("non-producing lazy work must follow a value or construct a net")
        };
        let result = eval_value_in(&context, target);
        self.finish_poll(&context, result)
    }
}

impl LazyTaskMachine {
    fn fail(
        &self,
        context: &EvaluatorStepContext<'_>,
        error: EvaluationHalt,
    ) -> EvaluationMachinePoll {
        if let Some(wait) = error.blocked_on() {
            return EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                dependency: Some(WorkDependency::Wait(wait.0)),
                observed_epoch: None,
                error: None,
            });
        }
        if let Some(promise) = error.unassigned_promise() {
            let wait = match promise_wait(context.context(), promise) {
                Ok(wait) => wait,
                Err(error) => {
                    return EvaluationMachinePoll::Failed(Arc::new(EvaluationFailure::message(
                        error.as_ref(),
                    )));
                }
            };
            return EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                dependency: Some(WorkDependency::Wait(wait)),
                observed_epoch: None,
                error: None,
            });
        }
        let failure = error.into_permanent_failure();
        match self.lazy.cache(Err(failure)) {
            Ok(value) => EvaluationMachinePoll::Complete(context.root_value(value.into_value())),
            Err(error) => EvaluationMachinePoll::Failed(error),
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
    fn poll(
        &mut self,
        poll_context: &crate::evaluation::EvaluationPollContext,
        _step_budget: usize,
    ) -> EvaluationMachinePoll {
        let durable_context = self.context.clone();
        let context = poll_context.evaluator(&durable_context);
        let result = match &self.state {
            PromiseFollowerState::AwaitAssignment => match self.promise.assignment() {
                Some(result) => result.map_err(EvaluationHalt::failure),
                None => {
                    return EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                        dependency: Some(WorkDependency::Promise(self.promise.clone())),
                        observed_epoch: None,
                        error: None,
                    });
                }
            },
            PromiseFollowerState::FollowAssignment(target) => eval_value_in(&context, target),
        };

        match result {
            Ok(value) if is_deferred(&value) => {
                self.state = PromiseFollowerState::FollowAssignment(value);
                EvaluationMachinePoll::Yielded
            }
            Ok(value) => EvaluationMachinePoll::Complete(context.root_value(value)),
            Err(error) => block_or_fail(context.context(), error),
        }
    }
}

fn is_deferred(value: &Value) -> bool {
    matches!(value, Value::Lazy(_) | Value::Promised(_))
}

pub(super) fn promise_wait(
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

fn block_or_fail(context: &EvalContext, error: EvaluationHalt) -> EvaluationMachinePoll {
    if let Some(wait) = error.blocked_on() {
        return EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
            dependency: Some(WorkDependency::Wait(wait.0)),
            observed_epoch: None,
            error: None,
        });
    }
    if let Some(promise) = error.unassigned_promise() {
        return match promise_wait(context, promise) {
            Ok(wait) => EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                dependency: Some(WorkDependency::Wait(wait)),
                observed_epoch: None,
                error: None,
            }),
            Err(error) => {
                EvaluationMachinePoll::Failed(Arc::new(EvaluationFailure::message(error.as_ref())))
            }
        };
    }
    EvaluationMachinePoll::Failed(error.into_permanent_failure())
}

#[cfg(test)]
pub(super) fn eval_lazy(context: &EvalContext, lazy: &LazyValue) -> Result<Value, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| eval_lazy_in(evaluator, lazy))
}

pub(super) fn eval_lazy_in(
    context: &EvaluatorStepContext<'_>,
    lazy: &LazyValue,
) -> Result<Value, EvaluationHalt> {
    loop {
        if let Some(result) = lazy.cached() {
            return result
                .map(EvaluatedValue::into_value)
                .map_err(EvaluationHalt::failure);
        }
        let wait = context
            .context()
            .lazy_task(lazy, |task_context| {
                Box::new(LazyTaskMachine {
                    context: task_context,
                    lazy: lazy.clone(),
                    work: LazyTaskWork::Produce,
                })
            })
            .map_err(|error| EvaluationHalt::new(error.as_ref()))?;
        if let Some(value) = await_deferred_task(context.context(), wait, "lazy value")? {
            return Ok(value);
        }
    }
}

fn await_deferred_task(
    context: &EvalContext,
    wait: crate::evaluation::EvaluationWaitToken,
    kind: &str,
) -> Result<Option<Value>, EvaluationHalt> {
    match context.poll_wait(&wait) {
        EvaluationWaitPoll::Complete(value) => return Ok(Some(value)),
        EvaluationWaitPoll::Failed(error) => {
            return Err(deferred_task_failure(context, &wait, error));
        }
        EvaluationWaitPoll::Cancelled => {
            return Err(EvaluationHalt::new(format!(
                "{kind} evaluation was cancelled"
            )));
        }
        EvaluationWaitPoll::Abandoned => return Ok(None),
        EvaluationWaitPoll::Exited => {
            return Err(EvaluationHalt::new(format!(
                "{kind} producer exited without a result"
            )));
        }
        EvaluationWaitPoll::Killed(error) => return Err(EvaluationHalt::failure(error)),
        EvaluationWaitPoll::Pending(_) => {}
    }
    if context.runs_scheduled_task() {
        return match context.pump_wait(&wait, 256) {
            EvaluationPumpOutcome::TargetReady => match context.poll_wait(&wait) {
                EvaluationWaitPoll::Complete(value) => Ok(Some(value)),
                EvaluationWaitPoll::Failed(error) => {
                    Err(deferred_task_failure(context, &wait, error))
                }
                EvaluationWaitPoll::Pending(wait) => {
                    Err(EvaluationHalt::blocked(CoreWaitToken(wait)))
                }
                EvaluationWaitPoll::Cancelled => Err(EvaluationHalt::new(format!(
                    "{kind} evaluation was cancelled"
                ))),
                EvaluationWaitPoll::Abandoned => Ok(None),
                EvaluationWaitPoll::Exited => Err(EvaluationHalt::new(format!(
                    "{kind} producer exited without a result"
                ))),
                EvaluationWaitPoll::Killed(error) => Err(EvaluationHalt::failure(error)),
            },
            EvaluationPumpOutcome::Busy
            | EvaluationPumpOutcome::NoProgress
            | EvaluationPumpOutcome::BudgetExhausted => {
                Err(EvaluationHalt::blocked(CoreWaitToken(wait)))
            }
        };
    }
    loop {
        match context.pump_wait(&wait, 256) {
            EvaluationPumpOutcome::TargetReady => break,
            EvaluationPumpOutcome::Busy if context.waits_for_claimed_tasks() => {
                context.wait_for_claimed_task(&wait);
            }
            EvaluationPumpOutcome::Busy => {
                return Err(EvaluationHalt::blocked(CoreWaitToken(wait)));
            }
            EvaluationPumpOutcome::NoProgress => {
                return Err(EvaluationHalt::blocked(CoreWaitToken(wait)));
            }
            EvaluationPumpOutcome::BudgetExhausted => {}
        }
    }
    match context.poll_wait(&wait) {
        EvaluationWaitPoll::Complete(value) => Ok(Some(value)),
        EvaluationWaitPoll::Failed(error) => Err(deferred_task_failure(context, &wait, error)),
        EvaluationWaitPoll::Pending(wait) => Err(EvaluationHalt::blocked(CoreWaitToken(wait))),
        EvaluationWaitPoll::Cancelled => Err(EvaluationHalt::new(format!(
            "{kind} evaluation was cancelled"
        ))),
        EvaluationWaitPoll::Abandoned => Ok(None),
        EvaluationWaitPoll::Exited => Err(EvaluationHalt::new(format!(
            "{kind} producer exited without a result"
        ))),
        EvaluationWaitPoll::Killed(error) => Err(EvaluationHalt::failure(error)),
    }
}

fn deferred_task_failure(
    context: &EvalContext,
    wait: &crate::evaluation::EvaluationWaitToken,
    failure: Arc<EvaluationFailure>,
) -> EvaluationHalt {
    context
        .lazy_failure_for_wait(wait)
        .map(EvaluationHalt::failure)
        .unwrap_or_else(|| EvaluationHalt::failure(failure))
}

fn produce_lazy_source_in(
    context: &EvaluatorStepContext<'_>,
    lazy: &LazyValue,
    source: &LazySource,
) -> Result<Value, EvaluationHalt> {
    match source {
        LazySource::Error => Err(EvaluationHalt::new(
            "initialized lazy errors must be returned from their result cache",
        )),
        LazySource::ComputedFixpoint(fixpoint) => {
            eval_computed_fixpoint_in(context, lazy, fixpoint)
        }
        LazySource::Deferred(thunk) => thunk(context.context()),
        LazySource::ReflectionTask(task) => eval_reflection_task_source(context.context(), task),
        LazySource::Access { path, arguments } => {
            resolve_core_access(context.context(), arguments, path)
        }
        LazySource::Application(application) => apply_values_in(
            context,
            application.function().clone(),
            application.arguments().to_vec(),
        ),
        LazySource::Builtin(call) => {
            let mut arguments = call.arguments.iter().cloned().collect::<Vec<_>>();
            let argument = arguments
                .pop()
                .expect("saturated builtin thunk must contain an argument");
            apply_builtin(context.context(), call.builtin, arguments, argument)
        }
        LazySource::NetConstruction(_) => {
            unreachable!("net construction must retain its pollable effect machine")
        }
        LazySource::NetComputation(net) => {
            let runtime = net.runtime().clone();
            let exposed = runtime.with(|runtime| runtime.exposed());
            extract_net_data(context.context(), runtime, exposed, "lazy net computation")
                .map_err(|error| error.with_context(evaluation_context_frame("net_computation")))
        }
        LazySource::FunctionCall {
            function,
            arguments,
        } => evaluate_function_call(context.context(), function, arguments),
    }
}

fn eval_promised_in(
    context: &EvaluatorStepContext<'_>,
    promise: &PromisedValue,
) -> Result<Value, EvaluationHalt> {
    loop {
        if let Some(assignment) = promise.assignment() {
            let value = assignment.map_err(EvaluationHalt::failure)?;
            if !is_deferred(&value) {
                return Ok(value);
            }
            let wait = promise_wait(context.context(), promise)
                .map_err(|error| EvaluationHalt::new(error.as_ref()))?;
            if let Some(value) = await_deferred_task(context.context(), wait, "promised value")? {
                return Ok(value);
            }
            continue;
        }
        if let Some(task) = promise.task() {
            if context.context().observes_as_task(task.owner()) {
                return Err(EvaluationHalt::new(format!(
                    "reflection promise {} recursively observed itself in task {}",
                    promise.id().get(),
                    task.owner().get()
                )));
            }
            let wait = promise_wait(context.context(), promise)
                .map_err(|error| EvaluationHalt::new(error.as_ref()))?;
            if let Some(value) = await_deferred_task(context.context(), wait, "promised value")? {
                return Ok(value);
            }
            continue;
        }
        return Err(EvaluationHalt::unassigned(promise.clone()));
    }
}

fn eval_reflection_task_source(
    context: &EvalContext,
    computation: &crate::core::ReflectionComputation,
) -> Result<Value, EvaluationHalt> {
    let (context_name, cancellation_message) = match computation.completion() {
        crate::core::ReflectionCompletion::Gate { .. } => (
            "reflection_annotation",
            "reflection annotation task was cancelled",
        ),
        crate::core::ReflectionCompletion::ReturnValue => {
            ("reflection_task", "reflection result task was cancelled")
        }
    };
    let task = computation.task(context).map_err(|error| {
        EvaluationHalt::failure(Arc::clone(error))
            .with_context(evaluation_context_frame(context_name))
    })?;
    match context.poll_reflection_task(task) {
        EvaluationWaitPoll::Pending(wait) => Err(EvaluationHalt::blocked(CoreWaitToken(wait))),
        EvaluationWaitPoll::Complete(value) => match computation.completion() {
            crate::core::ReflectionCompletion::Gate { target } => Ok(target.clone()),
            crate::core::ReflectionCompletion::ReturnValue => Ok(value),
        },
        EvaluationWaitPoll::Failed(error) => {
            task.acknowledge_propagated_failure();
            Err(EvaluationHalt::failure(error).with_context(evaluation_context_frame(context_name)))
        }
        EvaluationWaitPoll::Cancelled => Err(EvaluationHalt::new(cancellation_message)),
        EvaluationWaitPoll::Abandoned => Err(EvaluationHalt::new(
            "reflection task was abandoned when its evaluation session closed",
        )
        .with_context(evaluation_context_frame(context_name))),
        EvaluationWaitPoll::Exited => Err(EvaluationHalt::new(
            "reflection task exited without producing a result",
        )
        .with_context(evaluation_context_frame(context_name))),
        EvaluationWaitPoll::Killed(error) => {
            Err(EvaluationHalt::failure(error).with_context(evaluation_context_frame(context_name)))
        }
    }
}

fn eval_computed_fixpoint_in(
    context: &EvaluatorStepContext<'_>,
    lazy: &LazyValue,
    computation: &FixpointComputation,
) -> Result<Value, EvaluationHalt> {
    let marker = Value::Lazy(lazy.clone());
    match computation {
        FixpointComputation::Function(function) => {
            apply_value_in(context, function.clone(), marker)
                .and_then(|application| eval_value_in(context, &application))
        }
        FixpointComputation::ObjectInstance(spec) => {
            construct_fixpoint_object(context.context(), spec, marker)
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

pub(super) fn value_to_key(context: &EvalContext, value: &Value) -> Result<Key, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| value_to_key_in(evaluator, value))
}

pub(super) fn value_to_key_in(
    context: &EvaluatorStepContext<'_>,
    value: &Value,
) -> Result<Key, EvaluationHalt> {
    let value = eval_value_in(context, value)?;
    match &value {
        Value::Atom(atom) => Ok(Key::Atom(*atom)),
        Value::Number(number) => Ok(Key::Number(number.clone())),
        Value::Binary(bytes) => Ok(Key::Binary(bytes.clone())),
        Value::List(list) => Ok(Key::List(list_to_key_items_in(context, list)?)),
        Value::Dict(dict) => Ok(Key::Dict(Arc::from(
            dict.iter()
                .map(|(key, value)| {
                    let value = value_to_key_in(context, value)?;
                    if matches!(&value, Key::Dict(entries) if entries.is_empty()) {
                        return Ok(None);
                    }
                    Ok(Some((key.clone(), value)))
                })
                .collect::<Result<Vec<_>, EvaluationHalt>>()?
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
        | Value::Metadata(_)
        | Value::Opaque(_) => Err(EvaluationHalt::new(
            "dictionary keys must evaluate to keyable values",
        )),
    }
}

pub(super) fn force_list_thunk(
    context: &EvalContext,
    thunk: &ListThunk,
) -> Result<List, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| force_list_thunk_in(evaluator, thunk))
}

pub(super) fn force_list_thunk_in(
    context: &EvaluatorStepContext<'_>,
    thunk: &ListThunk,
) -> Result<List, EvaluationHalt> {
    let thunk = match thunk {
        ListThunk::Lazy(lazy) => Value::Lazy(lazy.clone()),
        ListThunk::Promised(promise) => Value::Promised(promise.clone()),
    };
    match eval_value_in(context, &thunk)? {
        Value::Binary(bytes) => Ok(List::from_bytes(bytes)),
        Value::List(list) => Ok(list),
        other => Err(EvaluationHalt::new(format!(
            "lazy list chunk must evaluate to a list or binary value, got {other:?}"
        ))),
    }
}

pub(crate) fn pop_list_front(
    context: &EvalContext,
    list: &List,
) -> Result<Option<(Value, List)>, EvaluationHalt> {
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
) -> Result<Number, EvaluationHalt> {
    let value = eval_value(context, value)?;
    let Value::Number(number) = value else {
        return Err(EvaluationHalt::new(format!(
            "{builtin_name} builtin requires number values"
        )));
    };
    Ok(number)
}

pub(super) fn eval_index_number(
    context: &EvalContext,
    value: &Value,
    builtin_name: &str,
    evaluation_label: &str,
) -> Result<usize, EvaluationHalt> {
    let value = eval_value(context, value)
        .map_err(|error| error.with_context(evaluation_context_frame(evaluation_label)))?;
    let Value::Number(number) = value else {
        return Err(EvaluationHalt::new(format!(
            "{builtin_name} builtin requires number values"
        )));
    };
    number.to_usize_if_integer().ok_or_else(|| {
        EvaluationHalt::new(format!(
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
    fn tagged_payload(
        &self,
        context: &EvalContext,
        tag: &Key,
    ) -> Result<Option<Value>, EvaluationHalt>;
}

impl TaggedDictExt for crate::core::Dict {
    fn tagged_payload(
        &self,
        context: &EvalContext,
        tag: &Key,
    ) -> Result<Option<Value>, EvaluationHalt> {
        super::with_direct_evaluator(context, |evaluator| tagged_payload_in(self, evaluator, tag))
    }
}

pub(super) fn tagged_payload_in(
    dict: &crate::core::Dict,
    context: &EvaluatorStepContext<'_>,
    tag: &Key,
) -> Result<Option<Value>, EvaluationHalt> {
    let Some(payload) = dict.get(tag) else {
        return Ok(None);
    };
    if is_semantically_undefined_in(context, payload)? {
        return Ok(None);
    }

    for (key, value) in dict.iter() {
        if key != tag && !is_semantically_undefined_in(context, value)? {
            return Ok(None);
        }
    }
    Ok(Some(payload.clone()))
}

fn is_semantically_undefined_in(
    context: &EvaluatorStepContext<'_>,
    value: &Value,
) -> Result<bool, EvaluationHalt> {
    let value = eval_value_in(context, value)?;
    let Value::Dict(dict) = value else {
        return Ok(false);
    };
    for (_, value) in dict.iter() {
        if !is_semantically_undefined_in(context, value)? {
            return Ok(false);
        }
    }
    Ok(true)
}
