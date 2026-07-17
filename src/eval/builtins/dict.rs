use super::super::*;

mod basic;
mod merge;

pub(super) use basic::eval_dict_union_builtin;
use basic::*;
use merge::*;

pub(super) fn apply(
    context: &EvalContext,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    match builtin {
        Builtin::DictSingleton => {
            let [key, value] = super::exact(arguments, "singleton")?;
            eval_singleton_builtin(context, &key, &value)
        }
        Builtin::DictUnion => {
            let [left, right] = super::exact(arguments, "dict union")?;
            eval_dict_union_builtin(context, &left, &right)
        }
        Builtin::DictUpdate => {
            let [path, new_value, dict] = super::exact(arguments, "dict update")?;
            eval_dict_update_builtin(context, &path, &new_value, &dict)
        }
        Builtin::MergeDuplicate => {
            let [name, left, right] = super::exact(arguments, "merge duplicate")?;
            eval_merge_duplicate_builtin(context, &name, &left, &right)
        }
        _ => unreachable!("dictionary dispatcher received another builtin"),
    }
}
