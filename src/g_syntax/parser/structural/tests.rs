use super::*;
use crate::g_syntax::{ObjectExpr, SyntaxExpr};

fn parse_structural(source: &str) -> SyntaxExpr {
    parse_compound_expression_fragment(source.as_bytes())
        .unwrap_or_else(|diagnostics| panic!("parser rejected `{source}`: {diagnostics:#?}"))
}

fn token_object_header(source: &str) -> ObjectHeader {
    super::super::input::parse_expression_fragment(source.as_bytes(), |view| {
        parse_object_header(view, ExpressionContext::for_owner(view))
            .expect("source should begin with an object expression")
    })
    .unwrap()
}

fn token_with_header(source: &str) -> WithHeader {
    super::super::input::parse_expression_fragment(source.as_bytes(), |view| {
        parse_with_header(view, ExpressionContext::for_owner(view))
            .expect("source should contain a with-expression header")
    })
    .unwrap()
}

#[test]
fn let_and_where_parse() {
    for source in [
        "let x = 1 in x + x",
        "let { x = 1; _y = 2 } in x",
        "let {; x = 1; _y = 2; } in x",
        "let {} in x",
        "let x = {key:\"in where =\"} in x",
        "let x = (left == right) in x",
        "let x = 1 in x where y = 2",
        "let x = 1\n    y = 2\nx + y",
        "x + y where { x = 1; y = 2 }",
        "x + y where {; x = 1; y = 2; }",
        "x where {}",
        "x where x = y where y = 1",
        "f ('where) where x = \"in = where\"",
        "x + y where\nx = 1\ny = 2",
        "x + y where x = 1\n            y = 2",
    ] {
        parse_structural(source);
    }
}

#[test]
fn where_and_with_support_hanging_member_layout() {
    let parsed =
        parse_structural("first + second where first = 1\n                     second = 2");
    assert!(matches!(
        parsed,
        SyntaxExpr::Let { bindings, .. }
            if bindings.iter().map(|(name, _)| name.as_str()).eq(["first", "second"])
    ));

    let parsed = parse_structural("base with first := 1\n          second = 2");
    assert!(matches!(
        parsed,
        SyntaxExpr::With { body, .. } if body.len() == 2
    ));

    let parsed = parse_structural("object _ with first = 1\n              second = 2");
    assert!(matches!(
        parsed,
        SyntaxExpr::Object(ObjectExpr { body, .. }) if body.len() == 2
    ));

    let outer_anchor = "base with ".len();
    let inner_anchor = "base with child = object _ with ".len();
    let source = format!(
        "base with child = object _ with first = 1\n{inner:inner_anchor$}second = 2\n{outer:outer_anchor$}sibling = 3",
        inner = "",
        outer = "",
    );
    let parsed = parse_structural(&source);
    let SyntaxExpr::With { body, .. } = parsed else {
        panic!("outer hanging body should remain a with expression");
    };
    assert_eq!(body.len(), 2);
    assert!(matches!(
        &body[0].kind,
        crate::g_syntax::ObjectBodyDefinitionKind::Definition(definition)
            if matches!(
                definition.expr.as_ref(),
                Some(SyntaxExpr::Object(ObjectExpr { body, .. })) if body.len() == 2
            )
    ));
}

#[test]
fn hanging_where_and_with_report_the_expected_sibling_indentation() {
    for (source, expected) in [
        (
            "value where first = 1\n  second = 2",
            "multi-line where binding is indented 2 spaces; expected sibling indentation 12",
        ),
        (
            "base with first = 1\n  second = 2",
            "hanging `with` member is indented 2 spaces; expected sibling indentation 10",
        ),
    ] {
        let diagnostics = parse_compound_expression_fragment(source.as_bytes())
            .expect_err("under-indented hanging members must be rejected");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.line == 2 && diagnostic.message == expected),
            "`{source}` reported {diagnostics:#?}"
        );
    }
}

