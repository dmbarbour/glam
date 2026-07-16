use super::super::*;

mod basic;
mod merge;

pub(super) use basic::eval_dict_union_builtin;
use basic::*;
use merge::*;

pub(super) fn apply(
    builtin: Builtin,
    arguments: Vec<Value>,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    match builtin {
        Builtin::DictSingleton => {
            let [key, value] = super::exact(arguments, "singleton")?;
            eval_singleton_builtin(&key, &value, local_env)
        }
        Builtin::DictUnion => {
            let [left, right] = super::exact(arguments, "dict union")?;
            eval_dict_union_builtin(&left, &right, local_env)
        }
        Builtin::DictUpdate => {
            let [path, new_value, dict] = super::exact(arguments, "dict update")?;
            eval_dict_update_builtin(&path, &new_value, &dict, local_env)
        }
        Builtin::MergeDuplicate => {
            let [name, left, right] = super::exact(arguments, "merge duplicate")?;
            eval_merge_duplicate_builtin(&name, &left, &right, local_env)
        }
        _ => unreachable!("dictionary dispatcher received another builtin"),
    }
}
