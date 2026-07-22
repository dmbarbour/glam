use super::super::*;

pub(in crate::g_syntax) fn lower_do_expr_resolved(
    do_expr: &DoExpr,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    lower_do_steps_resolved(
        &do_expr.steps,
        &do_expr.result,
        do_expr.result_line,
        context,
        scope,
        locals,
    )
}

fn lower_do_steps_resolved(
    steps: &[DoStep],
    result: &SyntaxExpr,
    result_line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let Some((step, remaining_steps)) = steps.split_first() else {
        return syntax_expr_to_resolved_in_semantic_scope(
            result,
            result_line,
            context,
            scope,
            locals,
        );
    };

    match &step.kind {
        DoStepKind::Bind { name, operation } => {
            // The producing operation is outside the new binding's scope.
            let operation = syntax_expr_to_resolved_in_semantic_scope(
                operation, step.line, context, scope, locals,
            )?;
            let base_len = locals.len();
            let [binding] = locals
                .extend_source_bindings([name.as_str()], step.line)?
                .try_into()
                .expect("one do binder produces one binding identity");
            let continuation = lower_do_steps_resolved(
                remaining_steps,
                result,
                result_line,
                context,
                scope,
                locals,
            );
            locals.truncate(base_len);
            let continuation = ResolvedExpr::lambda(vec![binding], continuation?);
            Ok(effect_call_resolved("seq", [operation, continuation]))
        }
        DoStepKind::ValueBind { name, value } => {
            // A name-only value guard is irrefutable, so normalize its
            // `.r value >>= continuation` semantics to ordinary lazy apply.
            let value = syntax_expr_to_resolved_in_semantic_scope(
                value, step.line, context, scope, locals,
            )?;
            let base_len = locals.len();
            let [binding] = locals
                .extend_source_bindings([name.as_str()], step.line)?
                .try_into()
                .expect("one do value binder produces one binding identity");
            let continuation = lower_do_steps_resolved(
                remaining_steps,
                result,
                result_line,
                context,
                scope,
                locals,
            );
            locals.truncate(base_len);
            Ok(ResolvedExpr::apply(
                ResolvedExpr::lambda(vec![binding], continuation?),
                [value],
            ))
        }
        DoStepKind::Then(operation) => {
            let operation = syntax_expr_to_resolved_in_semantic_scope(
                operation, step.line, context, scope, locals,
            )?;
            let continuation = lower_do_steps_resolved(
                remaining_steps,
                result,
                result_line,
                context,
                scope,
                locals,
            )?;
            Ok(effect_then_resolved(operation, continuation, locals))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::number::Number;

    fn resolve(expr: &SyntaxExpr) -> ResolvedExpr<Value> {
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        syntax_expr_to_resolved_in_scope(expr, 1, &context, &scope, &mut ResolverContext::default())
            .expect("do expression should resolve")
    }

    #[test]
    fn final_expression_is_lowered_without_an_implicit_effect_operation() {
        let resolved = resolve(&SyntaxExpr::Do(DoExpr {
            steps: Vec::new(),
            result_line: 2,
            result: Box::new(SyntaxExpr::Number(42.into())),
        }));

        assert!(
            matches!(resolved, ResolvedExpr::Embedded(Value::Number(number))
            if number == Number::from(42_i64))
        );
    }

    #[test]
    fn irrefutable_value_binding_uses_fused_lambda_application() {
        let resolved = resolve(&SyntaxExpr::Do(DoExpr {
            steps: vec![DoStep {
                line: 2,
                kind: DoStepKind::ValueBind {
                    name: "value".to_owned(),
                    value: SyntaxExpr::Number(42.into()),
                },
            }],
            result_line: 3,
            result: Box::new(SyntaxExpr::Name("value".to_owned())),
        }));

        assert!(matches!(
            resolved,
            ResolvedExpr::ApplyLambda {
                parameters,
                body,
                arguments,
            } if matches!(body.as_ref(), ResolvedExpr::Local(binding)
                if *binding == parameters[0])
                && matches!(arguments.as_slice(),
                    [ResolvedExpr::Embedded(Value::Number(number))]
                        if *number == Number::from(42_i64))
        ));
    }
}
