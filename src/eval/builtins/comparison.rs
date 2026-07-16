use std::cmp::Ordering;

use super::super::*;

mod implementation;

use implementation::*;

pub(super) fn apply(
    builtin: Builtin,
    arguments: Vec<Value>,
    local_env: &[Value],
) -> Result<Value, EvalError> {
    let [left, right] = super::exact(arguments, comparison_name(builtin))?;
    match builtin {
        Builtin::Greater => {
            eval_compare_ordering_builtin("greater-than", &left, &right, local_env, |ordering| {
                ordering == Ordering::Greater
            })
        }
        Builtin::GreaterEqual => eval_compare_ordering_builtin(
            "greater-than-or-equal",
            &left,
            &right,
            local_env,
            |ordering| ordering != Ordering::Less,
        ),
        Builtin::Equal => {
            eval_compare_equality_builtin("equal", &left, &right, local_env, |equal| equal)
        }
        Builtin::NotEqual => {
            eval_compare_equality_builtin("not-equal", &left, &right, local_env, |equal| !equal)
        }
        Builtin::LessEqual => eval_compare_ordering_builtin(
            "less-than-or-equal",
            &left,
            &right,
            local_env,
            |ordering| ordering != Ordering::Greater,
        ),
        Builtin::Less => {
            eval_compare_ordering_builtin("less-than", &left, &right, local_env, |ordering| {
                ordering == Ordering::Less
            })
        }
        _ => unreachable!("comparison dispatcher received a non-comparison builtin"),
    }
}

fn comparison_name(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::Greater => "greater-than",
        Builtin::GreaterEqual => "greater-than-or-equal",
        Builtin::Equal => "equal",
        Builtin::NotEqual => "not-equal",
        Builtin::LessEqual => "less-than-or-equal",
        Builtin::Less => "less-than",
        _ => unreachable!("comparison name requested for another builtin"),
    }
}
