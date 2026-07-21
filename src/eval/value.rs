use std::fmt;
use std::sync::Arc;

use crate::core::{
    Builtin, ComputedFixpointAction, EvaluatedValue, FixpointComputation, Key, LazySource,
    LazyValue, List, Value, keys,
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

#[derive(Clone, Copy)]
enum PostForcePolicy {
    Return,
    ContinueEvaluation,
    RunReflectionGate,
}

enum LazyTaskWork {
    Produce,
    Follow(Value),
}

struct LazyTaskMachine {
    context: EvalContext,
    lazy: LazyValue,
    policy: PostForcePolicy,
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
        if matches!(value, Value::Lazy(_)) {
            // Shallow evaluation may intentionally return a lazy value as
            // data. The session task shares that result without installing a
            // forwarding value in the immutable cache.
            return EvaluationMachinePoll::Complete(value);
        }
        match self.lazy.cache(Ok(value)) {
            Ok(value) => EvaluationMachinePoll::Complete(value),
            Err(error) => EvaluationMachinePoll::Failed(error),
        }
    }
}

impl EvaluationTaskMachine for LazyTaskMachine {
    fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
        if let Some(result) = self.lazy.cached() {
            return match result {
                Ok(value) => EvaluationMachinePoll::Complete(value),
                Err(error) => EvaluationMachinePoll::Failed(error),
            };
        }

        let following = matches!(self.work, LazyTaskWork::Follow(_));
        match self.poll_source() {
            Ok(value) if !following && should_follow_result(self.policy, &value) => {
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
                let error = Arc::<str>::from(error.to_string());
                match self.lazy.cache(Err(error)) {
                    Ok(value) => EvaluationMachinePoll::Complete(value),
                    Err(error) => EvaluationMachinePoll::Failed(error),
                }
            }
        }
    }
}

fn should_follow_result(policy: PostForcePolicy, value: &Value) -> bool {
    match policy {
        PostForcePolicy::Return => false,
        PostForcePolicy::ContinueEvaluation => true,
        PostForcePolicy::RunReflectionGate => matches!(
            value,
            Value::Lazy(lazy) if matches!(lazy.source(), LazySource::ReflectionGate(_))
        ),
    }
}

