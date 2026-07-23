use crate::diagnostic::Severity;
use crate::g_syntax::{
    DeclarationKind, Diagnostic, ObjectBodyDefinitionKind, ParsedSource, SyntaxExpr,
};

use super::expression_context::ExpressionContext;
use super::input::parse_expression_fragment;
use super::parse_source;
use super::structural::parse_expression_extent;

fn parse_without_errors(source: &str) -> ParsedSource {
    let parsed = parse_source(source.as_bytes());
    let errors = parsed
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect::<Vec<_>>();
    assert!(errors.is_empty(), "`{source}` reported {errors:#?}");
    parsed
}

fn definition_expr(source: &str) -> SyntaxExpr {
    let parsed = parse_without_errors(source);
    let mut expressions =
        parsed
            .declarations
            .into_iter()
            .filter_map(|declaration| match declaration.kind {
                DeclarationKind::Definition(definition) => definition.expr,
                _ => None,
            });
    let expression = expressions
        .next()
        .unwrap_or_else(|| panic!("`{source}` did not contain a parsed definition expression"));
    assert!(
        expressions.next().is_none(),
        "`{source}` contained more than one definition expression"
    );
    expression
}

fn assert_currently_rejected(source: &str) -> Vec<Diagnostic> {
    let parsed = parse_source(source.as_bytes());
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "`{source}` unexpectedly parsed without errors"
    );
    parsed.diagnostics
}

fn assert_has_error(source: &str, line: usize, message: &str) {
    let diagnostics = assert_currently_rejected(source);
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.line == line
                && diagnostic.message.contains(message)
        }),
        "`{source}` did not report line {line} containing `{message}`: {diagnostics:#?}"
    );
}

#[test]
fn contextual_expression_reports_its_absolute_token_end() {
    parse_expression_fragment(b"  do { .r value }.result", |view| {
        let expected_end = view.range().end();
        let parsed = parse_expression_extent(view, ExpressionContext::for_owner(view).may_yield())?;
        assert_eq!(parsed.end(), expected_end);
        assert!(matches!(parsed.into_expression(), SyntaxExpr::Access(_, _)));
        Ok(())
    })
    .unwrap();
}

#[test]
fn declaration_floor_does_not_depend_on_inline_rhs_position() {
    let inline = definition_expr(concat!(
        "language g0\n",
        "result = \\value ->\n",
        "  finish value\n",
    ));
    let next_line = definition_expr(concat!(
        "language g0\n",
        "result =\n",
        "  \\value ->\n",
        "  finish value\n",
    ));

    assert_eq!(inline, next_line);
    assert!(matches!(
        inline,
        SyntaxExpr::Lambda(parameters, body)
            if parameters == ["value"]
                && matches!(*body, SyntaxExpr::Apply(_, _))
    ));
}

#[test]
fn continuation_lambdas_as_infix_operands_are_currently_rejected() {
    for source in [
        concat!(
            "language g0\n",
            "result = Operation1 >>= \\r1 ->\n",
            "  Operation2 r1 >>= \\r2 ->\n",
            "  finish r1 r2\n",
        ),
        concat!(
            "language g0\n",
            "result =\n",
            "  Operation1 >>= \\r1 ->\n",
            "  Operation2 r1 >>= \\r2 ->\n",
            "  finish r1 r2\n",
        ),
    ] {
        assert_currently_rejected(source);
    }
}

#[test]
fn layout_do_is_already_a_final_application_argument() {
    let expression = definition_expr(concat!(
        "language g0\n",
        "result = begin_op_header do\n",
        "  Operation1 -> r1\n",
        "  Operation2 -> r2\n",
        "  finish r1 r2\n",
    ));

    let SyntaxExpr::Apply(function, argument) = expression else {
        panic!("definition should apply `begin_op_header` to one structural argument");
    };
    assert!(matches!(*function, SyntaxExpr::Name(ref name) if name == "begin_op_header"));
    assert!(matches!(*argument, SyntaxExpr::Do(_)));
}

