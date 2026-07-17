use super::super::*;

mod implementation;

use implementation::*;

pub(super) fn apply(builtin: Builtin, arguments: Vec<Value>) -> Result<Value, EvalError> {
    match builtin {
        Builtin::Fixpoint => {
            let [function] = super::exact(arguments, "fixpoint")?;
            eval_fixpoint_builtin(&function)
        }
        Builtin::EffectApply => {
            let [function, argument, api] = super::exact(arguments, "effect apply")?;
            apply_values(eval_value(&function)?, vec![api, argument])
        }
        Builtin::EffectCall => {
            let [name, arguments, api] = super::exact(arguments, "effect call")?;
            let name = value_to_key(&eval_value(&name)?)?;
            let function = resolve_core_access(&[api], &[CoreDataKey::Key(name)])?;
            let arguments = match force_value_shell(&arguments)? {
                Value::List(arguments) => list_to_value_items(&arguments)?,
                _ => {
                    return Err(EvalError::new(
                        "effect call builtin requires a list of arguments",
                    ));
                }
            };
            apply_values(function, arguments)
        }
        _ => unreachable!("effect dispatcher received another builtin"),
    }
}
