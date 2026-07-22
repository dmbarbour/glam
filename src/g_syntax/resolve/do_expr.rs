use super::super::recursive_do::{ForwardNameId, RecursiveDoPlan};
use super::super::*;
use crate::number::Number;

#[derive(Default)]
struct ResolvedForward {
    resolver_slot: Option<usize>,
    forward_binding: Option<BindingId>,
    resolved_binding: Option<BindingId>,
    future_binding: Option<BindingId>,
    fixed_result_binding: Option<BindingId>,
    continuation_parameter: Option<BindingId>,
}

enum ResolvedDoStep {
    Abstract,
    Bind {
        operation: ResolvedExpr<Value>,
        binding: BindingId,
    },
    ValueBind {
        value: ResolvedExpr<Value>,
        binding: BindingId,
    },
    Then {
        operation: ResolvedExpr<Value>,
        result: BindingId,
    },
}

struct ResolvedDoBlock {
    steps: Vec<ResolvedDoStep>,
    result: ResolvedExpr<Value>,
    forwards: Vec<ResolvedForward>,
}

struct DoLowering<'a> {
    context: &'a CompileContext,
    scope: &'a NameScope<ResolvedRoot>,
    plan: &'a RecursiveDoPlan,
}

struct DoEmitter<'a> {
    steps: &'a mut [Option<ResolvedDoStep>],
    forwards: &'a [ResolvedForward],
    plan: &'a RecursiveDoPlan,
    context: &'a CompileContext,
}

pub(in crate::g_syntax) fn lower_do_expr_resolved(
    do_expr: &DoExpr,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let plan = RecursiveDoPlan::build(do_expr)?;
    let lowering = DoLowering {
        context,
        scope,
        plan: &plan,
    };
    let block = lowering.resolve(do_expr, locals)?;
    Ok(lowering.emit(block))
}

impl DoLowering<'_> {
    fn resolve(
        &self,
        do_expr: &DoExpr,
        locals: &mut ResolverContext,
    ) -> Result<ResolvedDoBlock, Diagnostic> {
        let base_len = locals.len();
        let result = (|| {
            let mut forwards = (0..self.plan.forwards.len())
                .map(|_| ResolvedForward::default())
                .collect::<Vec<_>>();
            let mut steps = Vec::with_capacity(do_expr.steps.len());

            for (step_index, step) in do_expr.steps.iter().enumerate() {
                let resolved = match &step.kind {
                    DoStepKind::Abstract(names) => {
                        let ids = &self.plan.declarations_at[step_index];
                        debug_assert_eq!(ids.len(), names.len());
                        let first_slot = locals.len();
                        let bindings = locals
                            .extend_source_bindings(names.iter().map(String::as_str), step.line)?;
                        for (offset, (id, binding)) in ids.iter().copied().zip(bindings).enumerate()
                        {
                            forwards[id].resolver_slot = Some(first_slot + offset);
                            forwards[id].forward_binding = Some(binding);
                        }
                        ResolvedDoStep::Abstract
                    }
                    DoStepKind::Bind { name, operation } => {
                        let operation = syntax_expr_to_resolved_in_semantic_scope(
                            operation,
                            step.line,
                            self.context,
                            self.scope,
                            locals,
                        )?;
                        let binding = resolve_step_binding(
                            self.plan.fulfillment_at[step_index],
                            name,
                            step.line,
                            locals,
                            &mut forwards,
                        )?;
                        ResolvedDoStep::Bind { operation, binding }
                    }
                    DoStepKind::ValueBind { name, value } => {
                        let value = syntax_expr_to_resolved_in_semantic_scope(
                            value,
                            step.line,
                            self.context,
                            self.scope,
                            locals,
                        )?;
                        let binding = resolve_step_binding(
                            self.plan.fulfillment_at[step_index],
                            name,
                            step.line,
                            locals,
                            &mut forwards,
                        )?;
                        ResolvedDoStep::ValueBind { value, binding }
                    }
                    DoStepKind::Then(operation) => {
                        let operation = syntax_expr_to_resolved_in_semantic_scope(
                            operation,
                            step.line,
                            self.context,
                            self.scope,
                            locals,
                        )?;
                        ResolvedDoStep::Then {
                            operation,
                            result: locals.fresh_binding(),
                        }
                    }
                };
                steps.push(resolved);
            }

            let result = syntax_expr_to_resolved_in_semantic_scope(
                &do_expr.result,
                do_expr.result_line,
                self.context,
                self.scope,
                locals,
            )?;
            for forward in &mut forwards {
                forward.future_binding = Some(locals.fresh_binding());
                forward.fixed_result_binding = Some(locals.fresh_binding());
                forward.continuation_parameter = Some(locals.fresh_binding());
                debug_assert!(forward.forward_binding.is_some());
                debug_assert!(forward.resolved_binding.is_some());
            }
            Ok(ResolvedDoBlock {
                steps,
                result,
                forwards,
            })
        })();
        locals.truncate(base_len);
        result
    }

    fn emit(&self, block: ResolvedDoBlock) -> ResolvedExpr<Value> {
        let ResolvedDoBlock {
            steps,
            result,
            forwards,
        } = block;
        let mut steps = steps.into_iter().map(Some).collect::<Vec<_>>();
        let end = steps.len();
        let roots = self.plan.roots.clone();
        let emitted = DoEmitter {
            steps: &mut steps,
            forwards: &forwards,
            plan: self.plan,
            context: self.context,
        }
        .emit_range(0, end, &roots, result);
        debug_assert!(steps.iter().all(Option::is_none));
        emitted
    }
}

