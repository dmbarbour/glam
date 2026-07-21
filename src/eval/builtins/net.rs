//! Lambda-style interfaces for opaque interaction-net values.

use super::super::*;

pub(super) fn apply(context: &EvalContext, arguments: Vec<Value>) -> Result<Value, EvalError> {
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
