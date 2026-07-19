//! Evaluation-strategy hints exposed both directly and through annotations.

use super::super::*;

pub(super) fn apply(
    context: &EvalContext,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
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
) -> Result<Value, EvalError> {
    force_value_shell(context, first)?;
    Ok(target.clone())
}

pub(in crate::eval) fn spark(context: &EvalContext, first: Value, target: &Value) -> Value {
    context.spark(first);
    target.clone()
}

fn builtin_name(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::Seq => "seq",
        Builtin::Spark => "spark",
        _ => unreachable!("strategy dispatcher received a different builtin"),
    }
}
