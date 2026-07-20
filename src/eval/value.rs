use std::fmt;
use std::sync::Arc;

use crate::core::{
    Builtin, ComputedFixpointAction, FixpointComputation, Key, LazyValue, List, Value, keys,
};
use crate::core_net::CoreWaitToken;
use crate::evaluation::{EvalContext, EvaluationPumpOutcome, EvaluationTaskPoll};
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
    Blocked(CoreWaitToken),
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

    pub(crate) fn blocked_on(&self) -> Option<CoreWaitToken> {
        match &self.kind {
            EvalErrorKind::Blocked(wait) => Some(wait.clone()),
            EvalErrorKind::Message(_) => None,
        }
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            EvalErrorKind::Message(message) => f.write_str(message),
            EvalErrorKind::Blocked(wait) => {
                write!(f, "evaluation is blocked on wait token {}", wait.wait_id())
            }
        }
    }
}

impl std::error::Error for EvalError {}

pub fn eval_value(context: &EvalContext, value: &Value) -> Result<Value, EvalError> {
    match value {
        Value::Lazy(lazy) => eval_lazy(context, lazy),
        Value::Net(net) => observe_net(context, net.clone()),
        other => Ok(other.clone()),
    }
}

pub(super) fn eval_lazy(context: &EvalContext, lazy: &LazyValue) -> Result<Value, EvalError> {
    let net_computation = lazy.net_computation();
    let function_call = lazy.function_call();
    let application = lazy.application();
    let builtin = lazy.builtin();
    let reflection_annotation = builtin.is_some_and(|call| call.builtin == Builtin::Anno);
    let continue_through_result = net_computation.is_some()
        || function_call.is_some()
        || application.is_some()
        || builtin.is_some_and(|call| {
            matches!(
                call.builtin,
                Builtin::Fixpoint
                    | Builtin::ObjectInstance
                    | Builtin::ObjectInstanceFromParts
                    | Builtin::ObjectWithDefs
            )
        });
    if let Some(result) = lazy.cached() {
        let result = result.map_err(|message| EvalError::new(message.as_ref()))?;
        return if reflection_annotation {
            eval_reflection_annotation_result(context, result)
        } else if continue_through_result {
            eval_value(context, &result)
        } else {
            Ok(result)
        };
    }
    let _claim = context
        .claim_lazy(lazy.id())
        .map_err(|error| EvalError::new(error.as_ref()))?;
    if let Some(result) = lazy.cached() {
        let result = result.map_err(|message| EvalError::new(message.as_ref()))?;
        return if reflection_annotation {
            eval_reflection_annotation_result(context, result)
        } else if continue_through_result {
            eval_value(context, &result)
        } else {
            Ok(result)
        };
    }
    let result = if let Some(gate) = lazy.reflection_gate() {
        let task = gate
            .task(context)
            .map_err(|error| EvalError::new(error.as_ref()))?;
        let mut poll = context.poll_reflection_task(task);
        if let EvaluationTaskPoll::Pending(wait) = &poll {
            loop {
                match context.pump_wait(wait, 256) {
                    EvaluationPumpOutcome::TargetReady => {
                        poll = context.poll_reflection_task(task);
                        break;
                    }
                    EvaluationPumpOutcome::NoProgress => break,
                    EvaluationPumpOutcome::BudgetExhausted => {}
                }
            }
        }
        match poll {
            EvaluationTaskPoll::Pending(wait) => {
                return Err(EvalError::blocked(CoreWaitToken(wait)));
            }
            EvaluationTaskPoll::Complete(_) => Ok(gate.target().clone()),
            EvaluationTaskPoll::Failed(error) => Err(EvalError::new(error.as_ref())),
            EvaluationTaskPoll::Cancelled => {
                Err(EvalError::new("reflection annotation task was cancelled"))
            }
            EvaluationTaskPoll::ForeignSession => {
                return Err(EvalError::new(
                    "reflection annotation task belongs to another evaluation session",
                ));
            }
        }
    } else if let Some(fixpoint) = lazy.computed_fixpoint_cell() {
        return eval_computed_fixpoint(context, lazy, fixpoint);
    } else if let Some(fixpoint) = lazy.fixpoint_cell() {
        let observer = context
            .task_id()
            .map_err(|error| EvalError::new(error.as_ref()))?;
        if observer == fixpoint.owner() {
            return Err(EvalError::new(format!(
                "reflection fixpoint {} recursively observed itself in task {}",
                fixpoint.id(),
                fixpoint.owner().get()
            )));
        }
        match context.poll_wait(fixpoint.wait()) {
            EvaluationTaskPoll::Pending(wait) => {
                return Err(EvalError::blocked(CoreWaitToken(wait)));
            }
            EvaluationTaskPoll::Complete(_) => lazy
                .cached()
                .expect("completed fixpoint promise must contain a result")
                .map_err(|message| EvalError::new(message.as_ref())),
            EvaluationTaskPoll::Failed(error) => Err(EvalError::new(error.as_ref())),
            EvaluationTaskPoll::Cancelled => {
                Err(EvalError::new("reflection fixpoint producer was cancelled"))
            }
            EvaluationTaskPoll::ForeignSession => Err(EvalError::new(
                "reflection fixpoint belongs to another evaluation session",
            )),
        }
    } else if let Some(result) = lazy.force_deferred(context) {
        result.map_err(|message| EvalError::new(message.as_ref()))
    } else if let Some((path, arguments)) = lazy.access() {
        resolve_core_access(context, arguments, path)
    } else if let Some((function, arguments)) = application {
        apply_values(context, function.clone(), arguments.to_vec())
    } else if let Some(call) = builtin {
        let mut arguments = call.arguments.iter().cloned().collect::<Vec<_>>();
        let argument = arguments
            .pop()
            .expect("saturated builtin thunk must contain an argument");
        apply_builtin(context, call.builtin, arguments, argument)
    } else if let Some((function, arguments)) = function_call.as_ref() {
        evaluate_function_call(context, function, arguments)
    } else if let Some(net) = net_computation.as_ref() {
        let runtime = net.runtime().clone();
        let exposed = runtime.with(|runtime| runtime.exposed());
        extract_net_data(context, runtime, exposed, "lazy net computation")
    } else if lazy.is_promised() {
        // TODO(parallel evaluation): an unfulfilled lazy value currently
        // fails fast. Parallel evaluation needs a thunk-level scheduler,
        // including explicit sparks and suspended continuations, rather
        // than a blocking IVar join that can deadlock on cyclic demand.
        return Err(EvalError::new(
            "promised value was observed before initialization",
        ));
    } else {
        return Err(EvalError::new("lazy value has no producer"));
    };
    if let Err(error) = &result
        && error.blocked_on().is_some()
    {
        return Err(error.clone());
    }
    let result = result.map_err(|error| Arc::<str>::from(error.to_string()));
    let result = lazy
        .cache(result)
        .map_err(|message| EvalError::new(message.as_ref()))?;
    if reflection_annotation {
        eval_reflection_annotation_result(context, result)
    } else if continue_through_result {
        // A net computation itself has exactly one meaning: extract the
        // exposed Data payload and cache it. Once that has happened, the
        // surrounding computation (including an ordinary function-call
        // stage) may perform the next lazy step without re-entering the
        // source runtime.
        eval_value(context, &result)
    } else {
        Ok(result)
    }
}

