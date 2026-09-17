use super::super::*;

mod implementation;

use implementation::*;

pub(super) fn apply(
    context: &EvaluatorStepContext<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    match builtin {
        Builtin::Append => {
            let [left, right] = super::exact(arguments, "append")?;
            context.with_value_access(|access| append_values(access.values(), left, right))
        }
        Builtin::ListConcat => {
            let [value] = super::exact(arguments, "list concat")?;
            eval_list_concat_builtin(context, &value)
        }
        Builtin::TextLines => {
            let [value] = super::exact(arguments, "text lines")?;
            eval_text_lines_builtin(context, &value)
        }
        _ => unreachable!("list dispatcher received a non-list builtin"),
    }
}
