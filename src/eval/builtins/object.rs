use super::super::*;
mod implementation;

use implementation::*;

pub(super) fn apply(
    context: &EvaluatorStepContext<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    match builtin {
        Builtin::ObjectFromDict => {
            let [value] = super::exact(arguments, "object_from_dict")?;
            eval_object_from_dict_builtin(context, &value)
        }
        _ => unreachable!("object dispatcher received another builtin"),
    }
}
