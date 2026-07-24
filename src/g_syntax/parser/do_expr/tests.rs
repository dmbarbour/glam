use crate::g_syntax::{DoExpr, DoStepKind, SyntaxExpr};

use super::super::structural::parse_compound_expression_fragment;

fn parse_do_expression(source: &str) -> SyntaxExpr {
    parse_compound_expression_fragment(source.as_bytes())
        .unwrap_or_else(|diagnostics| panic!("parser rejected `{source}`: {diagnostics:#?}"))
}

#[test]
fn layout_do_parses() {
    for source in [
        "do value",
        "do first <- .read\n   second <- .read\n   .r [first, second]",
        "do # the first significant member establishes next-line layout\n  first <- .read\n\n  # comments do not change the anchor\n  second <- .read\n  .r [first, second]",
        "do\n.read 'left -> left\nright <- .read 'right\ntotal = left + right\n.write total\n.r total",
        "do\nabstract left, _right\n.r (\\_ -> left) -> use_left\nleft = 1\nright <- .r 2\n.r (use_left ())",
        "do\nvalue <- do\n  input <- source\n  .r input\nwritten <- write\n  value\n.r written",
        "interaction_net do\n.bind -> ports\n.r ports",
        "\\api -> do\n.r api",
        "\\api -> interaction_net do\n.bind -> ports\n.r ports",
    ] {
        parse_do_expression(source);
    }
}

#[test]
fn hanging_do_reports_the_expected_sibling_indentation() {
    for (source, actual) in [
        (
            "do first <- .read\n    second <- .read\n   .r [first, second]",
            4,
        ),
        (
            "do first <- .read\n  second <- .read\n   .r [first, second]",
            2,
        ),
    ] {
        let diagnostics = parse_compound_expression_fragment(source.as_bytes())
            .expect_err("a hanging do binding must align with the first statement");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.line == 2
                && diagnostic
                    .message
                    .contains(&format!("indented {actual} spaces"))
                && diagnostic
                    .message
                    .contains("expected sibling indentation 3")
        }));
    }
}

#[test]
fn token_statement_classification_leaves_lambda_arrows_inside_expressions() {
    let expr = parse_do_expression(
        "do\n(\\value -> value) -> function\nidentity = \\value -> value\n.r (function identity)",
    );
    let SyntaxExpr::Do(DoExpr { steps, .. }) = expr else {
        panic!("expected a do expression");
    };
    assert!(matches!(
        &steps[0].kind,
        DoStepKind::Bind { name, operation }
            if name == "function" && matches!(operation, SyntaxExpr::Lambda(_, _))
    ));
    assert!(matches!(
        &steps[1].kind,
        DoStepKind::ValueBind { name, value }
            if name == "identity" && matches!(value, SyntaxExpr::Lambda(_, _))
    ));
}

#[test]
fn braced_do_is_a_structural_atom_in_containers_and_other_do_blocks() {
    for source in [
        "consume [do { .r 1 }, do { text = \"a;b\"; .r text }, do { x = do .r 2; .r x }]",
        "consume do { .r 1 } next",
        "result:do { .r 1 }",
        "do { .r {answer: 42} }.answer",
        "(do { .r 1 }, do { .r 2 })",
        "do { nested <- do { .r 1 }; .r nested }",
        "do {\n; text = \"\"\"\n  \" semicolon; remains text\n\"\"\"\n; .r text\n}",
        "do {}",
        "do {   }",
    ] {
        parse_do_expression(source);
    }
}

#[test]
fn braced_do_semicolons_currently_separate_unparenthesized_where_expressions() {
    for source in [
        "do { result where x = 1; y = 2 }",
        "do { result where x = 1; y = 2; .r y }",
    ] {
        let diagnostics = parse_compound_expression_fragment(source.as_bytes())
            .expect_err("an unparenthesized inline `where` is currently misclassified");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic
                    .message
                    .contains("patterns are not yet supported in do value bindings")
            }),
            "unexpected diagnostics for `{source}`: {diagnostics:#?}"
        );
    }

    let parsed = parse_do_expression("do { (result where x = 1); y = 2; .r y }");
    let SyntaxExpr::Do(DoExpr { steps, .. }) = parsed else {
        panic!("expected a do expression");
    };
    assert!(matches!(
        &steps[0].kind,
        DoStepKind::Then(SyntaxExpr::Let { bindings, .. })
            if bindings.iter().map(|(name, _)| name.as_str()).eq(["x"])
    ));
    assert!(matches!(
        &steps[1].kind,
        DoStepKind::ValueBind { name, .. } if name == "y"
    ));

    let parenthesized = parse_do_expression("do { (result where { x = 1; y = 2 }) }");
    let SyntaxExpr::Do(DoExpr { result, .. }) = parenthesized else {
        panic!("expected a do expression");
    };
    assert!(matches!(
        result.as_ref(),
        SyntaxExpr::Let { bindings, .. }
            if bindings.iter().map(|(name, _)| name.as_str()).eq(["x", "y"])
    ));

    let braced_where = parse_do_expression("do { result where { x = 1 }; y = 2; .r y }");
    let SyntaxExpr::Do(DoExpr { steps, .. }) = braced_where else {
        panic!("expected a do expression");
    };
    assert!(matches!(
        &steps[0].kind,
        DoStepKind::Then(SyntaxExpr::Let { bindings, .. })
            if bindings.iter().map(|(name, _)| name.as_str()).eq(["x"])
    ));
}

#[test]
fn token_do_reports_structural_statement_errors() {
    let cases = [
        ("do {;}", "semicolon is not an empty computation"),
        ("do { .r ();; .r () }", "empty statement"),
        ("do { value <- .r 1; }", "cannot end with a binding"),
        (
            "do\n[first] <- .read\n.r first",
            "patterns are not yet supported",
        ),
        (
            "do\n.read -> first second\n.r ()",
            "requires exactly one local name",
        ),
        ("do\n-> result\n.r result", "requires an operation"),
        (
            "do\nabstract\n.r ()",
            "requires one or more comma-separated local names",
        ),
        (
            "do\nabstract _\n.r ()",
            "cannot use the inaccessible `_` name",
        ),
        (
            "do\nabstract value",
            "cannot end with an abstract declaration",
        ),
    ];

    for (source, expected) in cases {
        let diagnostics = parse_compound_expression_fragment(source.as_bytes())
            .expect_err("malformed do expression should be rejected");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "`{source}` reported {diagnostics:#?} instead of `{expected}`"
        );
    }
}
