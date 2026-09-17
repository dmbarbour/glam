use super::super::*;
use crate::core::ListEffectComputation;

mod implementation;

use implementation::*;

pub(super) fn apply(
    context: &EvaluatorStepContext<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    match builtin {
        Builtin::ListEffect => {
            let [effect] = super::exact(arguments, "list effect")?;
            Ok(Value::List(deferred_list(
                context,
                "list effect",
                ListEffectComputation::Run { effect },
            )))
        }
        Builtin::ListEffectReturn => {
            let [value] = super::exact(arguments, "list effect return")?;
            Ok(Value::List(List::from_values(vec![value])))
        }
        Builtin::ListEffectSeq => {
            let [operation, continuation] = super::exact(arguments, "list effect seq")?;
            let results = deferred_list(
                context,
                "list effect",
                ListEffectComputation::Run { effect: operation },
            );
            Ok(Value::List(deferred_list(
                context,
                "list effect seq",
                ListEffectComputation::Sequence {
                    results,
                    continuation,
                },
            )))
        }
        Builtin::ListEffectAlt => {
            let [left, right] = super::exact(arguments, "list effect alt")?;
            let left = deferred_list(
                context,
                "list effect",
                ListEffectComputation::Run { effect: left },
            );
            let right = deferred_list(
                context,
                "list effect",
                ListEffectComputation::Run { effect: right },
            );
            Ok(Value::List(List::concat(left, right)))
        }
        Builtin::ListEffectCut => {
            let [operation] = super::exact(arguments, "list effect cut")?;
            Ok(Value::List(deferred_list(
                context,
                "list effect cut",
                ListEffectComputation::Cut { operation },
            )))
        }
        Builtin::ListEffectFix => {
            let [function] = super::exact(arguments, "list effect fix")?;
            eval_list_effect_fix_builtin(context, &function)
        }
        _ => unreachable!("list-effect dispatcher received another builtin"),
    }
}
