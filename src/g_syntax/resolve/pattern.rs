//! Pattern-to-primitive-do expansion.
//!
//! Every runtime operation observes one syntax-independent fact. Pattern
//! structure remains front-end syntax and source captures enter the resolver
//! exactly where their subpatterns succeed.

use super::super::recursive_do::{ForwardNameRegistry, RecursiveDoEvent};
use super::super::*;
use super::do_expr::{
    PrimitiveDoStep, PrimitiveDoStepKind, PrimitivePatternInput, ResolvedForward,
    push_primitive_step,
};

pub(super) fn append_pattern_steps(
    steps: &mut Vec<PrimitiveDoStep>,
    input: PrimitivePatternInput,
    forward_names: &mut ForwardNameRegistry,
    pattern: &SyntaxPattern,
    line: usize,
    locals: &mut ResolverContext,
    forwards: &mut [ResolvedForward],
) -> Result<(), Diagnostic> {
    if pattern.is_irrefutable() {
        return append_irrefutable_input(
            steps,
            input,
            forward_names,
            pattern,
            line,
            locals,
            forwards,
        );
    }

    let subject = locals.fresh_binding();
    push_primitive_step(
        steps,
        line,
        input.into_step(subject),
        RecursiveDoEvent::None,
    );
    append_match_steps(
        steps,
        subject,
        forward_names,
        pattern,
        line,
        locals,
        forwards,
    )
}

fn append_value_pattern(
    steps: &mut Vec<PrimitiveDoStep>,
    value: ResolvedExpr<Value>,
    forward_names: &mut ForwardNameRegistry,
    pattern: &SyntaxPattern,
    line: usize,
    locals: &mut ResolverContext,
    forwards: &mut [ResolvedForward],
) -> Result<(), Diagnostic> {
    append_pattern_steps(
        steps,
        PrimitivePatternInput::Value(value),
        forward_names,
        pattern,
        line,
        locals,
        forwards,
    )
}

fn append_irrefutable_input(
    steps: &mut Vec<PrimitiveDoStep>,
    input: PrimitivePatternInput,
    forward_names: &mut ForwardNameRegistry,
    pattern: &SyntaxPattern,
    line: usize,
    locals: &mut ResolverContext,
    forwards: &mut [ResolvedForward],
) -> Result<(), Diagnostic> {
    let captures = pattern.captures();
    if captures.len() <= 1 {
        let (binding, recursion) = if let Some(name) = captures.first() {
            resolve_capture_binding(forward_names, name, line, locals, forwards)?
        } else {
            (locals.fresh_binding(), RecursiveDoEvent::None)
        };
        push_primitive_step(steps, line, input.into_step(binding), recursion);
        return Ok(());
    }

    let subject = locals.fresh_binding();
    push_primitive_step(
        steps,
        line,
        input.into_step(subject),
        RecursiveDoEvent::None,
    );
    for name in captures {
        append_capture(steps, subject, forward_names, name, line, locals, forwards)?;
    }
    Ok(())
}

fn append_match_steps(
    steps: &mut Vec<PrimitiveDoStep>,
    subject: BindingId,
    forward_names: &mut ForwardNameRegistry,
    pattern: &SyntaxPattern,
    line: usize,
    locals: &mut ResolverContext,
    forwards: &mut [ResolvedForward],
) -> Result<(), Diagnostic> {
    match &pattern.kind {
        SyntaxPatternKind::Capture(name) => {
            append_capture(steps, subject, forward_names, name, line, locals, forwards)
        }
        SyntaxPatternKind::Wildcard => Ok(()),
        SyntaxPatternKind::Group(pattern) => append_match_steps(
            steps,
            subject,
            forward_names,
            pattern,
            line,
            locals,
            forwards,
        ),
        SyntaxPatternKind::As(left, right) => {
            append_match_steps(steps, subject, forward_names, left, line, locals, forwards)?;
            append_match_steps(steps, subject, forward_names, right, line, locals, forwards)
        }
        SyntaxPatternKind::Literal(literal) => {
            append_then(
                steps,
                pattern_builtin(
                    Builtin::PatternEqual,
                    [
                        ResolvedExpr::Embedded(pattern_literal_value(literal)),
                        ResolvedExpr::Local(subject),
                    ],
                ),
                line,
                locals,
            );
            Ok(())
        }
        SyntaxPatternKind::List { .. } => append_list_match(
            steps,
            subject,
            forward_names,
            pattern,
            line,
            locals,
            forwards,
        ),
    }
}

