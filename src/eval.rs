//! Core value evaluation and interaction-net integration.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;

use crate::core::{
    Builtin, BuiltinCall, FunctionCode, FunctionValue, Key, LazyValue, List, NetValue,
    PromisedValue, Value, keys,
};
use crate::core_net::{CoreDataKey, CoreOperator, CoreSpecialization};
use crate::evaluation::EvalContext;
use crate::interaction_net::{
    ActivePairKey, Call, Callable, CursorDependency, NetBuilder, NetSpecialization, OperatorCall,
    OperatorYield, Port, Reduction, ReductionKind, StuckReason,
};
#[cfg(test)]
use crate::list::ListItem;
use crate::number::Number;

mod application;
mod builtins;
mod net;
mod operator;
mod sequence;
#[cfg(test)]
mod test_support;
mod value;

pub(crate) use application::apply_values;
pub(crate) use operator::{
    access_operator, apply_arity_operator, computation_capture_operator, function_capture_operator,
    list_operator, request_operator,
};
pub use sequence::list_output_bytes;
pub(crate) use sequence::{eval_key_path_list, list_output_bytes_range, list_to_value_items};
pub use value::{EvalError, eval_value};

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