#[test]
fn trailing_lambda_application_argument_is_currently_rejected() {
    assert_currently_rejected(concat!(
        "language g0\n",
        "mapped = map values \\value -> transform value\n",
    ));
}

#[test]
fn postfix_where_owns_its_binding_layout() {
    let expression = definition_expr(concat!(
        "language g0\n",
        "result = x + y + z where\n",
        "  y = 1\n",
        "  z = 2\n",
    ));

    let SyntaxExpr::Let { bindings, body } = expression else {
        panic!("postfix `where` should lower syntactically to a let expression");
    };
    assert_eq!(
        bindings
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["y", "z"]
    );
    assert!(matches!(*body, SyntaxExpr::Add(_, _)));
}

#[test]
fn dedented_where_preserves_nested_with_owners() {
    let nested_members = definition_expr(concat!(
        "language g0\n",
        "foo = op1 >>= op2 >>= op3 with\n",
        "    A := 42\n",
        "    B := op4 >>= op5 >>= op6 as o with\n",
        "        C c = op7\n",
        "  where\n",
        "    op1 = replacement\n",
    ));
    let SyntaxExpr::Let { bindings, body } = nested_members else {
        panic!("dedented `where` should own the outer binding group");
    };
    assert_eq!(
        bindings
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["op1"]
    );
    let SyntaxExpr::With {
        body: outer_members,
        ..
    } = *body
    else {
        panic!("the `where` body should retain the outer `with` expression");
    };
    assert_eq!(outer_members.len(), 2);
    let ObjectBodyDefinitionKind::Definition(nested_definition) = &outer_members[1].kind else {
        panic!("the second outer member should remain a definition");
    };
    assert!(matches!(
        nested_definition.expr.as_ref(),
        Some(SyntaxExpr::With { body, .. }) if body.len() == 1
    ));
}

#[test]
fn with_inside_a_where_binding_is_currently_rejected() {
    assert_currently_rejected(concat!(
        "language g0\n",
        "foo = op1 >>= op2 >>= op3a where\n",
        "    op3a = op3 with\n",
        "        C = value\n",
    ));
}

#[test]
fn leading_infix_lines_preserve_single_line_grouping() {
    let single_line = definition_expr(concat!(
        "language g0\n",
        "result = source |> decode |> validate\n",
    ));
    let multiline = definition_expr(concat!(
        "language g0\n",
        "result = source\n",
        "  |> decode\n",
        "  |> validate\n",
    ));

    assert_eq!(single_line, multiline);
}

#[test]
fn infix_resumption_after_layout_do_is_currently_rejected() {
    assert_currently_rejected(concat!(
        "language g0\n",
        "result = source\n",
        "  |> process do\n",
        "    input <- .read\n",
        "    .r (transform input)\n",
        "  |> finish\n",
    ));
}

#[test]
fn nested_layout_must_eventually_clear_an_operator_operand_floor() {
    assert_currently_rejected(concat!(
        "language g0\n",
        "result = source\n",
        "  |> configure with\n",
        "  A := 42\n",
        "  |> finish\n",
    ));
}

#[test]
fn boundary_aligned_closers_are_terminal_only() {
    parse_without_errors(concat!(
        "language g0\n",
        "value = (\n",
        "  1\n",
        ")\n",
        "next = value\n",
    ));

    assert_has_error(
        concat!("language g0\n", "value = (\n", "  1\n", ") + 2\n",),
        4,
        "expression continues after a boundary-aligned closing delimiter",
    );
    assert_has_error(
        concat!("language g0\n", "value = (\n", "  1\n", ")\n", "  + 2\n",),
        4,
        "expression continues after a boundary-aligned closing delimiter",
    );
    assert_has_error(
        concat!(
            "language g0\n",
            "object nested with\n",
            "  member = (\n",
            "    1\n",
            "  ) + 2\n",
        ),
        5,
        "expression continues after a boundary-aligned closing delimiter",
    );
}
