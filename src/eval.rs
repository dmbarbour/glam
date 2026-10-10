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
#[cfg(test)]
use crate::evaluation::OwnedEvalContext;
use crate::evaluation::{EvalContext, EvaluatorStepContext};
#[cfg(test)]
use crate::interaction_net::Reduction;
use crate::interaction_net::{
    ActivePairKey, Call, NetBuilder, NetSpecialization, OperatorCall, OperatorYield, Port,
    ReductionKind, StuckReason,
};
#[cfg(test)]
use crate::number::Number;

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
pub(crate) mod lazy_checkpoint;
#[cfg(test)]
mod lazy_producer_inventory;
mod list_effect_machine;
mod list_machine;
mod list_observation_machine;
mod list_transform_machine;
mod net;
mod object_builtin_machine;
mod object_composition_machine;
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
    LIST_OPERATOR_MAX_ARITY, access_operator, apply_arity_operator, builtin_operator,
    computation_capture_operator, constant_effect_in, function_capture_operator, list_operator,
    request_operator,
};
#[cfg(test)]
pub(crate) use sequence::list_output_bytes;
#[cfg(test)]
pub(crate) use sequence::list_to_value_items;
pub(crate) use value::failure_diagnostic_value_in;
pub(crate) use value::lazy_root_wait;
pub(crate) use value::poll_lazy_route;
#[cfg(test)]
pub(crate) use value::{
    evaluation_context_frame, evaluation_context_frame_with_args, failure_diagnostic_value,
    halt_diagnostic_value,
};

pub(crate) use access_machine::{ConversionPoll, KeyConversionMachine, KeyListMachine};
use application::*;
#[cfg(test)]
use builtins::apply_builtin;
#[cfg(test)]
use builtins::apply_builtin_in;
pub(crate) use list_machine::{ListFrontMachine, ListFrontPoll};
use net::*;
use operator::*;
#[cfg(test)]
use sequence::append_sequence;
#[cfg(test)]
use test_support::*;
pub(crate) use value::promise_root_wait;
#[cfg(test)]
use value::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "eval/tests/small_stack.rs"]
mod small_stack_tests;
