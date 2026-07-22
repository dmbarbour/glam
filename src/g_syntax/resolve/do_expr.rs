use super::super::*;
use crate::number::Number;

struct ForwardBinding {
    canonical: String,
    slot: usize,
    resolved: Option<BindingId>,
}

struct DoLowering<'a> {
    result: &'a SyntaxExpr,
    result_line: usize,
    context: &'a CompileContext,
    scope: &'a NameScope<ResolvedRoot>,
}

pub(in crate::g_syntax) fn lower_do_expr_resolved(
    do_expr: &DoExpr,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    DoLowering {
        result: &do_expr.result,
        result_line: do_expr.result_line,
        context,
        scope,
    }
    .lower_steps(&do_expr.steps, locals)
}

impl DoLowering<'_> {
    fn lower_steps(
        &self,
        steps: &[DoStep],
        locals: &mut ResolverContext,
    ) -> Result<ResolvedExpr<Value>, Diagnostic> {
        let Some((step, remaining_steps)) = steps.split_first() else {
            return syntax_expr_to_resolved_in_semantic_scope(
                self.result,
                self.result_line,
                self.context,
                self.scope,
                locals,
            );
        };

        match &step.kind {
            DoStepKind::Abstract(names) => {
                self.lower_recursive_region(names, step.line, remaining_steps, locals)
            }
            DoStepKind::Bind { name, operation } => {
                // The producing operation is outside the new binding's scope.
                let operation = syntax_expr_to_resolved_in_semantic_scope(
                    operation,
                    step.line,
                    self.context,
                    self.scope,
                    locals,
                )?;
                let base_len = locals.len();
                let [binding] = locals
                    .extend_source_bindings([name.as_str()], step.line)?
                    .try_into()
                    .expect("one do binder produces one binding identity");
                let continuation = self.lower_steps(remaining_steps, locals);
                locals.truncate(base_len);
                let continuation = ResolvedExpr::lambda(vec![binding], continuation?);
                Ok(effect_call_resolved("seq", [operation, continuation]))
            }
            DoStepKind::ValueBind { name, value } => {
                // A name-only value guard is irrefutable, so normalize its
                // `.r value >>= continuation` semantics to ordinary lazy apply.
                let value = syntax_expr_to_resolved_in_semantic_scope(
                    value,
                    step.line,
                    self.context,
                    self.scope,
                    locals,
                )?;
                let base_len = locals.len();
                let [binding] = locals
                    .extend_source_bindings([name.as_str()], step.line)?
                    .try_into()
                    .expect("one do value binder produces one binding identity");
                let continuation = self.lower_steps(remaining_steps, locals);
                locals.truncate(base_len);
                Ok(ResolvedExpr::apply(
                    ResolvedExpr::lambda(vec![binding], continuation?),
                    [value],
                ))
            }
            DoStepKind::Then(operation) => {
                let operation = syntax_expr_to_resolved_in_semantic_scope(
                    operation,
                    step.line,
                    self.context,
                    self.scope,
                    locals,
                )?;
                let continuation = self.lower_steps(remaining_steps, locals)?;
                Ok(effect_then_resolved(operation, continuation, locals))
            }
        }
    }
}