fn eval_reflection_annotation_result(
    context: &EvalContext,
    result: Value,
) -> Result<Value, EvalError> {
    match &result {
        Value::Lazy(lazy) if lazy.reflection_gate().is_some() => eval_lazy(context, lazy),
        _ => Ok(result),
    }
}

fn eval_computed_fixpoint(
    context: &EvalContext,
    lazy: &LazyValue,
    fixpoint: &crate::core::ComputedFixpointCell,
) -> Result<Value, EvalError> {
    match fixpoint
        .begin(context, lazy.result_cell())
        .map_err(|error| EvalError::new(error.as_ref()))?
    {
        ComputedFixpointAction::Recursive { id, owner } => Err(EvalError::new(format!(
            "fixpoint {id} recursively observed itself in task {}",
            owner.get()
        ))),
        ComputedFixpointAction::Wait(wait) => match context.poll_wait(&wait) {
            EvaluationTaskPoll::Pending(wait) => Err(EvalError::blocked(CoreWaitToken(wait))),
            EvaluationTaskPoll::Complete(_) => eval_lazy(context, lazy),
            EvaluationTaskPoll::Failed(error) => Err(EvalError::new(error.as_ref())),
            EvaluationTaskPoll::Cancelled => Err(EvalError::new("fixpoint producer was cancelled")),
            EvaluationTaskPoll::ForeignSession => Err(EvalError::new(
                "fixpoint belongs to another evaluation session",
            )),
        },
        ComputedFixpointAction::Produce { owner, computation } => {
            let marker = Value::Lazy(lazy.clone());
            let produced = match computation {
                FixpointComputation::Function(function) => apply_value(context, function, marker)
                    .and_then(|application| force_value_shell(context, &application)),
                FixpointComputation::ObjectInstance(spec) => {
                    construct_fixpoint_object(context, &spec, marker)
                }
            };
            if let Err(error) = &produced
                && error.blocked_on().is_some()
            {
                fixpoint.suspend(owner);
                return Err(error.clone());
            }
            let produced = produced.map_err(|error| Arc::<str>::from(error.to_string()));
            let result = lazy
                .cache(produced)
                .map_err(|message| EvalError::new(message.as_ref()));
            fixpoint.complete(context, owner);
            result
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
    let value = force_value_shell(context, value)?;
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
        Value::Builtin(_) | Value::PartialBuiltin(_) | Value::Function(_) | Value::Net(_) => Err(
            EvalError::new("dictionary keys must evaluate to keyable values"),
        ),
        Value::Lazy(_) => Err(EvalError::new(
            "dictionary keys must evaluate to keyable values",
        )),
    }
}

pub(super) fn force_value_shell(context: &EvalContext, value: &Value) -> Result<Value, EvalError> {
    let mut current = eval_value(context, value)?;
    while matches!(current, Value::Lazy(_)) {
        current = eval_value(context, &current)?;
    }
    Ok(current)
}

pub(super) fn force_list_thunk(
    context: &EvalContext,
    thunk: &LazyValue,
) -> Result<List, EvalError> {
    match force_value_shell(context, &Value::Lazy(thunk.clone()))? {
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
    let value = force_value_shell(context, value)?;
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

pub(super) fn is_lazy_value(value: &Value) -> bool {
    matches!(value, Value::Lazy(_))
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
    let value = force_value_shell(context, value)?;
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
