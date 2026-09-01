//! Lambda-style interfaces for opaque interaction-net values.

use super::super::*;

mod construction;

pub(in crate::eval) use construction::NetConstructionMachine;

pub(super) fn apply(
    context: &EvaluatorStepContext<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    match builtin {
        Builtin::InteractionNet => {
            let [effect] = super::exact(arguments, "interaction_net")?;
            Ok(Value::Lazy(LazyValue::from_net_construction(
                context.context().values(),
                effect,
            )))
        }
        Builtin::NetArity => apply_net_arity(context, arguments),
        _ => unreachable!("net builtin dispatcher received another builtin"),
    }
}

fn apply_net_arity(
    context: &EvaluatorStepContext<'_>,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    let [arity, net] = super::exact(arguments, "net_arity")?;
    let arity = eval_index_number_in(context, &arity, "net_arity", "net_arity")?;
    let net = eval_value_in(context, &net)?;
    let Value::Net(net) = net else {
        return Err(EvaluationHalt::new(
            "net_arity builtin requires an interaction-net value",
        ));
    };

    Ok(if arity == 0 {
        Value::Lazy(LazyValue::from_net_computation(
            context.context().values(),
            net,
        ))
    } else {
        Value::Function(FunctionValue::new(net, arity))
    })
}
