//! Pattern-to-resolved-effect-step expansion.
//!
//! Every runtime operation observes one syntax-independent fact. Pattern
//! structure remains front-end syntax and source captures enter the resolver
//! exactly where their subpatterns succeed.

use super::super::*;
use super::effect_steps::{ResolvedEffectStep, ResolvedEffectStepKind, ResolvedPatternInput};

pub(super) trait PatternStepSink {
    fn locals(&mut self) -> &mut ResolverContext;
    fn push_step(&mut self, step: ResolvedEffectStep);
    fn push_capture(
        &mut self,
        input: ResolvedPatternInput,
        name: &str,
        line: usize,
    ) -> Result<(), Diagnostic>;
}

pub(super) struct PatternLoweringContext<'a> {
    context: &'a CompileContext,
    scope: &'a NameScope<ResolvedRoot>,
    sink: &'a mut dyn PatternStepSink,
}

impl<'a> PatternLoweringContext<'a> {
    pub(super) fn new(
        context: &'a CompileContext,
        scope: &'a NameScope<ResolvedRoot>,
        sink: &'a mut dyn PatternStepSink,
    ) -> Self {
        Self {
            context,
            scope,
            sink,
        }
    }

    fn resolve_expression(
        &mut self,
        expression: &SyntaxExpr,
        line: usize,
    ) -> Result<ResolvedExpr<Value>, Diagnostic> {
        syntax_expr_to_resolved_in_semantic_scope(
            expression,
            line,
            self.context,
            self.scope,
            self.sink.locals(),
        )
    }

    fn fresh_binding(&mut self) -> BindingId {
        self.sink.locals().fresh_binding()
    }

    fn push_step(&mut self, line: usize, kind: ResolvedEffectStepKind) {
        self.sink.push_step(ResolvedEffectStep { line, kind });
    }
}

pub(super) fn append_pattern_steps(
    input: ResolvedPatternInput,
    pattern: &SyntaxPattern,
    line: usize,
    lowering: &mut PatternLoweringContext<'_>,
) -> Result<(), Diagnostic> {
    if pattern.is_irrefutable() {
        return append_irrefutable_input(input, pattern, line, lowering);
    }

    let subject = lowering.fresh_binding();
    lowering.sink.push_step(input.into_step(line, subject));
    append_match_steps(subject, pattern, line, lowering)
}

fn append_value_pattern(
    value: ResolvedExpr<Value>,
    pattern: &SyntaxPattern,
    line: usize,
    lowering: &mut PatternLoweringContext<'_>,
) -> Result<(), Diagnostic> {
    append_pattern_steps(ResolvedPatternInput::Value(value), pattern, line, lowering)
}

fn append_irrefutable_input(
    input: ResolvedPatternInput,
    pattern: &SyntaxPattern,
    line: usize,
    lowering: &mut PatternLoweringContext<'_>,
) -> Result<(), Diagnostic> {
    let captures = pattern.captures();
    if captures.len() <= 1 {
        if let Some(name) = captures.first() {
            return lowering.sink.push_capture(input, name, line);
        }

        let binding = lowering.fresh_binding();
        lowering.sink.push_step(input.into_step(line, binding));
        return Ok(());
    }

    let subject = lowering.fresh_binding();
    lowering.sink.push_step(input.into_step(line, subject));
    for name in captures {
        lowering.sink.push_capture(
            ResolvedPatternInput::Value(ResolvedExpr::Local(subject)),
            name,
            line,
        )?;
    }
    Ok(())
}

