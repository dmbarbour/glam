//! Lambda-style interfaces for opaque interaction-net values.

use super::super::*;

mod construction;

pub(in crate::eval) use construction::NetConstructionMachine;

pub(super) fn apply(
    context: &EvalContext,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    match builtin {
        Builtin::InteractionNet => {
            let [effect] = super::exact(arguments, "interaction_net")?;
            Ok(Value::Lazy(LazyValue::from_net_construction(effect)))
        }
        Builtin::NetArity => apply_net_arity(context, arguments),
        _ => unreachable!("net builtin dispatcher received another builtin"),
    }
}

fn apply_net_arity(context: &EvalContext, arguments: Vec<Value>) -> Result<Value, EvalError> {
    let [arity, net] = super::exact(arguments, "net_arity")?;
    let arity = eval_index_number(context, &arity, "net_arity")?;
    let net = eval_value(context, &net)?;
    let Value::Net(net) = net else {
        return Err(EvalError::new(
            "net_arity builtin requires an interaction-net value",
        ));
    };

    Ok(if arity == 0 {
        Value::Lazy(LazyValue::from_net_computation(net))
    } else {
        Value::Function(FunctionValue::new(net, arity))
    })
}