pub(super) fn eval_lazy(context: &EvalContext, lazy: &LazyValue) -> Result<Value, EvalError> {
    let policy = match lazy.source() {
        LazySource::Promised => return eval_promised(lazy),
        LazySource::Fixpoint(fixpoint) => return eval_task_fixpoint(context, lazy, fixpoint),
        LazySource::ComputedFixpoint(_) | LazySource::Deferred(_) | LazySource::Access { .. } => {
            PostForcePolicy::Return
        }
        LazySource::ReflectionGate(_) => PostForcePolicy::Return,
        LazySource::Application(_)
        | LazySource::NetComputation(_)
        | LazySource::FunctionCall { .. } => PostForcePolicy::ContinueEvaluation,
        LazySource::Builtin(call) => builtin_post_force(call.builtin),
    };
    if let Some(result) = lazy.cached() {
        return finish_lazy_result(context, result, policy);
    }
    if let LazySource::ReflectionGate(gate) = lazy.source() {
        let task = gate
            .task(context)
            .map_err(|error| EvalError::new(error.as_ref()))?;
        if matches!(
            context.poll_reflection_task(task),
            EvaluationTaskPoll::ForeignSession
        ) {
            return Err(EvalError::new(
                "reflection annotation task belongs to another evaluation session",
            ));
        }
    }
    let wait = context
        .lazy_task(lazy, |task_context| {
            Box::new(LazyTaskMachine {
                context: task_context,
                lazy: lazy.clone(),
                policy,
                work: LazyTaskWork::Produce,
            })
        })
        .map_err(|error| EvalError::new(error.as_ref()))?;
    match context.poll_wait(&wait) {
        EvaluationTaskPoll::Complete(value) => return Ok(value),
        EvaluationTaskPoll::Failed(error) => return Err(EvalError::new(error.as_ref())),
        EvaluationTaskPoll::Cancelled => {
            return Err(EvalError::new("lazy evaluation was cancelled"));
        }
        EvaluationTaskPoll::ForeignSession => {
            return Err(EvalError::new(
                "lazy evaluation belongs to another evaluation session",
            ));
        }
        EvaluationTaskPoll::Pending(_) => {}
    }
    if context.runs_scheduled_task() {
        return match context.pump_wait(&wait, 256) {
            EvaluationPumpOutcome::TargetReady => match context.poll_wait(&wait) {
                EvaluationTaskPoll::Complete(value) => Ok(value),
                EvaluationTaskPoll::Failed(error) => Err(EvalError::new(error.as_ref())),
                EvaluationTaskPoll::Pending(wait) => Err(EvalError::blocked(CoreWaitToken(wait))),
                EvaluationTaskPoll::Cancelled => {
                    Err(EvalError::new("lazy evaluation was cancelled"))
                }
                EvaluationTaskPoll::ForeignSession => Err(EvalError::new(
                    "lazy evaluation belongs to another evaluation session",
                )),
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
        EvaluationTaskPoll::Failed(error) => Err(EvalError::new(error.as_ref())),
        EvaluationTaskPoll::Pending(wait) => Err(EvalError::blocked(CoreWaitToken(wait))),
        EvaluationTaskPoll::Cancelled => Err(EvalError::new("lazy evaluation was cancelled")),
        EvaluationTaskPoll::ForeignSession => Err(EvalError::new(
            "lazy evaluation belongs to another evaluation session",
        )),
    }
}

fn produce_lazy_source(context: &EvalContext, lazy: &LazyValue) -> Result<Value, EvalError> {
    match lazy.source() {
        LazySource::Promised | LazySource::Fixpoint(_) => {
            unreachable!("promised values do not use computed lazy tasks")
        }
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

fn finish_lazy_result(
    context: &EvalContext,
    result: Result<Value, Arc<str>>,
    policy: PostForcePolicy,
) -> Result<Value, EvalError> {
    let result = result.map_err(|message| EvalError::new(message.as_ref()))?;
    match policy {
        PostForcePolicy::Return => Ok(result),
        PostForcePolicy::ContinueEvaluation => {
            // Cache a net's exposed Data payload before continuing. If the
            // payload blocks or re-enters evaluation, source-net work remains
            // shared instead of being repeated.
            eval_value(context, &result)
        }
        PostForcePolicy::RunReflectionGate => match &result {
            Value::Lazy(lazy) if matches!(lazy.source(), LazySource::ReflectionGate(_)) => {
                eval_lazy(context, lazy)
            }
            _ => Ok(result),
        },
    }
}

fn builtin_post_force(builtin: Builtin) -> PostForcePolicy {
    match builtin {
        Builtin::Anno => PostForcePolicy::RunReflectionGate,
        Builtin::Fixpoint
        | Builtin::ObjectInstanceFromParts
        | Builtin::ObjectInstance
        | Builtin::ObjectWithDefs => PostForcePolicy::ContinueEvaluation,
        Builtin::Append
        | Builtin::Add
        | Builtin::Subtract
        | Builtin::Multiply
        | Builtin::Divide
        | Builtin::Greater
        | Builtin::GreaterEqual
        | Builtin::Equal
        | Builtin::NotEqual
        | Builtin::LessEqual
        | Builtin::Less
        | Builtin::Seq
        | Builtin::Spark
        | Builtin::MergeDuplicate
        | Builtin::Floor
        | Builtin::Mod
        | Builtin::Slice
        | Builtin::Map
        | Builtin::ListConcat
        | Builtin::ListLen
        | Builtin::ListSplit
        | Builtin::ListSplitEnd
        | Builtin::ListHead
        | Builtin::ListTail
        | Builtin::TextLines
        | Builtin::ListEffect
        | Builtin::ListEffectReturn
        | Builtin::ListEffectSeq
        | Builtin::ListEffectAlt
        | Builtin::ListEffectCut
        | Builtin::ListEffectFix
        | Builtin::DictSingleton
        | Builtin::DictUnion
        | Builtin::DictUpdate
        | Builtin::ObjectSpec
        | Builtin::ObjectLocalName
        | Builtin::EffectApply
        | Builtin::EffectCall
        | Builtin::EffectMap
        | Builtin::EffectMapRun
        | Builtin::EffectMapContinue
        | Builtin::ObjectDefaultDefs
        | Builtin::ObjectDictDefs
        | Builtin::ObjectComposedDefs
        | Builtin::ObjectOverrideDefs => PostForcePolicy::Return,
    }
}

fn eval_promised(lazy: &LazyValue) -> Result<Value, EvalError> {
    match lazy.cached() {
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(EvalError::new(error.as_ref())),
        None => Err(EvalError::new(
            "promised value was observed before initialization",
        )),
    }
}

fn eval_task_fixpoint(
    context: &EvalContext,
    lazy: &LazyValue,
    fixpoint: &crate::core::FixpointCell,
) -> Result<Value, EvalError> {
    if let Some(result) = lazy.cached() {
        return result.map_err(|message| EvalError::new(message.as_ref()));
    }
    if context.observes_as_task(fixpoint.owner()) {
        return Err(EvalError::new(format!(
            "reflection fixpoint {} recursively observed itself in task {}",
            fixpoint.id(),
            fixpoint.owner().get()
        )));
    }
    match context.poll_wait(fixpoint.wait()) {
        EvaluationTaskPoll::Pending(wait) => Err(EvalError::blocked(CoreWaitToken(wait))),
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
        Value::Builtin(_)
        | Value::PartialBuiltin(_)
        | Value::Function(_)
        | Value::Net(_)
        | Value::Lazy(_)
        | Value::Opaque(_) => Err(EvalError::new(
            "dictionary keys must evaluate to keyable values",
        )),
    }
}

pub(super) fn force_value_shell(context: &EvalContext, value: &Value) -> Result<Value, EvalError> {
    let mut current = eval_value(context, value)?;
    while matches!(current, Value::Lazy(_)) {
        current = eval_value(context, &current)?;
    }
    Ok(EvaluatedValue::try_from(current)
        .expect("forcing a value shell must eliminate the outer lazy variant")
        .into_value())
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