fn append_match_steps(
    subject: BindingId,
    pattern: &SyntaxPattern,
    line: usize,
    lowering: &mut PatternLoweringContext<'_>,
) -> Result<(), Diagnostic> {
    match &pattern.kind {
        SyntaxPatternKind::Capture(name) => lowering.sink.push_capture(
            ResolvedPatternInput::Value(ResolvedExpr::Local(subject)),
            name,
            line,
        ),
        SyntaxPatternKind::Wildcard => Ok(()),
        SyntaxPatternKind::Group(pattern) => append_match_steps(subject, pattern, line, lowering),
        SyntaxPatternKind::As(left, right) => {
            append_match_steps(subject, left, line, lowering)?;
            append_match_steps(subject, right, line, lowering)
        }
        SyntaxPatternKind::View { view, pattern } => {
            let view = lowering.resolve_expression(view, line)?;
            let operation = ResolvedExpr::apply(view, [ResolvedExpr::Local(subject)]);
            let viewed = append_bind(operation, line, lowering);
            append_match_steps(viewed, pattern, line, lowering)
        }
        SyntaxPatternKind::Predicate { predicate, pattern } => {
            let predicate = lowering.resolve_expression(predicate, line)?;
            append_then(
                ResolvedExpr::apply(predicate, [ResolvedExpr::Local(subject)]),
                line,
                lowering,
            );
            append_match_steps(subject, pattern, line, lowering)
        }
        SyntaxPatternKind::Guarded { pattern, guards } => {
            append_match_steps(subject, pattern, line, lowering)?;
            append_guard_steps(guards, line, lowering)
        }
        SyntaxPatternKind::Literal(literal) => {
            append_then(
                pattern_builtin(
                    Builtin::PatternEqual,
                    [
                        ResolvedExpr::Embedded(pattern_literal_value(literal)),
                        ResolvedExpr::Local(subject),
                    ],
                ),
                line,
                lowering,
            );
            Ok(())
        }
        SyntaxPatternKind::List { .. } => append_list_match(subject, pattern, line, lowering),
        SyntaxPatternKind::Dict { .. } => append_dict_match(subject, pattern, line, lowering),
        SyntaxPatternKind::QuotedPath(path) => {
            let expected = syntax_path_resolved(
                path,
                line,
                lowering.context,
                lowering.scope,
                lowering.sink.locals(),
            )?;
            append_then(
                pattern_builtin(
                    Builtin::PatternPathEqual,
                    [expected, ResolvedExpr::Local(subject)],
                ),
                line,
                lowering,
            );
            Ok(())
        }
    }
}

fn append_guard_steps(
    guards: &[SyntaxGuardClause],
    line: usize,
    lowering: &mut PatternLoweringContext<'_>,
) -> Result<(), Diagnostic> {
    for guard in guards {
        match guard {
            SyntaxGuardClause::Pass => {}
            SyntaxGuardClause::Effect(expr) => {
                let operation = lowering.resolve_expression(expr, line)?;
                append_then(operation, line, lowering);
            }
            SyntaxGuardClause::EffectBind { pattern, operation } => {
                let operation = lowering.resolve_expression(operation, line)?;
                append_pattern_steps(
                    ResolvedPatternInput::Effect(operation),
                    pattern,
                    line,
                    lowering,
                )?;
            }
            SyntaxGuardClause::ValueBind { pattern, value } => {
                let value = lowering.resolve_expression(value, line)?;
                append_pattern_steps(ResolvedPatternInput::Value(value), pattern, line, lowering)?;
            }
        }
    }
    Ok(())
}

