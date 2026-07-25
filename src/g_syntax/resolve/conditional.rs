//! Internal conditional choice resolution and effect lowering.
//!
//! Public conditional syntax is introduced in later phases. This module owns
//! the shared flat search shape first, so syntax parsing never has to invent a
//! do block or duplicate pattern lowering.

use super::super::*;
use super::effect_steps::{ResolvedEffectStep, ResolvedPatternInput, emit_effect_steps};
use super::pattern::{PatternLoweringContext, PatternStepSink, append_guard_steps};

pub(super) struct GuardChoiceArm<'a> {
    pub(super) line: usize,
    pub(super) guards: &'a [SyntaxGuardClause],
    pub(super) result_line: usize,
    pub(super) result: &'a SyntaxExpr,
}

struct ResolvedGuardChoice {
    alternatives: Vec<ResolvedGuardAlternative>,
}

struct ResolvedGuardAlternative {
    steps: Vec<ResolvedEffectStep>,
    result: ResolvedExpr<Value>,
}

struct ConditionalPatternStepSink<'a> {
    steps: &'a mut Vec<ResolvedEffectStep>,
    locals: &'a mut ResolverContext,
}

impl PatternStepSink for ConditionalPatternStepSink<'_> {
    fn locals(&mut self) -> &mut ResolverContext {
        self.locals
    }

    fn push_step(&mut self, step: ResolvedEffectStep) {
        self.steps.push(step);
    }

    fn push_capture(
        &mut self,
        input: ResolvedPatternInput,
        name: &str,
        line: usize,
    ) -> Result<(), Diagnostic> {
        let binding = self
            .locals
            .extend_source_bindings([name], line)?
            .into_iter()
            .next()
            .expect("one conditional capture produces one binding identity");
        self.steps.push(input.into_step(line, binding));
        Ok(())
    }
}

