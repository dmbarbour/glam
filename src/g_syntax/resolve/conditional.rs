//! Internal conditional choice resolution and effect lowering.
//!
//! Public conditional syntax is introduced in later phases. This module owns
//! the shared flat search shape first, so syntax parsing never has to invent a
//! do block or duplicate pattern lowering.

use super::super::*;
use super::effect_steps::{ResolvedEffectStep, ResolvedPatternInput, emit_effect_steps};
use super::pattern::{
    PatternLoweringContext, PatternStepSink, append_guard_steps, append_pattern_steps,
};

pub(super) struct GuardChoiceArm<'a> {
    pub(super) line: usize,
    pub(super) guards: &'a [SyntaxGuardClause],
    pub(super) result_line: usize,
    pub(super) result_mode: ConditionalResultMode,
    pub(super) result: &'a SyntaxExpr,
}

struct ResolvedChoice {
    alternatives: Vec<ResolvedAlternative>,
}

struct ResolvedAlternative {
    steps: Vec<ResolvedEffectStep>,
    outcome: ResolvedChoiceOutcome,
}

enum ResolvedChoiceOutcome {
    Value(ResolvedExpr<Value>),
    Effect(ResolvedExpr<Value>),
    Nested(ResolvedChoice),
}

struct ChoiceArmSpec<'a> {
    pattern: Option<(BindingId, &'a SyntaxPattern)>,
    guards: &'a [SyntaxGuardClause],
    line: usize,
    result_line: usize,
    result_mode: ConditionalResultMode,
    result: &'a SyntaxExpr,
    unit_assertion_context: &'static str,
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
            result_mode: if_expr.then_mode,
            result: &if_expr.then_result,
        },
        GuardChoiceArm {
            line,
            guards: &[],
            result_line: line,
            result_mode: ConditionalResultMode::Ordinary,
            result: &if_expr.else_result,
        },
    ];
    let search = lower_guard_choices_resolved(&alternatives, context, scope, locals)?;
    Ok(match if_expr.mode {
        ConditionalMode::Pure => compiler_values::run_pure_conditional_resolved(search),
        ConditionalMode::Host => search,
    })
}

pub(super) fn lower_match_expr_resolved(
    match_expr: &MatchExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let subject = syntax_expr_to_resolved_in_semantic_scope(
        &match_expr.subject,
        line,
        context,
        scope,
        locals,
    )?;
    let base_len = locals.len();
    let subject_binding = locals.fresh_binding();
    let resolved = (|| {
        let search =
            resolve_match_choice(&match_expr.arms, subject_binding, context, scope, locals)?.emit();
        let selected = match match_expr.mode {
            ConditionalMode::Pure => compiler_values::run_pure_match_resolved(search),
            ConditionalMode::Host => search,
        };
        Ok(ResolvedExpr::apply(
            ResolvedExpr::lambda(vec![subject_binding], selected),
            [subject],
        ))
    })();
    locals.truncate(base_len);
    resolved
}

pub(super) fn lower_match_when_expr_resolved(
    match_when: &MatchWhenExpr,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let search = resolve_when_choice(&match_when.arms, context, scope, locals)?.emit();
    Ok(match match_when.mode {
        ConditionalMode::Pure => compiler_values::run_pure_match_resolved(search),
        ConditionalMode::Host => search,
    })
}

