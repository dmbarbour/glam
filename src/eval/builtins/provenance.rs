use super::*;

pub(super) fn apply(context: &EvalContext, arguments: Vec<Value>) -> Result<Value, EvalError> {
    let [origin] = exact::<1>(arguments, "origin inspection")?;
    let origin = eval_value(context, &origin)
        .map_err(|error| error.with_context(evaluation_context_frame("compilation_origin")))?;
    let Value::Opaque(origin) = origin else {
        return Err(EvalError::new(
            "origin inspection requires an opaque compilation origin",
        ));
    };
    crate::diagnostic::inspect_compilation_origin(&origin)
        .ok_or_else(|| EvalError::new("origin inspection requires an opaque compilation origin"))
}
