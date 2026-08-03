use super::super::recursive_do::{
    ForwardNameId, ForwardNameRegistry, RecursiveDoEvent, RecursiveDoPlan, RecursiveDoStep,
};
use super::super::*;
use super::effect_steps::{
    ResolvedEffectStep, ResolvedEffectStepKind, ResolvedPatternInput, emit_effect_steps,
};
use super::pattern::{PatternLoweringContext, PatternStepSink, append_pattern_steps};
use crate::number::Number;

#[derive(Default)]
pub(super) struct ResolvedForward {
    pub(super) resolver_slot: Option<usize>,
    forward_binding: Option<BindingId>,
    pub(super) resolved_binding: Option<BindingId>,
    future_binding: Option<BindingId>,
    fixed_result_binding: Option<BindingId>,
    continuation_parameter: Option<BindingId>,
}

/// A resolved effect step decorated only with recursive-do planning metadata.
///
/// Pattern lowering emits syntax-independent `ResolvedEffectStep`s through a
/// do-specific sink; it never acquires recursive-do state itself.
struct RecursiveEffectStep {
    recursion: RecursiveDoStep,
    effect: Option<ResolvedEffectStep>,
}

struct ResolvedDoBlock {
    steps: Vec<RecursiveEffectStep>,
    result: ResolvedExpr<Value>,
    forwards: Vec<ResolvedForward>,
    plan: RecursiveDoPlan,
}

struct DoLowering<'a> {
    context: &'a CompileContext,
    scope: &'a NameScope<ResolvedRoot>,
}

struct DoEmitter<'a> {
    steps: &'a mut [Option<RecursiveEffectStep>],
    forwards: &'a [ResolvedForward],
    plan: &'a RecursiveDoPlan,
    context: &'a CompileContext,
}

struct DoPatternStepSink<'a> {
    steps: &'a mut Vec<RecursiveEffectStep>,
    forward_names: &'a mut ForwardNameRegistry,
    locals: &'a mut ResolverContext,
    forwards: &'a mut [ResolvedForward],
}

impl PatternStepSink for DoPatternStepSink<'_> {
    fn locals(&mut self) -> &mut ResolverContext {
        self.locals
    }

    fn push_step(&mut self, step: ResolvedEffectStep) {
        let line = step.line;
        push_recursive_step(self.steps, line, Some(step), RecursiveDoEvent::None);
    }

    fn push_capture(
        &mut self,
        input: ResolvedPatternInput,
        name: &str,
        line: usize,
    ) -> Result<(), Diagnostic> {
        let (binding, event) =
            resolve_capture_binding(self.forward_names, name, line, self.locals, self.forwards)?;
        push_recursive_step(
            self.steps,
            line,
            Some(input.into_step(line, binding)),
            event,
        );
        Ok(())
    }
}

pub(in crate::g_syntax) fn lower_do_expr_resolved(
    do_expr: &DoExpr,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let lowering = DoLowering { context, scope };
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
            let mut forward_names = ForwardNameRegistry::default();
            let mut forwards = Vec::new();
            let mut steps = Vec::with_capacity(do_expr.steps.len());

            for step in &do_expr.steps {
                match &step.kind {
                    DoStepKind::Abstract(names) => {
                        let ids = forward_names.declare(names, step.line)?;
                        debug_assert_eq!(ids.len(), names.len());
                        let first_slot = locals.len();
                        let bindings = locals
                            .extend_source_bindings(names.iter().map(String::as_str), step.line)?;
                        for (offset, (id, binding)) in ids.iter().copied().zip(bindings).enumerate()
                        {
                            debug_assert_eq!(
                                id,
                                forwards.len(),
                                "recursive-do IDs follow declaration order"
                            );
                            forwards.push(ResolvedForward {
                                resolver_slot: Some(first_slot + offset),
                                forward_binding: Some(binding),
                                ..ResolvedForward::default()
                            });
                        }
                        push_recursive_step(
                            &mut steps,
                            step.line,
                            None,
                            RecursiveDoEvent::Declare(ids),
                        );
                    }
                    DoStepKind::Bind { pattern, operation } => {
                        let operation = syntax_expr_to_resolved_in_semantic_scope(
                            operation,
                            step.line,
                            self.context,
                            self.scope,
                            locals,
                        )?;
                        let mut sink = DoPatternStepSink {
                            steps: &mut steps,
                            forward_names: &mut forward_names,
                            locals,
                            forwards: &mut forwards,
                        };
                        let mut pattern_lowering =
                            PatternLoweringContext::new(self.context, self.scope, &mut sink);
                        append_pattern_steps(
                            ResolvedPatternInput::Effect(operation),
                            pattern,
                            step.line,
                            &mut pattern_lowering,
                        )?;
                    }
                    DoStepKind::ValueBind { pattern, value } => {
                        let value = syntax_expr_to_resolved_in_semantic_scope(
                            value,
                            step.line,
                            self.context,
                            self.scope,
                            locals,
                        )?;
                        let mut sink = DoPatternStepSink {
                            steps: &mut steps,
                            forward_names: &mut forward_names,
                            locals,
                            forwards: &mut forwards,
                        };
                        let mut pattern_lowering =
                            PatternLoweringContext::new(self.context, self.scope, &mut sink);
                        append_pattern_steps(
                            ResolvedPatternInput::Value(value),
                            pattern,
                            step.line,
                            &mut pattern_lowering,
                        )?;
                    }
                    DoStepKind::Then(operation) => {
                        let operation = syntax_expr_to_resolved_in_semantic_scope(
                            operation,
                            step.line,
                            self.context,
                            self.scope,
                            locals,
                        )?;
                        let result = locals.fresh_binding();
                        push_recursive_step(
                            &mut steps,
                            step.line,
                            Some(ResolvedEffectStep {
                                line: step.line,
                                kind: ResolvedEffectStepKind::Then {
                                    operation,
                                    result,
                                    diagnostic_context: "effect-only `do` statement",
                                },
                            }),
                            RecursiveDoEvent::None,
                        );
                    }
                }
            }

            let result = syntax_expr_to_resolved_in_semantic_scope(
                &do_expr.result,
                do_expr.result_line,
                self.context,
                self.scope,
                locals,
            )?;
            let plan = RecursiveDoPlan::build(
                steps.iter().map(|step| &step.recursion),
                forward_names.into_forwards(),
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
                plan,
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
            plan,
        } = block;
        let mut steps = steps.into_iter().map(Some).collect::<Vec<_>>();
        let end = steps.len();
        let roots = plan.roots.clone();
        let emitted = DoEmitter {
            steps: &mut steps,
            forwards: &forwards,
            plan: &plan,
            context: self.context,
        }
        .emit_range(0, end, &roots, result);
        debug_assert!(steps.iter().all(Option::is_none));
        emitted
    }
}

