//! Syntax-independent resolved effect sequencing.
//!
//! Surface constructs expand into this affine stream after name resolution.
//! Recursive-do planning decorates the stream separately; conditionals can
//! reuse the same steps without acquiring do-specific state.

use super::super::*;

pub(super) struct ResolvedEffectStep {
    pub(super) line: usize,
    pub(super) kind: ResolvedEffectStepKind,
}

pub(super) enum ResolvedEffectStepKind {
    EffectBind {
        operation: ResolvedExpr<Value>,
        binding: BindingId,
    },
    ValueBind {
        value: ResolvedExpr<Value>,
        binding: BindingId,
    },
    Then {
        operation: ResolvedExpr<Value>,
        result: BindingId,
        diagnostic_context: &'static str,
    },
}

pub(super) enum ResolvedPatternInput {
    Effect(ResolvedExpr<Value>),
    Value(ResolvedExpr<Value>),
}

impl ResolvedPatternInput {
    pub(super) fn into_step(self, line: usize, binding: BindingId) -> ResolvedEffectStep {
        let kind = match self {
            Self::Effect(operation) => ResolvedEffectStepKind::EffectBind { operation, binding },
            Self::Value(value) => ResolvedEffectStepKind::ValueBind { value, binding },
        };
        ResolvedEffectStep { line, kind }
    }
}

pub(super) fn emit_effect_steps<I>(
    steps: I,
    mut continuation: ResolvedExpr<Value>,
) -> ResolvedExpr<Value>
where
    I: IntoIterator<Item = ResolvedEffectStep>,
    I::IntoIter: DoubleEndedIterator,
{
    for step in steps.into_iter().rev() {
        continuation = emit_effect_step(step, continuation);
    }
    continuation
}

fn emit_effect_step(
    step: ResolvedEffectStep,
    continuation: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    match step.kind {
        ResolvedEffectStepKind::EffectBind { operation, binding } => effect_call_resolved(
            "seq",
            [operation, ResolvedExpr::lambda(vec![binding], continuation)],
        ),
        ResolvedEffectStepKind::ValueBind { value, binding } => {
            ResolvedExpr::apply(ResolvedExpr::lambda(vec![binding], continuation), [value])
        }
        ResolvedEffectStepKind::Then {
            operation,
            result,
            diagnostic_context,
        } => {
            let body = assert_unit_resolved(
                diagnostic_context,
                ResolvedExpr::Local(result),
                continuation,
            );
            effect_call_resolved("seq", [operation, ResolvedExpr::lambda(vec![result], body)])
        }
    }
}
