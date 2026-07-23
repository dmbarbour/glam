use crate::g_syntax::{DoExpr, DoStepKind, SyntaxExpr};

use super::super::super::compound::parse_expr_result;
use super::super::super::compound::token::parse_compound_expression_fragment;

fn assert_expression_parity(source: &str) -> SyntaxExpr {
    let character = parse_expr_result(source)
        .unwrap_or_else(|error| panic!("character parser rejected `{source}`: {error}"));
    let tokens = parse_compound_expression_fragment(source.as_bytes())
        .unwrap_or_else(|diagnostics| panic!("token parser rejected `{source}`: {diagnostics:#?}"));
    assert_eq!(tokens, character, "parser mismatch for `{source}`");
    tokens
}

#[test]
fn layout_do_matches_the_character_parser() {
    for source in [
        "do value",
        "do\n.read 'left -> left\nright <- .read 'right\ntotal = left + right\n.write total\n.r total",
        "do\nabstract left, _right\n.r (\\_ -> left) -> use_left\nleft = 1\nright <- .r 2\n.r (use_left ())",
        "do\nvalue <- do\n  input <- source\n  .r input\nwritten <- write\n  value\n.r written",
        "interaction_net do\n.bind -> ports\n.r ports",
        "\\api -> do\n.r api",
        "\\api -> interaction_net do\n.bind -> ports\n.r ports",
    ] {
        assert_expression_parity(source);
    }
}

#[test]
fn token_statement_classification_leaves_lambda_arrows_inside_expressions() {
    let expr = assert_expression_parity(
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
        "result:do { .r 1 }",
        "do { .r {answer: 42} }.answer",
        "(do { .r 1 }, do { .r 2 })",
        "do { nested <- do { .r 1 }; .r nested }",
        "do {\n; text = \"\"\"\n  \" semicolon; remains text\n\"\"\"\n; .r text\n}",
        "do {}",
        "do {   }",
    ] {
        assert_expression_parity(source);
    }
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
