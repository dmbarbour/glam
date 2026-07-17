use super::super::*;

mod implementation;

use implementation::*;
pub(in crate::eval) use implementation::{annotation_error_value, atom_name, is_undefined_value};

pub(super) fn apply(context: &EvalContext, arguments: Vec<Value>) -> Result<Value, EvalError> {
    let [annotation, target] = super::exact(arguments, "anno")?;
    eval_anno_builtin(context, &annotation, &target)
}
