//! Core operators and specialization for generic interaction nets.
//!
//! Front-end semantic lowering lives in `g_syntax`; this module deliberately
//! contains no expression language.

use std::sync::Arc;

use crate::core::{BuiltinCall, FunctionCode, Key, Value};
use crate::interaction_net::{InteractionNet, SharedRuntimeNet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreDataKey {
    Key(Key),
    Index,
    PathIndex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreOperator {
    ApplyArity {
        arity: usize,
        supplied: Arc<[Value]>,
    },
    FunctionCaptures {
        code: Arc<FunctionCode>,
        supplied: Arc<[Value]>,
    },
    ComputationCaptures {
        code: Arc<FunctionCode>,
        supplied: Arc<[Value]>,
    },
    Dict {
        keys: Arc<[Key]>,
        supplied: Arc<[Value]>,
    },
    Builtin(BuiltinCall),
    Applicable(Value),
    List {
        arity: usize,
        supplied: Arc<[Value]>,
    },
    Access {
        path: Arc<[CoreDataKey]>,
        supplied: Arc<[Value]>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreSpecialization;

pub type CoreInteractionNet = InteractionNet<CoreSpecialization>;
pub type CoreRuntimeNet = SharedRuntimeNet<CoreSpecialization>;
