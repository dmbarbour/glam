use super::*;

pub(super) fn apply(context: &EvalContext, arguments: Vec<Value>) -> Result<Value, EvaluationHalt> {
    let [origin] = exact::<1>(arguments, "origin inspection")?;
    let origin = eval_value(context, &origin).map_err(|error| {
        context.values().with_runtime_value_access(|access| {
            error.with_context(&access, evaluation_context_frame("compilation_origin"))
        })
    })?;
    let Value::Opaque(origin) = origin else {
        return Err(EvaluationHalt::new(
            "origin inspection requires an opaque compilation origin",
        ));
    };
    crate::diagnostic::inspect_compilation_origin(context.values(), &origin).ok_or_else(|| {
        EvaluationHalt::new("origin inspection requires an opaque compilation origin")
    })
}