fn append_list_match(
    subject: BindingId,
    pattern: &SyntaxPattern,
    line: usize,
    lowering: &mut PatternLoweringContext<'_>,
) -> Result<(), Diagnostic> {
    let SyntaxPatternKind::List {
        prefix,
        middle,
        suffix,
    } = &pattern.kind
    else {
        unreachable!("list expansion receives a list pattern");
    };
    append_then(
        pattern_builtin(Builtin::PatternIsList, [ResolvedExpr::Local(subject)]),
        line,
        lowering,
    );

    let mut remainder = ResolvedExpr::Local(subject);
    for pattern in prefix {
        let parts = append_bind(
            pattern_builtin(Builtin::PatternListTryUncons, [remainder]),
            line,
            lowering,
        );
        append_value_pattern(part_access(parts, &keys::HEAD), pattern, line, lowering)?;
        remainder = part_access(parts, &keys::TAIL);
    }

    let mut extracted_suffix = Vec::with_capacity(suffix.len());
    for pattern in suffix.iter().rev() {
        let parts = append_bind(
            pattern_builtin(Builtin::PatternListTryUnsnoc, [remainder]),
            line,
            lowering,
        );
        let last = lowering.fresh_binding();
        lowering.push_step(
            line,
            ResolvedEffectStepKind::ValueBind {
                value: part_access(parts, &keys::LAST),
                binding: last,
            },
        );
        extracted_suffix.push((pattern, last));
        remainder = part_access(parts, &keys::INIT);
    }

    if let Some(middle) = middle.as_deref() {
        append_value_pattern(remainder, middle, line, lowering)?;
    } else {
        append_then(
            pattern_builtin(Builtin::PatternListIsEmpty, [remainder]),
            line,
            lowering,
        );
    }

    for (pattern, subject) in extracted_suffix.into_iter().rev() {
        append_match_steps(subject, pattern, line, lowering)?;
    }
    Ok(())
}

fn append_dict_match(
    subject: BindingId,
    pattern: &SyntaxPattern,
    line: usize,
    lowering: &mut PatternLoweringContext<'_>,
) -> Result<(), Diagnostic> {
    let SyntaxPatternKind::Dict { entries, remainder } = &pattern.kind else {
        unreachable!("dictionary expansion receives a dictionary pattern");
    };
    append_then(
        pattern_builtin(Builtin::PatternIsDict, [ResolvedExpr::Local(subject)]),
        line,
        lowering,
    );

    let mut rest = ResolvedExpr::Local(subject);
    for entry in entries {
        let builtin = if entry.optional {
            Builtin::PatternDictTryTakeOptional
        } else {
            Builtin::PatternDictTryTake
        };
        let path = syntax_path_resolved(
            &entry.path,
            line,
            lowering.context,
            lowering.scope,
            lowering.sink.locals(),
        )?;
        let parts = append_bind(pattern_builtin(builtin, [path, rest]), line, lowering);
        append_value_pattern(
            part_access(parts, &keys::VALUE),
            &entry.pattern,
            line,
            lowering,
        )?;
        rest = part_access(parts, &keys::REST);
    }

    if let Some(remainder) = remainder.as_deref() {
        append_value_pattern(rest, remainder, line, lowering)
    } else {
        append_then(
            pattern_builtin(Builtin::PatternDictIsEmpty, [rest]),
            line,
            lowering,
        );
        Ok(())
    }
}

fn append_bind(
    operation: ResolvedExpr<Value>,
    line: usize,
    lowering: &mut PatternLoweringContext<'_>,
) -> BindingId {
    let binding = lowering.fresh_binding();
    lowering.push_step(
        line,
        ResolvedEffectStepKind::EffectBind { operation, binding },
    );
    binding
}

fn append_then(
    operation: ResolvedExpr<Value>,
    line: usize,
    lowering: &mut PatternLoweringContext<'_>,
) {
    let result = lowering.fresh_binding();
    lowering.push_step(line, ResolvedEffectStepKind::Then { operation, result });
}

fn pattern_builtin(
    builtin: Builtin,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(ResolvedExpr::Embedded(Value::Builtin(builtin)), arguments)
}

fn part_access(parts: BindingId, key: &Key) -> ResolvedExpr<Value> {
    ResolvedExpr::Access {
        base: Box::new(ResolvedExpr::Local(parts)),
        path: vec![ResolvedPathPart::Key(key.clone())],
    }
}

fn pattern_literal_value(literal: &SyntaxPatternLiteral) -> Value {
    match literal {
        SyntaxPatternLiteral::Unit => (*keys::UNIT_VALUE).clone(),
        SyntaxPatternLiteral::Number(number) => Value::Number(number.clone()),
        SyntaxPatternLiteral::Atom(name) => {
            Value::Atom(Atom::from_key(&Key::binary_from_text(name)))
        }
        SyntaxPatternLiteral::Text(text) => Value::binary_from_text(text),
    }
}
