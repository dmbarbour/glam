use std::cmp::Ordering;

use super::super::*;

mod implementation;

use implementation::*;

pub(super) fn apply(
    context: &EvalContext,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvalError> {
    let [left, right] = super::exact(arguments, comparison_name(builtin))?;
    match builtin {
        Builtin::Greater => {
            eval_compare_ordering_builtin(context, "greater-than", &left, &right, |ordering| {
                ordering == Ordering::Greater
            })
        }
        Builtin::GreaterEqual => eval_compare_ordering_builtin(
            context,
            "greater-than-or-equal",
            &left,
            &right,
            |ordering| ordering != Ordering::Less,
        ),
        Builtin::Equal => {
            eval_compare_equality_builtin(context, "equal", &left, &right, |equal| equal)
        }
        Builtin::NotEqual => {
            eval_compare_equality_builtin(context, "not-equal", &left, &right, |equal| !equal)
        }
        Builtin::LessEqual => eval_compare_ordering_builtin(
            context,
            "less-than-or-equal",
            &left,
            &right,
            |ordering| ordering != Ordering::Greater,
        ),
        Builtin::Less => {
            eval_compare_ordering_builtin(context, "less-than", &left, &right, |ordering| {
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
