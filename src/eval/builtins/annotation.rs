use super::super::*;

mod implementation;

pub(in crate::eval) use implementation::is_undefined_value;
use implementation::*;

pub(super) fn apply(
    context: &EvaluatorStepContext<'_>,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    let [annotation, target] = super::exact(arguments, "anno")?;
    eval_anno_builtin(context, &annotation, &target)
}
