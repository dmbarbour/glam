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
    access: &RuntimeValueAccess<'_>,
    steps: I,
    mut continuation: ResolvedExpr<Value>,
) -> ResolvedExpr<Value>
where
    I: IntoIterator<Item = ResolvedEffectStep>,
    I::IntoIter: DoubleEndedIterator,
{
    for step in steps.into_iter().rev() {
        continuation = emit_effect_step(access, step, continuation);
    }
    continuation
}

/// The results of `steps` followed by `results`, as running them with the
/// list handler gives them (see `ResolvedChoice::emit_results`): each
/// effect a step binds runs with the handler, and the rest follows for each
/// of its results in turn.
pub(super) fn emit_effect_step_results<I>(
    access: &RuntimeValueAccess<'_>,
    steps: I,
    mut results: ResolvedExpr<Value>,
) -> ResolvedExpr<Value>
where
    I: IntoIterator<Item = ResolvedEffectStep>,
    I::IntoIter: DoubleEndedIterator,
{
    for step in steps.into_iter().rev() {
        results = match step.kind {
            ResolvedEffectStepKind::EffectBind { operation, binding } => {
                for_each_effect_result_resolved(access, operation, binding, results)
            }
            ResolvedEffectStepKind::ValueBind { value, binding } => {
                ResolvedExpr::apply(ResolvedExpr::lambda(vec![binding], results), [value])
            }
            ResolvedEffectStepKind::Then {
                operation,
                result,
                diagnostic_context,
            } => {
                let rest = assert_unit_resolved(
                    access,
                    diagnostic_context,
                    ResolvedExpr::Local(result),
                    results,
                );
                for_each_effect_result_resolved(access, operation, result, rest)
            }
        };
    }
    results
}

/// The results of `rest`, which gives results of its own, for each result
/// of running `operation` with the list handler bound to `binding`, in
/// order: a step of a search as the list handler runs it.
pub(in crate::g_syntax) fn for_each_effect_result_resolved(
    _access: &RuntimeValueAccess<'_>,
    operation: ResolvedExpr<Value>,
    binding: BindingId,
    rest: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    let each = ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::Map)),
        [
            ResolvedExpr::lambda(vec![binding], rest),
            ResolvedExpr::apply(
                ResolvedExpr::Embedded(Value::Builtin(Builtin::ListEffect)),
                [operation],
            ),
        ],
    );
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::ListConcat)),
        [each],
    )
}

fn emit_effect_step(
    access: &RuntimeValueAccess<'_>,
    step: ResolvedEffectStep,
    continuation: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    match step.kind {
        ResolvedEffectStepKind::EffectBind { operation, binding } => effect_call_resolved(
            access,
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
                access,
                diagnostic_context,
                ResolvedExpr::Local(result),
                continuation,
            );
            effect_call_resolved(
                access,
                "seq",
                [operation, ResolvedExpr::lambda(vec![result], body)],
            )
        }
    }
}
