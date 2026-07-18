//! Core operators and specialization for generic interaction nets.
//!
//! Front-end semantic lowering lives in `g_syntax`; this module deliberately
//! contains no expression language.

use std::sync::Arc;

use crate::core::{BuiltinCall, FunctionCode, Key, Value};
use crate::evaluation::EvaluationWaitToken;
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
    /// Reifies an opaque-tagged external effect request without performing it
    /// during interaction-net evaluation.
    Request {
        tag: Key,
        arity: usize,
        supplied: Arc<[Value]>,
        wrap_effect: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreSpecialization;

/// Opaque identity for evaluator work that suspends a core net call. The weak
/// session provenance remains hidden from the generic runtime, which only
/// clones and compares tokens for exact wakeups.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoreWaitToken(pub(crate) EvaluationWaitToken);

impl CoreWaitToken {
    pub(crate) fn task_id(&self) -> u64 {
        self.0.get()
    }
}

pub type CoreInteractionNet = InteractionNet<CoreSpecialization>;
pub type CoreRuntimeNet = SharedRuntimeNet<CoreSpecialization>;
