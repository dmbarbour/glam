//! Core value evaluation and interaction-net integration.

use std::sync::Arc;

#[cfg(test)]
use crate::core::CoreValueFactory;
#[cfg(test)]
use crate::core::PromisedValue;
use crate::core::{
    Builtin, BuiltinCall, EvaluationHalt, FunctionCode, FunctionValue, Key, LazyValue, List,
    ListEffectComputation, NetValue, Value, keys,
};
use crate::core_net::{CoreDataKey, CoreOperator, CoreSpecialization};
use crate::evaluation::{EvalContext, EvaluatorStepContext};
#[cfg(test)]
use crate::interaction_net::Reduction;
use crate::interaction_net::{
    ActivePairKey, Call, NetBuilder, NetSpecialization, OperatorCall, OperatorYield, Port,
    ReductionKind, StuckReason,
};
use crate::number::Number;
#[cfg(test)]
use crate::{evaluation::OwnedEvalContext, list::ListItem};

#[cfg(test)]
mod access_inventory;
mod access_machine;
mod annotation_machine;
mod application;
mod builtin_machine;
mod builtins;
mod comparison_machine;
mod dict_machine;
mod effect_machine;
mod list_effect_machine;
mod list_machine;
mod list_observation_machine;
mod list_transform_machine;
mod net;
mod object_machine;
mod operator;
mod pattern_machine;
mod sequence;
pub(crate) mod strategy_machine;
mod tagged_machine;
#[cfg(test)]
mod test_support;
mod value;
pub(crate) mod whnf;
#[cfg(test)]
mod whnf_inventory;

#[cfg(test)]
pub(crate) use application::apply_values;
#[cfg(test)]
pub(crate) use builtins::assert_construction_port_family_shape;
#[cfg(test)]
pub(crate) use operator::constant_effect;
pub(crate) use operator::{
    access_operator, apply_arity_operator, computation_capture_operator, constant_effect_in,
    function_capture_operator, list_operator, request_operator,
};
#[cfg(test)]
pub(crate) use sequence::list_output_bytes;
#[cfg(test)]
pub(crate) use sequence::list_to_value_items;
#[allow(
    unused_imports,
    reason = "W8 retains the direct compatibility evaluator for tests until the family migration closes"
)]
pub use value::eval_value;
pub(crate) use value::failure_diagnostic_value_in;
pub(crate) use value::{eval_value_in, lazy_root_wait};
#[cfg(test)]
pub(crate) use value::{
    evaluation_context_frame, evaluation_context_frame_with_args, failure_diagnostic_value,
    halt_diagnostic_value,
};

pub(crate) use access_machine::{ConversionPoll, KeyConversionMachine, KeyListMachine};
pub(crate) use application::*;
#[cfg(test)]
use builtins::apply_builtin;
use builtins::apply_builtin_in;
pub(crate) use list_machine::{ListFrontMachine, ListFrontPoll};
use net::*;
use operator::*;
pub(crate) use sequence::*;
#[cfg(test)]
use test_support::*;
pub(crate) use value::promise_root_wait;
use value::*;

fn with_direct_evaluator<R>(
    context: &EvalContext,
    operation: impl FnOnce(&EvaluatorStepContext<'_>) -> R,
) -> R {
    let evaluator = EvaluatorStepContext::for_direct_compatibility(context);
    let result = operation(&evaluator);
    evaluator.finish();
    result
}

#[cfg(test)]
mod tests;