impl DoLowering<'_> {
    fn lower_recursive_region(
        &self,
        names: &[String],
        declaration_line: usize,
        following_steps: &[DoStep],
        locals: &mut ResolverContext,
    ) -> Result<ResolvedExpr<Value>, Diagnostic> {
        let (canonical_names, region_len) =
            recursive_region(names, declaration_line, following_steps)?;
        let (region_steps, continuation_steps) = following_steps.split_at(region_len);

        let base_len = locals.len();
        let forward_bindings =
            locals.extend_source_bindings(names.iter().map(String::as_str), declaration_line)?;
        let mut forwards = canonical_names
            .into_iter()
            .enumerate()
            .map(|(offset, canonical)| ForwardBinding {
                canonical,
                slot: base_len + offset,
                resolved: None,
            })
            .collect::<Vec<_>>();

        let future = locals.fresh_binding();
        let region = self.lower_recursive_region_steps(
            region_steps,
            continuation_steps,
            locals,
            &mut forwards,
        );
        locals.truncate(base_len);
        let region = region?;

        let projections = forwards
            .iter()
            .enumerate()
            .map(|(index, _)| list_at_resolved(index, ResolvedExpr::Local(future)))
            .collect::<Vec<_>>();
        let region =
            ResolvedExpr::apply(ResolvedExpr::lambda(forward_bindings, region), projections);
        let fixed = effect_call_resolved("fix", [ResolvedExpr::lambda(vec![future], region)]);

        let fixed_result = locals.fresh_binding();
        let continuation = list_at_resolved(forwards.len(), ResolvedExpr::Local(fixed_result));
        let resumed = ResolvedExpr::apply(
            continuation,
            [ResolvedExpr::Embedded(self.context.unit_value())],
        );
        Ok(effect_call_resolved(
            "seq",
            [fixed, ResolvedExpr::lambda(vec![fixed_result], resumed)],
        ))
    }
}