pub(super) fn lower_guard_choices_resolved(
    alternatives: &[GuardChoiceArm<'_>],
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    Ok(resolve_guard_choice(alternatives, context, scope, locals)?.emit())
}

pub(super) fn lower_if_expr_resolved(
    if_expr: &IfExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let alternatives = [
        GuardChoiceArm {
            line,
            guards: &if_expr.guards,
            result_line: line,
            result: &if_expr.then_result,
        },
        GuardChoiceArm {
            line,
            guards: &[],
            result_line: line,
            result: &if_expr.else_result,
        },
    ];
    let search = lower_guard_choices_resolved(&alternatives, context, scope, locals)?;
    Ok(compiler_values::run_pure_conditional_resolved(search))
}

fn resolve_guard_choice(
    alternatives: &[GuardChoiceArm<'_>],
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedGuardChoice, Diagnostic> {
    let base_len = locals.len();
    let mut resolved = Vec::with_capacity(alternatives.len());

    for alternative in alternatives {
        let branch = (|| {
            let mut steps = Vec::with_capacity(alternative.guards.len());
            {
                let mut sink = ConditionalPatternStepSink {
                    steps: &mut steps,
                    locals,
                };
                let mut lowering = PatternLoweringContext::new(context, scope, &mut sink);
                append_guard_steps(alternative.guards, alternative.line, &mut lowering)?;
            }
            let result = syntax_expr_to_resolved_in_semantic_scope(
                alternative.result,
                alternative.result_line,
                context,
                scope,
                locals,
            )?;
            Ok(ResolvedGuardAlternative { steps, result })
        })();
        locals.truncate(base_len);
        resolved.push(branch?);
    }

    Ok(ResolvedGuardChoice {
        alternatives: resolved,
    })
}

impl ResolvedGuardChoice {
    fn emit(self) -> ResolvedExpr<Value> {
        let mut alternatives = self
            .alternatives
            .into_iter()
            .map(ResolvedGuardAlternative::emit)
            .rev();
        let mut search = alternatives
            .next()
            .unwrap_or_else(|| lower_effect_expr_resolved("fail"));
        for alternative in alternatives {
            search = effect_call_resolved("alt", [alternative, search]);
        }
        effect_call_resolved("cut", [search])
    }
}

impl ResolvedGuardAlternative {
    fn emit(self) -> ResolvedExpr<Value> {
        let returned = effect_call_resolved("r", [self.result]);
        emit_effect_steps(self.steps, returned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::g_syntax::resolve::effect_steps::ResolvedEffectStepKind;
    use crate::number::Number;

    fn resolve(alternatives: &[GuardChoiceArm<'_>]) -> ResolvedExpr<Value> {
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        lower_guard_choices_resolved(
            alternatives,
            &context,
            &scope.resolved(),
            &mut ResolverContext::default(),
        )
        .expect("guard choice should resolve")
    }

    fn pass(result: &SyntaxExpr) -> GuardChoiceArm<'_> {
        GuardChoiceArm {
            line: 1,
            guards: &[],
            result_line: 1,
            result,
        }
    }

    fn number(value: i64) -> SyntaxExpr {
        SyntaxExpr::Number(value.into())
    }

    fn returned(value: i64) -> ResolvedExpr<Value> {
        effect_call_resolved(
            "r",
            [ResolvedExpr::Embedded(Value::Number(Number::from(value)))],
        )
    }

    #[test]
    fn zero_alternatives_lower_to_one_cut_around_failure() {
        assert_eq!(
            resolve(&[]),
            effect_call_resolved("cut", [lower_effect_expr_resolved("fail")])
        );
    }

    #[test]
    fn one_alternative_has_one_cut_and_no_redundant_alt() {
        let result = number(1);
        assert_eq!(
            resolve(&[pass(&result)]),
            effect_call_resolved("cut", [returned(1)])
        );
    }

    #[test]
    fn pass_guard_adds_no_semantic_step() {
        let guards = [SyntaxGuardClause::Pass];
        let result = number(1);
        assert_eq!(
            resolve(&[GuardChoiceArm {
                line: 1,
                guards: &guards,
                result_line: 1,
                result: &result,
            }]),
            resolve(&[pass(&result)])
        );
    }

    #[test]
    fn alternatives_are_ordered_and_right_associated() {
        let first = number(1);
        let second = number(2);
        let third = number(3);
        let expected = effect_call_resolved(
            "cut",
            [effect_call_resolved(
                "alt",
                [
                    returned(1),
                    effect_call_resolved("alt", [returned(2), returned(3)]),
                ],
            )],
        );

        assert_eq!(
            resolve(&[pass(&first), pass(&second), pass(&third)]),
            expected
        );
    }

    #[test]
    fn direct_value_guards_bind_without_a_return_effect() {
        let guards = [SyntaxGuardClause::ValueBind {
            pattern: SyntaxPattern::capture("value"),
            value: number(42),
        }];
        let result = SyntaxExpr::Name("value".to_owned());
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        let mut locals = ResolverContext::default();
        let resolved = resolve_guard_choice(
            &[GuardChoiceArm {
                line: 2,
                guards: &guards,
                result_line: 3,
                result: &result,
            }],
            &context,
            &scope.resolved(),
            &mut locals,
        )
        .expect("direct value guard should resolve");

        let [alternative] = resolved.alternatives.as_slice() else {
            panic!("expected one alternative");
        };
        let [
            ResolvedEffectStep {
                kind: ResolvedEffectStepKind::ValueBind { value, binding },
                ..
            },
        ] = alternative.steps.as_slice()
        else {
            panic!("expected one direct value binding");
        };
        assert!(
            matches!(value, ResolvedExpr::Embedded(Value::Number(number))
                if *number == Number::from(42_i64))
        );
        assert_eq!(&alternative.result, &ResolvedExpr::Local(*binding));
        assert!(locals.is_empty());
    }

    #[test]
    fn effect_bind_guards_sequence_their_operation() {
        let operation = SyntaxExpr::Apply(
            Box::new(SyntaxExpr::Effect(vec!["r".to_owned()])),
            Box::new(number(42)),
        );
        let guards = [SyntaxGuardClause::EffectBind {
            pattern: SyntaxPattern::capture("value"),
            operation,
        }];
        let result = SyntaxExpr::Name("value".to_owned());
        let resolved = resolve(&[GuardChoiceArm {
            line: 2,
            guards: &guards,
            result_line: 3,
            result: &result,
        }]);

        assert!(contains_effect(&resolved, "seq"));
    }

    #[test]
    fn sibling_captures_have_isolated_scope_and_distinct_bindings() {
        let first_guards = [SyntaxGuardClause::ValueBind {
            pattern: SyntaxPattern::capture("value"),
            value: number(1),
        }];
        let second_guards = [SyntaxGuardClause::ValueBind {
            pattern: SyntaxPattern::capture("value"),
            value: number(2),
        }];
        let first_result = SyntaxExpr::Name("value".to_owned());
        let second_result = SyntaxExpr::Name("value".to_owned());
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        let mut locals = ResolverContext::default();
        let resolved = resolve_guard_choice(
            &[
                GuardChoiceArm {
                    line: 2,
                    guards: &first_guards,
                    result_line: 3,
                    result: &first_result,
                },
                GuardChoiceArm {
                    line: 4,
                    guards: &second_guards,
                    result_line: 5,
                    result: &second_result,
                },
            ],
            &context,
            &scope.resolved(),
            &mut locals,
        )
        .expect("sibling branch captures should resolve independently");

        let [first, second] = resolved.alternatives.as_slice() else {
            panic!("expected two alternatives");
        };
        let first_binding = value_binding(first);
        let second_binding = value_binding(second);
        assert_ne!(first_binding, second_binding);
        assert_eq!(first.result, ResolvedExpr::Local(first_binding));
        assert_eq!(second.result, ResolvedExpr::Local(second_binding));
        assert!(locals.is_empty());
    }

    #[test]
    fn prefix_if_resolves_each_owned_expression_once() {
        let if_expr = IfExpr {
            guards: vec![SyntaxGuardClause::ValueBind {
                pattern: SyntaxPattern::wildcard(),
                value: number(73),
            }],
            then_result: Box::new(number(1)),
            else_result: Box::new(number(2)),
        };
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        let resolved = lower_if_expr_resolved(
            &if_expr,
            1,
            &context,
            &scope.resolved(),
            &mut ResolverContext::default(),
        )
        .expect("prefix if should resolve");

        assert_eq!(count_embedded_number(&resolved, 73), 1);
        assert_eq!(count_embedded_number(&resolved, 1), 1);
        assert_eq!(count_embedded_number(&resolved, 2), 1);
    }

    fn value_binding(alternative: &ResolvedGuardAlternative) -> BindingId {
        let [
            ResolvedEffectStep {
                kind: ResolvedEffectStepKind::ValueBind { binding, .. },
                ..
            },
        ] = alternative.steps.as_slice()
        else {
            panic!("expected one value binding");
        };
        *binding
    }

    fn contains_effect(expression: &ResolvedExpr<Value>, name: &str) -> bool {
        let target = compiler_values::effect_value(name);
        match expression {
            ResolvedExpr::Embedded(value) | ResolvedExpr::Provided(value) => value == &target,
            ResolvedExpr::Local(_) => false,
            ResolvedExpr::List(items) => items.iter().any(|item| contains_effect(item, name)),
            ResolvedExpr::Access { base, path } => {
                contains_effect(base, name)
                    || path.iter().any(|part| match part {
                        ResolvedPathPart::Key(_) => false,
                        ResolvedPathPart::Index(expression)
                        | ResolvedPathPart::PathIndex(expression) => {
                            contains_effect(expression, name)
                        }
                    })
            }
            ResolvedExpr::Lambda { body, .. } => contains_effect(body, name),
            ResolvedExpr::Apply {
                function,
                arguments,
            } => {
                contains_effect(function, name)
                    || arguments
                        .iter()
                        .any(|argument| contains_effect(argument, name))
            }
            ResolvedExpr::ApplyLambda {
                body, arguments, ..
            } => {
                contains_effect(body, name)
                    || arguments
                        .iter()
                        .any(|argument| contains_effect(argument, name))
            }
        }
    }

    fn count_embedded_number(expression: &ResolvedExpr<Value>, expected: i64) -> usize {
        match expression {
            ResolvedExpr::Embedded(Value::Number(number))
            | ResolvedExpr::Provided(Value::Number(number)) => {
                usize::from(number == &Number::from(expected))
            }
            ResolvedExpr::Embedded(_) | ResolvedExpr::Provided(_) | ResolvedExpr::Local(_) => 0,
            ResolvedExpr::List(items) => items
                .iter()
                .map(|item| count_embedded_number(item, expected))
                .sum(),
            ResolvedExpr::Access { base, path } => {
                count_embedded_number(base, expected)
                    + path
                        .iter()
                        .map(|part| match part {
                            ResolvedPathPart::Key(_) => 0,
                            ResolvedPathPart::Index(expression)
                            | ResolvedPathPart::PathIndex(expression) => {
                                count_embedded_number(expression, expected)
                            }
                        })
                        .sum::<usize>()
            }
            ResolvedExpr::Lambda { body, .. } => count_embedded_number(body, expected),
            ResolvedExpr::Apply {
                function,
                arguments,
            } => {
                count_embedded_number(function, expected)
                    + arguments
                        .iter()
                        .map(|argument| count_embedded_number(argument, expected))
                        .sum::<usize>()
            }
            ResolvedExpr::ApplyLambda {
                body, arguments, ..
            } => {
                count_embedded_number(body, expected)
                    + arguments
                        .iter()
                        .map(|argument| count_embedded_number(argument, expected))
                        .sum::<usize>()
            }
        }
    }
}
