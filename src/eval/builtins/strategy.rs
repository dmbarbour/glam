//! Evaluation-strategy hints exposed both directly and through annotations.

use super::super::*;

pub(super) fn apply(
    context: &EvalContext,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    let [first, target] = super::exact(arguments, builtin_name(builtin))?;
    match builtin {
        Builtin::Seq => seq(context, &first, &target),
        Builtin::Spark => Ok(spark(context, first, &target)),
        _ => unreachable!("strategy dispatcher received a different builtin"),
    }
}

pub(in crate::eval) fn seq(
    context: &EvalContext,
    first: &Value,
    target: &Value,
) -> Result<Value, EvaluationHalt> {
    demand(context, first)?;
    Ok(target.clone())
}

pub(in crate::eval) fn spark(context: &EvalContext, first: Value, target: &Value) -> Value {
    context.spark(first);
    target.clone()
}

/// Demands an ordinary strategy input, then its hidden value when that input
/// resolves to a sealed metadata carrier.
pub(crate) fn demand(context: &EvalContext, value: &Value) -> Result<(), EvaluationHalt> {
    let value = eval_value(context, value)?;
    if let Some(metadata) = value.associated_metadata() {
        eval_value(context, &metadata)?;
    }
    Ok(())
}

fn builtin_name(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::Seq => "seq",
        Builtin::Spark => "spark",
        _ => unreachable!("strategy dispatcher received a different builtin"),
    }
}