fn recursive_region(
    names: &[String],
    declaration_line: usize,
    steps: &[DoStep],
) -> Result<(Vec<String>, usize), Diagnostic> {
    if names.is_empty() {
        return Err(Diagnostic::error(
            declaration_line,
            "recursive do abstract declaration requires at least one name",
        ));
    }
    let mut canonical_names = Vec::with_capacity(names.len());
    for name in names {
        let Some(canonical) = local_name_metadata(name).canonical else {
            return Err(Diagnostic::error(
                declaration_line,
                "recursive do abstract declarations require accessible local names",
            ));
        };
        if canonical_names.contains(&canonical) {
            return Err(Diagnostic::error(
                declaration_line,
                format!("duplicate recursive do abstract declaration for `{canonical}`"),
            ));
        }
        canonical_names.push(canonical);
    }

    let mut unresolved = canonical_names.clone();
    for (index, step) in steps.iter().enumerate() {
        match &step.kind {
            DoStepKind::Abstract(nested) => {
                if let Some(duplicate) = nested.iter().find_map(|name| {
                    let canonical = local_name_metadata(name).canonical?;
                    canonical_names.contains(&canonical).then_some(canonical)
                }) {
                    return Err(Diagnostic::error(
                        step.line,
                        format!("duplicate recursive do abstract declaration for `{duplicate}`"),
                    ));
                }
                return Err(Diagnostic::error(
                    step.line,
                    "overlapping recursive do abstract regions are not supported",
                ));
            }
            DoStepKind::Bind { name, .. } | DoStepKind::ValueBind { name, .. } => {
                if let Some(canonical) = local_name_metadata(name).canonical
                    && let Some(position) = unresolved
                        .iter()
                        .position(|candidate| candidate == &canonical)
                {
                    unresolved.remove(position);
                    if unresolved.is_empty() {
                        return Ok((canonical_names, index + 1));
                    }
                }
            }
            DoStepKind::Then(_) => {}
        }
    }

    Err(Diagnostic::error(
        declaration_line,
        format!(
            "recursive do abstract declaration has no later fulfillment for {}",
            unresolved
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ))
}

impl DoLowering<'_> {
    fn lower_recursive_region_steps(
        &self,
        steps: &[DoStep],
        continuation_steps: &[DoStep],
        locals: &mut ResolverContext,
        forwards: &mut [ForwardBinding],
    ) -> Result<ResolvedExpr<Value>, Diagnostic> {
        let Some((step, remaining_steps)) = steps.split_first() else {
            let continuation = self.lower_steps(continuation_steps, locals)?;
            let ignored = locals.fresh_binding();
            let mut record = forwards
                .iter()
                .map(|forward| {
                    ResolvedExpr::Local(
                        forward
                            .resolved
                            .expect("validated recursive do region must fulfill every abstract"),
                    )
                })
                .collect::<Vec<_>>();
            record.push(ResolvedExpr::lambda(vec![ignored], continuation));
            return Ok(effect_call_resolved("r", [ResolvedExpr::List(record)]));
        };

        match &step.kind {
            DoStepKind::Abstract(_) => Err(Diagnostic::error(
                step.line,
                "internal error: recursive do region contained a nested abstract declaration",
            )),
            DoStepKind::Bind { name, operation } => {
                let operation = syntax_expr_to_resolved_in_semantic_scope(
                    operation,
                    step.line,
                    self.context,
                    self.scope,
                    locals,
                )?;
                if let Some(forward_index) = unresolved_forward_index(forwards, name) {
                    let binding = resolve_forward_binding(forwards, forward_index, name, locals);
                    let continuation = self.lower_recursive_region_steps(
                        remaining_steps,
                        continuation_steps,
                        locals,
                        forwards,
                    )?;
                    return Ok(effect_call_resolved(
                        "seq",
                        [operation, ResolvedExpr::lambda(vec![binding], continuation)],
                    ));
                }

                let base_len = locals.len();
                let [binding] = locals
                    .extend_source_bindings([name.as_str()], step.line)?
                    .try_into()
                    .expect("one do binder produces one binding identity");
                let continuation = self.lower_recursive_region_steps(
                    remaining_steps,
                    continuation_steps,
                    locals,
                    forwards,
                );
                locals.truncate(base_len);
                Ok(effect_call_resolved(
                    "seq",
                    [
                        operation,
                        ResolvedExpr::lambda(vec![binding], continuation?),
                    ],
                ))
            }
            DoStepKind::ValueBind { name, value } => {
                let value = syntax_expr_to_resolved_in_semantic_scope(
                    value,
                    step.line,
                    self.context,
                    self.scope,
                    locals,
                )?;
                if let Some(forward_index) = unresolved_forward_index(forwards, name) {
                    let binding = resolve_forward_binding(forwards, forward_index, name, locals);
                    let continuation = self.lower_recursive_region_steps(
                        remaining_steps,
                        continuation_steps,
                        locals,
                        forwards,
                    )?;
                    return Ok(ResolvedExpr::apply(
                        ResolvedExpr::lambda(vec![binding], continuation),
                        [value],
                    ));
                }

                let base_len = locals.len();
                let [binding] = locals
                    .extend_source_bindings([name.as_str()], step.line)?
                    .try_into()
                    .expect("one do value binder produces one binding identity");
                let continuation = self.lower_recursive_region_steps(
                    remaining_steps,
                    continuation_steps,
                    locals,
                    forwards,
                );
                locals.truncate(base_len);
                Ok(ResolvedExpr::apply(
                    ResolvedExpr::lambda(vec![binding], continuation?),
                    [value],
                ))
            }
            DoStepKind::Then(operation) => {
                let operation = syntax_expr_to_resolved_in_semantic_scope(
                    operation,
                    step.line,
                    self.context,
                    self.scope,
                    locals,
                )?;
                let continuation = self.lower_recursive_region_steps(
                    remaining_steps,
                    continuation_steps,
                    locals,
                    forwards,
                )?;
                Ok(effect_then_resolved(operation, continuation, locals))
            }
        }
    }
}

fn unresolved_forward_index(forwards: &[ForwardBinding], name: &str) -> Option<usize> {
    let canonical = local_name_metadata(name).canonical?;
    forwards
        .iter()
        .position(|forward| forward.resolved.is_none() && forward.canonical == canonical)
}

fn resolve_forward_binding(
    forwards: &mut [ForwardBinding],
    index: usize,
    name: &str,
    locals: &mut ResolverContext,
) -> BindingId {
    let binding = locals.fresh_binding();
    let forward = &mut forwards[index];
    let mut local = local_name_metadata(name);
    debug_assert_eq!(local.canonical.as_deref(), Some(forward.canonical.as_str()));
    local.binding = Some(binding);
    locals[forward.slot] = local;
    forward.resolved = Some(binding);
    binding
}

fn list_at_resolved(index: usize, list: ResolvedExpr<Value>) -> ResolvedExpr<Value> {
    apply_builtin_resolved(
        Builtin::ListAt,
        [
            ResolvedExpr::Embedded(Value::Number(Number::from_usize(index))),
            list,
        ],
    )
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