#[test]
fn naked_semicolons_do_not_group_let_or_where_bindings() {
    for (source, expected) in [
        (
            "let x = 1; y = 2 in x + y",
            "naked semicolon-separated `let` bindings",
        ),
        (
            "x + y where x = 1; y = 2",
            "naked semicolon-separated `where` bindings",
        ),
    ] {
        let diagnostics = parse_compound_expression_fragment(source.as_bytes())
            .expect_err("naked semicolon-separated bindings must be rejected");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "`{source}` reported {diagnostics:#?}"
        );
    }
}

#[test]
fn braced_binding_groups_distinguish_empty_bodies_from_empty_members() {
    for source in [
        "let {;} in value",
        "let { x = 1;; y = 2 } in value",
        "value where {;}",
        "value where { x = 1;; y = 2 }",
    ] {
        let diagnostics = parse_compound_expression_fragment(source.as_bytes())
            .expect_err("semicolon-only or interior-empty binding members must be rejected");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.message.contains("use `{}` for an empty body")
                    || diagnostic
                        .message
                        .contains("empty member between semicolons")
            }),
            "`{source}` reported {diagnostics:#?}"
        );
    }
}

#[test]
fn chained_where_suffixes_are_left_associative_binding_groups() {
    let parsed = parse_structural("result where x = y where y = 1");
    let SyntaxExpr::Let {
        bindings: outer_bindings,
        body: outer_body,
    } = parsed
    else {
        panic!("last `where` suffix should produce the outer binding group");
    };
    assert_eq!(
        outer_bindings
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["y"]
    );

    let SyntaxExpr::Let {
        bindings: inner_bindings,
        body: inner_body,
    } = *outer_body
    else {
        panic!("first `where` suffix should produce the inner binding group");
    };
    assert_eq!(
        inner_bindings
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["x"]
    );
    assert!(matches!(*inner_body, SyntaxExpr::Name(ref name) if name == "result"));

    let after_structural = parse_structural("base with { member = value } where x = y where y = 1");
    let SyntaxExpr::Let {
        bindings: outer_bindings,
        body: outer_body,
    } = after_structural
    else {
        panic!("the later where should remain the outer structural suffix");
    };
    assert_eq!(outer_bindings[0].0, "y");
    assert!(matches!(
        outer_body.as_ref(),
        SyntaxExpr::Let { bindings, body }
            if bindings[0].0 == "x" && matches!(body.as_ref(), SyntaxExpr::With { .. })
    ));
}

#[test]
fn parentheses_can_make_a_where_binding_right_associative() {
    let parsed = parse_structural("result where x = (y where y = 1)");
    let SyntaxExpr::Let { bindings, body } = parsed else {
        panic!("outer expression should contain the `x` binding group");
    };
    assert_eq!(
        bindings
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["x"]
    );
    assert!(matches!(*body, SyntaxExpr::Name(ref name) if name == "result"));

    let SyntaxExpr::Let {
        bindings: nested_bindings,
        body: nested_body,
    } = &bindings[0].1
    else {
        panic!("parenthesized `where` should remain inside the `x` definition");
    };
    assert_eq!(
        nested_bindings
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["y"]
    );
    assert!(matches!(
        nested_body.as_ref(),
        SyntaxExpr::Name(name) if name == "y"
    ));
}

#[test]
fn where_after_a_bodyless_object_wraps_the_object_expression() {
    let parsed = parse_structural("object _ where x = 1");
    let SyntaxExpr::Let { body, bindings } = parsed else {
        panic!("where should remain outside a bodyless object expression");
    };
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].0, "x");
    assert!(matches!(body.as_ref(), SyntaxExpr::Object(_)));
}