fn push_recursive_step(
    steps: &mut Vec<RecursiveEffectStep>,
    line: usize,
    effect: Option<ResolvedEffectStep>,
    event: RecursiveDoEvent,
) {
    debug_assert!(effect.as_ref().is_none_or(|effect| effect.line == line));
    steps.push(RecursiveEffectStep {
        recursion: RecursiveDoStep { line, event },
        effect,
    });
}

fn resolve_capture_binding(
    forward_names: &mut ForwardNameRegistry,
    name: &str,
    line: usize,
    locals: &mut ResolverContext,
    forwards: &mut [ResolvedForward],
) -> Result<(BindingId, RecursiveDoEvent), Diagnostic> {
    let fulfillment = forward_names.fulfill(name);
    let Some(id) = fulfillment else {
        let binding = locals
            .extend_source_bindings([name], line)?
            .into_iter()
            .next()
            .expect("one pattern capture produces one binding identity");
        return Ok((binding, RecursiveDoEvent::None));
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
    Ok((binding, RecursiveDoEvent::Fulfill(id)))
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
        continuation: ResolvedExpr<Value>,
    ) -> ResolvedExpr<Value> {
        let steps = &mut self.steps;
        let effects = (start..end).filter_map(|index| {
            steps[index]
                .take()
                .expect("planned recursive-do step is emitted exactly once")
                .effect
        });
        emit_effect_steps(self.context.values(), effects, continuation)
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
            self.context.values(),
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
        let fixed = effect_call_resolved(
            self.context.values(),
            "fix",
            [ResolvedExpr::lambda(vec![future_binding], body)],
        );
        let continuation = list_at_resolved(1, ResolvedExpr::Local(fixed_result_binding));
        let resumed = ResolvedExpr::apply(
            continuation,
            [ResolvedExpr::Embedded(self.context.unit_value())],
        );
        effect_call_resolved(
            self.context.values(),
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

    fn as_pattern(left: SyntaxPattern, right: SyntaxPattern) -> SyntaxPattern {
        SyntaxPattern {
            kind: SyntaxPatternKind::As(Box::new(left), Box::new(right)),
        }
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
    fn embedded_semantic_data_lowers_without_reconstruction() {
        let value = Value::Number(Number::from(42_i64));
        let resolved = resolve(&SyntaxExpr::Embedded(value.clone()));

        assert_eq!(resolved, ResolvedExpr::Embedded(value));
    }

    #[test]
    fn irrefutable_value_binding_uses_fused_lambda_application() {
        let resolved = resolve(&SyntaxExpr::Do(DoExpr {
            steps: vec![DoStep {
                line: 2,
                kind: DoStepKind::ValueBind {
                    pattern: SyntaxPattern::capture("value"),
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
    fn grouped_capture_preserves_single_name_lowering() {
        let resolved = resolve(&SyntaxExpr::Do(DoExpr {
            steps: vec![DoStep {
                line: 2,
                kind: DoStepKind::ValueBind {
                    pattern: SyntaxPattern {
                        kind: SyntaxPatternKind::Group(Box::new(SyntaxPattern::capture("value"))),
                    },
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
    fn refutable_value_binding_does_not_synthesize_a_return_effect() {
        let value_pattern = SyntaxPattern {
            kind: SyntaxPatternKind::Literal(SyntaxPatternLiteral::Number(Number::from(42_i64))),
        };
        let resolved = resolve(&SyntaxExpr::Do(DoExpr {
            steps: vec![DoStep {
                line: 2,
                kind: DoStepKind::ValueBind {
                    pattern: value_pattern,
                    value: SyntaxExpr::Number(42.into()),
                },
            }],
            result_line: 3,
            result: Box::new(SyntaxExpr::Unit),
        }));

        assert_eq!(
            count_embedded_value(
                &resolved,
                &crate::g_syntax::compiler_values::effect_value(
                    &crate::core::test_value_factory(),
                    "r",
                )
            ),
            0
        );
        assert_eq!(
            count_embedded_value(&resolved, &Value::Builtin(Builtin::PatternEqual)),
            1
        );
    }

    #[test]
    fn wildcard_guard_emits_no_effect_step() {
        fn expression(pattern: SyntaxPattern) -> SyntaxExpr {
            SyntaxExpr::Do(DoExpr {
                steps: vec![DoStep {
                    line: 2,
                    kind: DoStepKind::ValueBind {
                        pattern,
                        value: SyntaxExpr::Number(42.into()),
                    },
                }],
                result_line: 3,
                result: Box::new(SyntaxExpr::Unit),
            })
        }

        let guarded = SyntaxPattern {
            kind: SyntaxPatternKind::Guarded {
                pattern: Box::new(SyntaxPattern::wildcard()),
                guards: vec![SyntaxGuardClause::Pass],
            },
        };

        assert_eq!(
            resolve(&expression(guarded)),
            resolve(&expression(SyntaxPattern::wildcard()))
        );
    }

    #[test]
    fn wildcard_uses_an_unspellable_continuation_parameter() {
        let resolved = resolve(&SyntaxExpr::Do(DoExpr {
            steps: vec![DoStep {
                line: 2,
                kind: DoStepKind::ValueBind {
                    pattern: SyntaxPattern::wildcard(),
                    value: SyntaxExpr::Number(42.into()),
                },
            }],
            result_line: 3,
            result: Box::new(SyntaxExpr::Unit),
        }));

        assert!(matches!(
            resolved,
            ResolvedExpr::ApplyLambda {
                parameters,
                body,
                arguments,
            } if parameters.len() == 1
                && matches!(body.as_ref(), ResolvedExpr::Embedded(value)
                    if *value == *crate::core::keys::UNIT_VALUE)
                && matches!(arguments.as_slice(),
                    [ResolvedExpr::Embedded(Value::Number(number))]
                        if *number == Number::from(42_i64))
        ));
    }

    #[test]
    fn irrefutable_as_pattern_shares_one_resolved_subject() {
        let resolved = resolve(&SyntaxExpr::Do(DoExpr {
            steps: vec![DoStep {
                line: 2,
                kind: DoStepKind::ValueBind {
                    pattern: as_pattern(
                        SyntaxPattern::capture("left"),
                        SyntaxPattern::capture("right"),
                    ),
                    value: SyntaxExpr::Number(42.into()),
                },
            }],
            result_line: 3,
            result: Box::new(SyntaxExpr::List(vec![
                SyntaxExpr::Name("left".to_owned()),
                SyntaxExpr::Name("right".to_owned()),
            ])),
        }));

        assert_eq!(
            count_embedded_value(&resolved, &Value::Number(Number::from(42_i64))),
            1
        );
    }

    #[test]
    fn irrefutable_as_captures_fulfill_independent_recursive_regions() {
        let resolved = resolve(&SyntaxExpr::Do(DoExpr {
            steps: vec![
                DoStep {
                    line: 2,
                    kind: DoStepKind::Abstract(vec!["left".to_owned(), "right".to_owned()]),
                },
                DoStep {
                    line: 3,
                    kind: DoStepKind::ValueBind {
                        pattern: as_pattern(
                            SyntaxPattern::capture("left"),
                            SyntaxPattern::capture("right"),
                        ),
                        value: SyntaxExpr::Number(42.into()),
                    },
                },
            ],
            result_line: 4,
            result: Box::new(SyntaxExpr::Unit),
        }));

        assert_eq!(
            count_embedded_value(
                &resolved,
                &crate::g_syntax::compiler_values::effect_value(
                    &crate::core::test_value_factory(),
                    "fix",
                )
            ),
            2
        );
        assert_eq!(
            count_embedded_value(&resolved, &Value::Number(Number::from(42_i64))),
            1
        );
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
                        pattern: SyntaxPattern::capture("y"),
                        value: SyntaxExpr::Unit,
                    },
                },
                DoStep {
                    line: 4,
                    kind: DoStepKind::ValueBind {
                        pattern: SyntaxPattern::capture("x"),
                        value: SyntaxExpr::Unit,
                    },
                },
                DoStep {
                    line: 5,
                    kind: DoStepKind::ValueBind {
                        pattern: SyntaxPattern::capture("z"),
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
                &crate::g_syntax::compiler_values::effect_value(
                    &crate::core::test_value_factory(),
                    "fix",
                )
            ),
            3
        );
    }
}
