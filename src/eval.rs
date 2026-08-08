//! Core value evaluation and interaction-net integration.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;

use crate::core::{
    Builtin, BuiltinCall, CoreValueFactory, EvaluationHalt, FunctionCode, FunctionValue, Key,
    LazyValue, List, NetValue, PromisedValue, Value, keys,
};
use crate::core_net::{CoreDataKey, CoreOperator, CoreSpecialization};
use crate::evaluation::EvalContext;
use crate::interaction_net::{
    ActivePairKey, Call, Callable, CursorDependency, NetBuilder, NetSpecialization, OperatorCall,
    OperatorYield, Port, Reduction, ReductionKind, StuckReason,
};
use crate::number::Number;
#[cfg(test)]
use crate::{evaluation::OwnedEvalContext, list::ListItem};

mod application;
mod builtins;
mod net;
mod operator;
mod sequence;
#[cfg(test)]
mod test_support;
mod value;

pub(crate) use application::apply_values;
pub(crate) use builtins::demand_strategy_value;
pub(crate) use operator::{
    access_operator, apply_arity_operator, computation_capture_operator, constant_effect,
    function_capture_operator, list_operator, request_operator,
};
#[cfg(test)]
pub(crate) use sequence::list_output_bytes;
pub(crate) use sequence::{eval_key_path_list, list_output_bytes_range, list_to_value_items};
pub use value::eval_value;
#[cfg(test)]
pub(crate) use value::halt_diagnostic_value;
pub(crate) use value::{
    evaluation_context_frame, evaluation_context_frame_with_args, failure_diagnostic_value,
    failure_diagnostic_value_with, halt_diagnostic_value_with, pop_list_front,
};

use application::*;
use builtins::apply_builtin;
use net::*;
use operator::*;
use sequence::*;
#[cfg(test)]
use test_support::*;
use value::*;

#[cfg(test)]
mod tests;
