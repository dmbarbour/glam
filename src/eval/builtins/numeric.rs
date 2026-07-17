use super::super::*;

mod implementation;

use implementation::*;

pub(super) fn apply(builtin: Builtin, arguments: Vec<Value>) -> Result<Value, EvalError> {
    match builtin {
        Builtin::Add => {
            let [left, right] = super::exact(arguments, "add")?;
            eval_numeric_builtin("add", &left, &right, Number::add)
        }
        Builtin::Subtract => {
            let [left, right] = super::exact(arguments, "subtract")?;
            eval_numeric_builtin("subtract", &left, &right, Number::sub)
        }
        Builtin::Multiply => {
            let [left, right] = super::exact(arguments, "multiply")?;
            eval_numeric_builtin("multiply", &left, &right, Number::mul)
        }
        Builtin::Divide => {
            let [left, right] = super::exact(arguments, "divide")?;
            eval_numeric_divide_builtin(&left, &right)
        }
        Builtin::Floor => {
            let [value] = super::exact(arguments, "floor")?;
            eval_floor_builtin(&value)
        }
        Builtin::Mod => {
            let [left, right] = super::exact(arguments, "mod")?;
            eval_numeric_mod_builtin(&left, &right)
        }
        _ => unreachable!("numeric dispatcher received a non-numeric builtin"),
    }
}