#[test]
fn token_keywords_ignore_nested_groups_and_text() {
    parse_structural("let x = {key:\"in where =\"} in x");
    parse_structural("f ('where) where x = \"in where = as\"");
    parse_structural("(choose 'as value)");

    let object = token_object_header(
        "object (choose 'as 'extends) as alias extends (parent 'with), right with\n  value = 1",
    );
    assert!(matches!(
        object,
        ObjectHeader {
            alias: Some(alias),
            deps,
            ..
        } if alias == "alias" && deps.len() == 2
    ));

    let with = token_with_header("(choose 'as value) as alias with\n  value = \"as with = where\"");
    assert!(matches!(
        with,
        WithHeader {
            alias: Some(alias),
            ..
        } if alias == "alias"
    ));

    assert!(matches!(
        parse_compound_expression_fragment(b"f (.where)"),
        Ok(SyntaxExpr::Apply(_, argument))
            if matches!(*argument, SyntaxExpr::Effect(ref path) if path == &["where"])
    ));
    assert!(
        parse_compound_expression_fragment(b"f .where")
            .expect_err("dot-leading arguments should require parentheses")
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("dot-leading application arguments must be parenthesized"))
    );
}

#[test]
fn object_headers_match_the_complete_structural_parse() {
    for source in [
        "object \"child\" with\n  value = 1",
        "object \"child\" with { value = 1 }",
        "object \"child\" as _self extends left, right with\n  value = 1",
        "object \"child\" as _self extends left, right with {; value = 1; }",
        "object _ as _ with\n  value = 1",
        "object _ as _ with {}",
    ] {
        let parsed = parse_structural(source);
        let SyntaxExpr::Object(ObjectExpr {
            name, alias, deps, ..
        }) = parsed
        else {
            panic!("parser did not produce an object");
        };
        assert_eq!(
            token_object_header(source),
            ObjectHeader {
                name,
                alias,
                deps,
                has_with: true,
            }
        );
    }
}

#[test]
fn with_headers_match_the_complete_structural_parse() {
    for source in [
        "{x:1} with\n  x := 2",
        "{x:1} with { x := 2 }",
        "base as prior with\n  x := _prior.x",
        "base as prior with {; x := _prior.x; }",
        "base as _ with\n  x := 2",
        "base as _ with {}",
    ] {
        let parsed = parse_structural(source);
        let SyntaxExpr::With { base, alias, .. } = parsed else {
            panic!("parser did not produce a with expression");
        };
        assert_eq!(token_with_header(source), WithHeader { base, alias });
    }
}

#[test]
fn multiline_let_and_where_use_lexical_indentation_lines() {
    for source in [
        "let first = 1\n    second = 2\nfirst + second",
        "let # the first significant binding begins on the next line\n  first = 1\n\n  # comments do not change the anchor\n  second = 2\nfirst + second",
    ] {
        assert!(
            parse_compound_expression_fragment(source.as_bytes()).is_ok(),
            "valid hanging/next-line let was rejected: `{source}`"
        );
    }

    assert!(
        parse_compound_expression_fragment(b"x + y where\n  x = 1\n  y = 2").is_ok(),
        "raw source indentation should be interpreted without normalizing text"
    );

    let let_diagnostics =
        parse_compound_expression_fragment(b"let first = 1\n  second = 2\nfirst + second")
            .expect_err("misaligned let bindings must be rejected");
    assert!(let_diagnostics.iter().any(|diagnostic| {
        diagnostic.line == 2
            && diagnostic.message.contains("indented 2 spaces")
            && diagnostic
                .message
                .contains("expected sibling indentation 4")
    }));

    let in_diagnostics =
        parse_compound_expression_fragment(b"let first = 1\n    second = 2\nin first")
            .expect_err("multiline let must reject in");
    assert!(in_diagnostics.iter().any(|diagnostic| {
        diagnostic.line == 1 && diagnostic.message.contains("must not use `in`")
    }));

    let where_diagnostics =
        parse_compound_expression_fragment(b"value where\n    first = 1\n  second = 2")
            .expect_err("misaligned where bindings must be rejected");
    assert!(where_diagnostics.iter().any(|diagnostic| {
        diagnostic.line == 3
            && diagnostic.message.contains("indented 2 spaces")
            && diagnostic
                .message
                .contains("expected sibling indentation 4")
    }));
}