fn resolve_guard_choice(
    alternatives: &[GuardChoiceArm<'_>],
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedChoice, Diagnostic> {
    let base_len = locals.len();
    let mut resolved = Vec::with_capacity(alternatives.len());

    for alternative in alternatives {
        let branch = resolve_alternative(
            ChoiceArmSpec {
                pattern: None,
                guards: alternative.guards,
                line: alternative.line,
                result_line: alternative.result_line,
                result_mode: alternative.result_mode,
                result: alternative.result,
                unit_assertion_context: "conditional guard",
            },
            context,
            scope,
            locals,
        );
        locals.truncate(base_len);
        resolved.push(branch?);
    }

    Ok(ResolvedChoice {
        alternatives: resolved,
    })
}

fn resolve_alternative(
    arm: ChoiceArmSpec<'_>,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedAlternative, Diagnostic> {
    let steps = resolve_prefix_steps(
        arm.pattern,
        arm.guards,
        arm.line,
        arm.unit_assertion_context,
        context,
        scope,
        locals,
    )?;
    let result = syntax_expr_to_resolved_in_semantic_scope(
        arm.result,
        arm.result_line,
        context,
        scope,
        locals,
    )?;
    let outcome = match arm.result_mode {
        ConditionalResultMode::Ordinary => ResolvedChoiceOutcome::Value(result),
        ConditionalResultMode::Tentative => ResolvedChoiceOutcome::Effect(result),
    };
    Ok(ResolvedAlternative { steps, outcome })
}

fn resolve_match_choice(
    arms: &[MatchArm],
    subject: BindingId,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedChoice, Diagnostic> {
    let base_len = locals.len();
    let mut alternatives = Vec::with_capacity(arms.len());
    for arm in arms {
        let branch = resolve_match_alternative(arm, subject, context, scope, locals);
        locals.truncate(base_len);
        alternatives.push(branch?);
    }
    Ok(ResolvedChoice { alternatives })
}

fn resolve_match_alternative(
    arm: &MatchArm,
    subject: BindingId,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedAlternative, Diagnostic> {
    let steps = resolve_prefix_steps(
        Some((subject, &arm.pattern)),
        &arm.guards,
        arm.line,
        "match condition",
        context,
        scope,
        locals,
    )?;
    let outcome = resolve_match_outcome(&arm.outcome, context, scope, locals)?;
    Ok(ResolvedAlternative { steps, outcome })
}

fn resolve_when_choice(
    arms: &[WhenArm],
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedChoice, Diagnostic> {
    let base_len = locals.len();
    let mut alternatives = Vec::with_capacity(arms.len());
    for arm in arms {
        let branch = resolve_when_alternative(arm, context, scope, locals);
        locals.truncate(base_len);
        alternatives.push(branch?);
    }
    Ok(ResolvedChoice { alternatives })
}

fn resolve_when_alternative(
    arm: &WhenArm,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedAlternative, Diagnostic> {
    let steps = resolve_prefix_steps(
        None,
        &arm.guards,
        arm.line,
        "match condition",
        context,
        scope,
        locals,
    )?;
    let outcome = resolve_match_outcome(&arm.outcome, context, scope, locals)?;
    Ok(ResolvedAlternative { steps, outcome })
}

fn resolve_match_outcome(
    outcome: &MatchOutcome,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedChoiceOutcome, Diagnostic> {
    match outcome {
        MatchOutcome::Result {
            line,
            mode,
            expression,
        } => syntax_expr_to_resolved_in_semantic_scope(expression, *line, context, scope, locals)
            .map(|result| match mode {
                ConditionalResultMode::Ordinary => ResolvedChoiceOutcome::Value(result),
                ConditionalResultMode::Tentative => ResolvedChoiceOutcome::Effect(result),
            }),
        MatchOutcome::Nested(arms) => {
            resolve_when_choice(arms, context, scope, locals).map(ResolvedChoiceOutcome::Nested)
        }
    }
}

fn resolve_prefix_steps(
    pattern: Option<(BindingId, &SyntaxPattern)>,
    guards: &[SyntaxGuardClause],
    line: usize,
    unit_assertion_context: &'static str,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<Vec<ResolvedEffectStep>, Diagnostic> {
    let mut steps = Vec::with_capacity(guards.len() + usize::from(pattern.is_some()));
    let mut sink = ConditionalPatternStepSink {
        steps: &mut steps,
        locals,
    };
    let mut lowering = PatternLoweringContext::new(context, scope, &mut sink)
        .with_unit_assertion_context(unit_assertion_context);
    if let Some((subject, pattern)) = pattern {
        append_pattern_steps(
            ResolvedPatternInput::Value(ResolvedExpr::Local(subject)),
            pattern,
            line,
            &mut lowering,
        )?;
    }
    append_guard_steps(guards, line, &mut lowering)?;
    Ok(steps)
}

impl ResolvedChoice {
    fn emit(self) -> ResolvedExpr<Value> {
        effect_call_resolved("cut", [self.emit_search()])
    }

    fn emit_search(self) -> ResolvedExpr<Value> {
        let mut alternatives = self
            .alternatives
            .into_iter()
            .map(ResolvedAlternative::emit)
            .rev();
        let mut search = alternatives
            .next()
            .unwrap_or_else(|| lower_effect_expr_resolved("fail"));
        for alternative in alternatives {
            search = effect_call_resolved("alt", [alternative, search]);
        }
        search
    }
}

impl ResolvedAlternative {
    fn emit(self) -> ResolvedExpr<Value> {
        emit_effect_steps(self.steps, self.outcome.emit())
    }
}

impl ResolvedChoiceOutcome {
    fn emit(self) -> ResolvedExpr<Value> {
        match self {
            Self::Value(result) => effect_call_resolved("r", [result]),
            Self::Effect(result) => result,
            Self::Nested(choice) => choice.emit_search(),
        }
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
            result_mode: ConditionalResultMode::Ordinary,
            result,
        }
    }

    fn number(value: i64) -> SyntaxExpr {
        SyntaxExpr::Number(value.into())
    }

    fn is_root_effect_call(expression: &ResolvedExpr<Value>, name: &str) -> bool {
        matches!(
            expression,
            ResolvedExpr::Apply { function, .. }
                if function.as_ref()
                    == &ResolvedExpr::Embedded(compiler_values::effect_value(name))
        )
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
    fn tentative_results_are_emitted_as_effects_without_an_automatic_return() {
        let result = SyntaxExpr::Effect(vec!["fail".to_owned()]);
        assert_eq!(
            resolve(&[GuardChoiceArm {
                line: 1,
                guards: &[],
                result_line: 1,
                result_mode: ConditionalResultMode::Tentative,
                result: &result,
            }]),
            effect_call_resolved("cut", [lower_effect_expr_resolved("fail")])
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
                result_mode: ConditionalResultMode::Ordinary,
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
                result_mode: ConditionalResultMode::Ordinary,
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
        assert_eq!(
            alternative_value(alternative),
            &ResolvedExpr::Local(*binding)
        );
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
            result_mode: ConditionalResultMode::Ordinary,
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
                    result_mode: ConditionalResultMode::Ordinary,
                    result: &first_result,
                },
                GuardChoiceArm {
                    line: 4,
                    guards: &second_guards,
                    result_line: 5,
                    result_mode: ConditionalResultMode::Ordinary,
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
        assert_eq!(
            alternative_value(first),
            &ResolvedExpr::Local(first_binding)
        );
        assert_eq!(
            alternative_value(second),
            &ResolvedExpr::Local(second_binding)
        );
        assert!(locals.is_empty());
    }

    #[test]
    fn prefix_if_resolves_each_owned_expression_once() {
        let if_expr = IfExpr {
            mode: ConditionalMode::Pure,
            guards: vec![SyntaxGuardClause::ValueBind {
                pattern: SyntaxPattern::wildcard(),
                value: number(73),
            }],
            then_mode: ConditionalResultMode::Ordinary,
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

    #[test]
    fn host_conditionals_return_their_root_cut_without_an_isolated_runner() {
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));

        let host_if = IfExpr {
            mode: ConditionalMode::Host,
            guards: vec![SyntaxGuardClause::Pass],
            then_mode: ConditionalResultMode::Ordinary,
            then_result: Box::new(number(1)),
            else_result: Box::new(number(2)),
        };
        let resolved_if = lower_if_expr_resolved(
            &host_if,
            1,
            &context,
            &scope.resolved(),
            &mut ResolverContext::default(),
        )
        .expect("host try should resolve");
        assert!(is_root_effect_call(&resolved_if, "cut"));

        let host_match_when = MatchWhenExpr {
            mode: ConditionalMode::Host,
            arms: Vec::new(),
        };
        let resolved_match = lower_match_when_expr_resolved(
            &host_match_when,
            &context,
            &scope.resolved(),
            &mut ResolverContext::default(),
        )
        .expect("empty host try_match should resolve");
        assert!(is_root_effect_call(&resolved_match, "cut"));
        assert!(contains_effect(&resolved_match, "fail"));
    }

    #[test]
    fn subject_match_resolves_the_subject_and_each_result_once() {
        let match_expr = MatchExpr {
            mode: ConditionalMode::Pure,
            subject: Box::new(number(73)),
            arms: vec![
                MatchArm {
                    line: 1,
                    pattern: SyntaxPattern {
                        kind: SyntaxPatternKind::Literal(SyntaxPatternLiteral::Number(
                            Number::from(1_i64),
                        )),
                    },
                    guards: Vec::new(),
                    outcome: MatchOutcome::Result {
                        line: 1,
                        mode: ConditionalResultMode::Ordinary,
                        expression: number(2),
                    },
                },
                MatchArm {
                    line: 2,
                    pattern: SyntaxPattern::wildcard(),
                    guards: Vec::new(),
                    outcome: MatchOutcome::Result {
                        line: 2,
                        mode: ConditionalResultMode::Ordinary,
                        expression: number(3),
                    },
                },
            ],
        };
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        let resolved = lower_match_expr_resolved(
            &match_expr,
            1,
            &context,
            &scope.resolved(),
            &mut ResolverContext::default(),
        )
        .expect("subject match should resolve");

        assert_eq!(count_embedded_number(&resolved, 73), 1);
        assert_eq!(count_embedded_number(&resolved, 2), 1);
        assert_eq!(count_embedded_number(&resolved, 3), 1);
    }

    #[test]
    fn hierarchical_match_resolves_a_shared_view_only_once() {
        let view = SyntaxExpr::Apply(
            Box::new(SyntaxExpr::Effect(vec!["r".to_owned()])),
            Box::new(number(73)),
        );
        let match_expr = MatchExpr {
            mode: ConditionalMode::Pure,
            subject: Box::new(number(74)),
            arms: vec![MatchArm {
                line: 1,
                pattern: SyntaxPattern {
                    kind: SyntaxPatternKind::View {
                        view: Box::new(view),
                        pattern: Box::new(SyntaxPattern::capture("value")),
                    },
                },
                guards: Vec::new(),
                outcome: MatchOutcome::Nested(vec![
                    WhenArm {
                        line: 2,
                        guards: vec![SyntaxGuardClause::Pass],
                        outcome: MatchOutcome::Result {
                            line: 2,
                            mode: ConditionalResultMode::Ordinary,
                            expression: SyntaxExpr::Name("value".to_owned()),
                        },
                    },
                    WhenArm {
                        line: 3,
                        guards: vec![SyntaxGuardClause::Pass],
                        outcome: MatchOutcome::Result {
                            line: 3,
                            mode: ConditionalResultMode::Ordinary,
                            expression: number(0),
                        },
                    },
                ]),
            }],
        };
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        let resolved = lower_match_expr_resolved(
            &match_expr,
            1,
            &context,
            &scope.resolved(),
            &mut ResolverContext::default(),
        )
        .expect("hierarchical match should resolve");

        assert_eq!(
            count_embedded_number(&resolved, 73),
            1,
            "nested child alternatives must not duplicate their shared view prefix"
        );
    }

    fn value_binding(alternative: &ResolvedAlternative) -> BindingId {
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

    fn alternative_value(alternative: &ResolvedAlternative) -> &ResolvedExpr<Value> {
        let ResolvedChoiceOutcome::Value(value) = &alternative.outcome else {
            panic!("expected a value alternative");
        };
        value
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
