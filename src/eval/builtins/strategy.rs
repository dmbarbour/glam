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
    match &first {
        Value::Metadata(_) => {
            context.spark(
                first
                    .associated_metadata()
                    .expect("a metadata carrier must retain its hidden value"),
            );
        }
        Value::Lazy(_) | Value::Promised(_) | Value::Net(_) => {
            // The strategy input may itself resolve to a carrier. Queue a
            // private demand rather than stopping at that outer shell.
            context.spark(Value::deferred("spark strategy demand", move |context| {
                demand(context, &first)?;
                Ok((*keys::UNIT_VALUE).clone())
            }));
        }
        _ => {}
    }
    target.clone()
}

/// Demands an ordinary strategy input, then its hidden value when that input
/// resolves to a sealed metadata carrier.
fn demand(context: &EvalContext, value: &Value) -> Result<(), EvaluationHalt> {
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