fn append_list_match(
    steps: &mut Vec<PrimitiveDoStep>,
    subject: BindingId,
    forward_names: &mut ForwardNameRegistry,
    pattern: &SyntaxPattern,
    line: usize,
    locals: &mut ResolverContext,
    forwards: &mut [ResolvedForward],
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
        steps,
        pattern_builtin(Builtin::PatternIsList, [ResolvedExpr::Local(subject)]),
        line,
        locals,
    );

    let mut remainder = ResolvedExpr::Local(subject);
    for pattern in prefix {
        let parts = append_bind(
            steps,
            pattern_builtin(Builtin::PatternListTryUncons, [remainder]),
            line,
            locals,
        );
        append_value_pattern(
            steps,
            part_access(parts, &keys::HEAD),
            forward_names,
            pattern,
            line,
            locals,
            forwards,
        )?;
        remainder = part_access(parts, &keys::TAIL);
    }

    let mut extracted_suffix = Vec::with_capacity(suffix.len());
    for pattern in suffix.iter().rev() {
        let parts = append_bind(
            steps,
            pattern_builtin(Builtin::PatternListTryUnsnoc, [remainder]),
            line,
            locals,
        );
        let last = locals.fresh_binding();
        push_primitive_step(
            steps,
            line,
            PrimitiveDoStepKind::ValueBind {
                value: part_access(parts, &keys::LAST),
                binding: last,
            },
            RecursiveDoEvent::None,
        );
        extracted_suffix.push((pattern, last));
        remainder = part_access(parts, &keys::INIT);
    }

    if let Some(middle) = middle.as_deref() {
        debug_assert!(
            middle.is_irrefutable(),
            "variable-length list segments are irrefutable"
        );
        append_value_pattern(
            steps,
            remainder,
            forward_names,
            middle,
            line,
            locals,
            forwards,
        )?;
    } else {
        append_then(
            steps,
            pattern_builtin(Builtin::PatternListIsEmpty, [remainder]),
            line,
            locals,
        );
    }

    for (pattern, subject) in extracted_suffix.into_iter().rev() {
        append_match_steps(
            steps,
            subject,
            forward_names,
            pattern,
            line,
            locals,
            forwards,
        )?;
    }
    Ok(())
}

fn append_capture(
    steps: &mut Vec<PrimitiveDoStep>,
    subject: BindingId,
    forward_names: &mut ForwardNameRegistry,
    name: &str,
    line: usize,
    locals: &mut ResolverContext,
    forwards: &mut [ResolvedForward],
) -> Result<(), Diagnostic> {
    let (binding, recursion) =
        resolve_capture_binding(forward_names, name, line, locals, forwards)?;
    push_primitive_step(
        steps,
        line,
        PrimitiveDoStepKind::ValueBind {
            value: ResolvedExpr::Local(subject),
            binding,
        },
        recursion,
    );
    Ok(())
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
            .expect("one do binder produces one binding identity");
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

fn append_bind(
    steps: &mut Vec<PrimitiveDoStep>,
    operation: ResolvedExpr<Value>,
    line: usize,
    locals: &mut ResolverContext,
) -> BindingId {
    let binding = locals.fresh_binding();
    push_primitive_step(
        steps,
        line,
        PrimitiveDoStepKind::Bind { operation, binding },
        RecursiveDoEvent::None,
    );
    binding
}

fn append_then(
    steps: &mut Vec<PrimitiveDoStep>,
    operation: ResolvedExpr<Value>,
    line: usize,
    locals: &mut ResolverContext,
) {
    let result = locals.fresh_binding();
    push_primitive_step(
        steps,
        line,
        PrimitiveDoStepKind::Then { operation, result },
        RecursiveDoEvent::None,
    );
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
