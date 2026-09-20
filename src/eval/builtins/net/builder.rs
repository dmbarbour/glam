//! Pure state-over-list composition for interaction-net construction.
//!
//! These hidden builtins construct ordinary semantic applications and lazy
//! lists. Ordered search, suspension, and cut remain owned by the canonical
//! list-effect reducer.

use std::sync::Arc;

use crate::core::{Builtin, BuiltinCall, EvaluationHalt, LazyValue, List, ListEffectComputation};
use crate::core::{RuntimeValueAccess, Value};

pub(in crate::eval) fn apply_builder_builtin_in(
    access: &RuntimeValueAccess<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    match builtin {
        Builtin::InteractionNetBuilderReturn => {
            let [value, state] = exact(arguments, "builder return")?;
            Ok(Value::List(List::from_values(vec![outcome(value, state)])))
        }
        Builtin::InteractionNetBuilderSeq => {
            let [operation, continuation, state] = exact(arguments, "builder seq")?;
            let results = application_list(access, operation, [state]);
            let continuation = Value::PartialBuiltin(BuiltinCall {
                builtin: Builtin::InteractionNetBuilderContinue,
                arguments: Arc::from([continuation]),
            });
            Ok(deferred_results(
                access,
                "builder seq",
                ListEffectComputation::FlatMapResults {
                    results,
                    continuation,
                },
            ))
        }
        Builtin::InteractionNetBuilderContinue => {
            let [continuation, outcome] = exact(arguments, "builder continuation")?;
            let [value, state] = decode_outcome(access, &outcome)?;
            Ok(Value::List(application_list(
                access,
                continuation,
                [value, state],
            )))
        }
        Builtin::InteractionNetBuilderAlt => {
            let [left, right, state] = exact(arguments, "builder alt")?;
            let left = application_list(access, left, [access.duplicate_value(&state)]);
            let right = application_list(access, right, [state]);
            Ok(Value::List(List::concat(left, right)))
        }
        Builtin::InteractionNetBuilderFail => {
            let [_state] = exact(arguments, "builder fail")?;
            Ok(Value::List(List::empty()))
        }
        Builtin::InteractionNetBuilderCut => {
            let [operation, state] = exact(arguments, "builder cut")?;
            let results = application_list(access, operation, [state]);
            Ok(deferred_results(
                access,
                "builder cut",
                ListEffectComputation::FirstResult { results },
            ))
        }
        _ => unreachable!("builder composition received another builtin"),
    }
}

pub(super) fn outcome(value: Value, state: Value) -> Value {
    Value::List(List::from_values(vec![value, state]))
}

pub(super) fn decode_outcome(
    access: &RuntimeValueAccess<'_>,
    outcome: &Value,
) -> Result<[Value; 2], EvaluationHalt> {
    super::netlist::strict_record(access, outcome, "builder outcome")?
        .try_into()
        .map_err(|_| EvaluationHalt::new("builder outcome has the wrong number of fields"))
}

fn deferred_results(
    access: &RuntimeValueAccess<'_>,
    label: &'static str,
    computation: ListEffectComputation,
) -> Value {
    Value::List(List::from_thunk(
        LazyValue::list_effect_computation_in(access, label, computation).into(),
    ))
}

fn application_list(
    access: &RuntimeValueAccess<'_>,
    function: Value,
    arguments: impl Into<Arc<[Value]>>,
) -> List {
    List::from_thunk(LazyValue::from_application_in(access, function, arguments.into()).into())
}

fn exact<const N: usize>(
    arguments: Vec<Value>,
    operation: &str,
) -> Result<[Value; N], EvaluationHalt> {
    arguments.try_into().map_err(|_| {
        EvaluationHalt::new(format!(
            "interaction-net {operation} received the wrong number of arguments"
        ))
    })
}
