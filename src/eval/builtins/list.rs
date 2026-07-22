use super::super::*;

mod implementation;

pub(super) use implementation::list_like_value;
use implementation::*;

pub(super) fn apply(
    context: &EvalContext,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    match builtin {
        Builtin::Append => {
            let [left, right] = super::exact(arguments, "append")?;
            append_values(left, right)
        }
        Builtin::Slice => {
            let [start, end, value] = super::exact(arguments, "slice")?;
            eval_slice_builtin(context, &start, &end, &value)
        }
        Builtin::Map => {
            let [function, value] = super::exact(arguments, "map")?;
            eval_map_builtin(context, &function, &value)
        }
        Builtin::ListConcat => {
            let [value] = super::exact(arguments, "list concat")?;
            eval_list_concat_builtin(context, &value)
        }
        Builtin::ListLen => {
            let [value] = super::exact(arguments, "list len")?;
            eval_list_len_builtin(context, &value)
        }
        Builtin::ListSplit => {
            let [index, value] = super::exact(arguments, "list split")?;
            eval_list_split_builtin(context, &index, &value)
        }
        Builtin::ListSplitEnd => {
            let [count, value] = super::exact(arguments, "list split_end")?;
            eval_list_split_end_builtin(context, &count, &value)
        }
        Builtin::ListAt => {
            let [index, value] = super::exact(arguments, "list at")?;
            eval_list_at_builtin(context, &index, &value)
        }
        Builtin::ListHead => {
            let [value] = super::exact(arguments, "list head")?;
            eval_list_head_builtin(context, &value)
        }
        Builtin::ListTail => {
            let [value] = super::exact(arguments, "list tail")?;
            eval_list_tail_builtin(context, &value)
        }
        Builtin::TextLines => {
            let [value] = super::exact(arguments, "text lines")?;
            eval_text_lines_builtin(context, &value)
        }
        _ => unreachable!("list dispatcher received a non-list builtin"),
    }
}