fn resolve_step_binding(
    fulfillment: Option<ForwardNameId>,
    name: &str,
    line: usize,
    locals: &mut ResolverContext,
    forwards: &mut [ResolvedForward],
) -> Result<BindingId, Diagnostic> {
    let Some(id) = fulfillment else {
        return Ok(locals
            .extend_source_bindings([name], line)?
            .into_iter()
            .next()
            .expect("one do binder produces one binding identity"));
    };

    let binding = locals.fresh_binding();
    let forward = &mut forwards[id];
    let slot = forward
        .resolver_slot
        .expect("planned fulfillment follows its abstract declaration");
    let mut local = local_name_metadata(name);
    local.binding = Some(binding);
    locals[slot] = local;
    forward.resolved_binding = Some(binding);
    Ok(binding)
}

impl DoEmitter<'_> {
    fn emit_range(
        &mut self,
        start: usize,
        end: usize,
        scopes: &[ForwardNameId],
        mut continuation: ResolvedExpr<Value>,
    ) -> ResolvedExpr<Value> {
        let mut cursor = end;
        for id in scopes.iter().rev().copied() {
            let scope = &self.plan.forwards[id];
            let scope_start = scope.semantic_start;
            let scope_end = scope.fulfillment_step;
            debug_assert!(start <= scope_start);
            debug_assert!(scope_end < cursor);
            continuation = self.emit_plain_range(scope_end + 1, cursor, continuation);
            continuation = self.emit_fix_scope(id, continuation);
            cursor = scope_start;
        }
        self.emit_plain_range(start, cursor, continuation)
    }

    fn emit_plain_range(
        &mut self,
        start: usize,
        end: usize,
        mut continuation: ResolvedExpr<Value>,
    ) -> ResolvedExpr<Value> {
        for index in (start..end).rev() {
            let step = self.steps[index]
                .take()
                .expect("planned recursive-do step is emitted exactly once");
            continuation = match step {
                ResolvedDoStep::Abstract => continuation,
                ResolvedDoStep::Bind { operation, binding } => effect_call_resolved(
                    "seq",
                    [operation, ResolvedExpr::lambda(vec![binding], continuation)],
                ),
                ResolvedDoStep::ValueBind { value, binding } => {
                    ResolvedExpr::apply(ResolvedExpr::lambda(vec![binding], continuation), [value])
                }
                ResolvedDoStep::Then { operation, result } => {
                    let body =
                        annotate_assert_unit_resolved(ResolvedExpr::Local(result), continuation);
                    effect_call_resolved(
                        "seq",
                        [operation, ResolvedExpr::lambda(vec![result], body)],
                    )
                }
            };
        }
        continuation
    }

    fn emit_fix_scope(
        &mut self,
        id: ForwardNameId,
        after: ResolvedExpr<Value>,
    ) -> ResolvedExpr<Value> {
        let scope = &self.plan.forwards[id];
        let scope_start = scope.semantic_start;
        let scope_end = scope.fulfillment_step;
        let children = scope.children.clone();
        let resolved = &self.forwards[id];
        let forward_binding = resolved
            .forward_binding
            .expect("planned abstract name has a forward binding");
        let resolved_binding = resolved
            .resolved_binding
            .expect("planned abstract name has a resolved binding");
        let future_binding = resolved
            .future_binding
            .expect("planned abstract name has a future binding");
        let fixed_result_binding = resolved
            .fixed_result_binding
            .expect("planned abstract name has a fixed-result binding");
        let continuation_parameter = resolved
            .continuation_parameter
            .expect("planned abstract name has a continuation parameter");

        let payload = effect_call_resolved(
            "r",
            [ResolvedExpr::List(vec![
                ResolvedExpr::Local(resolved_binding),
                ResolvedExpr::lambda(vec![continuation_parameter], after),
            ])],
        );
        let body = self.emit_range(scope_start, scope_end + 1, &children, payload);
        let body = ResolvedExpr::apply(
            ResolvedExpr::lambda(vec![forward_binding], body),
            [list_at_resolved(0, ResolvedExpr::Local(future_binding))],
        );
        let fixed = effect_call_resolved("fix", [ResolvedExpr::lambda(vec![future_binding], body)]);
        let continuation = list_at_resolved(1, ResolvedExpr::Local(fixed_result_binding));
        let resumed = ResolvedExpr::apply(
            continuation,
            [ResolvedExpr::Embedded(self.context.unit_value())],
        );
        effect_call_resolved(
            "seq",
            [
                fixed,
                ResolvedExpr::lambda(vec![fixed_result_binding], resumed),
            ],
        )
    }
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

    fn count_embedded_value(expr: &ResolvedExpr<Value>, target: &Value) -> usize {
        let own = usize::from(matches!(expr, ResolvedExpr::Embedded(value) if value == target));
        own + match expr {
            ResolvedExpr::Embedded(_) | ResolvedExpr::Provided(_) | ResolvedExpr::Local(_) => 0,
            ResolvedExpr::List(items) => items
                .iter()
                .map(|item| count_embedded_value(item, target))
                .sum(),
            ResolvedExpr::Access { base, path } => {
                count_embedded_value(base, target)
                    + path
                        .iter()
                        .map(|part| match part {
                            ResolvedPathPart::Key(_) => 0,
                            ResolvedPathPart::Index(expr) | ResolvedPathPart::PathIndex(expr) => {
                                count_embedded_value(expr, target)
                            }
                        })
                        .sum::<usize>()
            }
            ResolvedExpr::Lambda { body, .. } => count_embedded_value(body, target),
            ResolvedExpr::Apply {
                function,
                arguments,
            } => {
                count_embedded_value(function, target)
                    + arguments
                        .iter()
                        .map(|argument| count_embedded_value(argument, target))
                        .sum::<usize>()
            }
            ResolvedExpr::ApplyLambda {
                body, arguments, ..
            } => {
                count_embedded_value(body, target)
                    + arguments
                        .iter()
                        .map(|argument| count_embedded_value(argument, target))
                        .sum::<usize>()
            }
        }
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

    #[test]
    fn every_abstract_name_lowers_to_an_independent_fix_request() {
        let resolved = resolve(&SyntaxExpr::Do(DoExpr {
            steps: vec![
                DoStep {
                    line: 2,
                    kind: DoStepKind::Abstract(vec![
                        "x".to_owned(),
                        "y".to_owned(),
                        "z".to_owned(),
                    ]),
                },
                DoStep {
                    line: 3,
                    kind: DoStepKind::ValueBind {
                        name: "y".to_owned(),
                        value: SyntaxExpr::Unit,
                    },
                },
                DoStep {
                    line: 4,
                    kind: DoStepKind::ValueBind {
                        name: "x".to_owned(),
                        value: SyntaxExpr::Unit,
                    },
                },
                DoStep {
                    line: 5,
                    kind: DoStepKind::ValueBind {
                        name: "z".to_owned(),
                        value: SyntaxExpr::Unit,
                    },
                },
            ],
            result_line: 6,
            result: Box::new(SyntaxExpr::Unit),
        }));

        assert_eq!(
            count_embedded_value(
                &resolved,
                &crate::g_syntax::compiler_values::effect_value("fix")
            ),
            3
        );
    }
}
