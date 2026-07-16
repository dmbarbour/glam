use super::super::*;

mod implementation;

use implementation::*;

pub(super) fn apply(
    builtin: Builtin,
    arguments: Vec<Value>,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    match builtin {
        Builtin::ListEffect => {
            let [effect] = super::exact(arguments, "list effect")?;
            eval_list_effect_builtin(&effect, local_env)
        }
        Builtin::ListEffectReturn => {
            let [value] = super::exact(arguments, "list effect return")?;
            Ok(Value::List(List::from_values(vec![value])))
        }
        Builtin::ListEffectSeq => {
            let [operation, continuation] = super::exact(arguments, "list effect seq")?;
            eval_list_effect_seq_builtin(&operation, &continuation, local_env)
        }
        Builtin::ListEffectAlt => {
            let [left, right] = super::exact(arguments, "list effect alt")?;
            eval_list_effect_alt_builtin(&left, &right, local_env)
        }
        Builtin::ListEffectCut => {
            let [operation] = super::exact(arguments, "list effect cut")?;
            eval_list_effect_cut_builtin(&operation, local_env)
        }
        Builtin::ListEffectFix => {
            let [function] = super::exact(arguments, "list effect fix")?;
            eval_list_effect_fix_builtin(&function, local_env)
        }
        _ => unreachable!("list-effect dispatcher received another builtin"),
    }
}
