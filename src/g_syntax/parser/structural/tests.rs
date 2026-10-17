use super::*;
use crate::g_syntax::{ObjectBodyDefinition, ObjectBodyDefinitionKind, ObjectExpr, SyntaxExpr};

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
fn using_parses_explicit_and_do_bodies() {
    for source in [
        "using namespace in value",
        "using namespace in do .r value",
        "using namespace do .r value",
        "using (choose left right) in self.value",
        "using namespace in ^outer",
    ] {
        let parsed = parse_structural(source);
        assert!(
            matches!(parsed, SyntaxExpr::Using { .. }),
            "`{source}` did not produce a using expression"
        );
    }

    let shorthand = parse_structural("using namespace do .r value");
    let SyntaxExpr::Using { namespace, body } = shorthand else {
        panic!("using-do shorthand should produce a using expression");
    };
    assert!(matches!(*namespace, SyntaxExpr::Name(ref name) if name == "namespace"));
    assert!(matches!(*body, SyntaxExpr::Do(_)));
}

#[test]
fn malformed_using_expressions_report_the_missing_boundary() {
    for (source, expected) in [
        ("using", "requires a namespace and `in` or `do` body"),
        ("using namespace", "requires `in` or a trailing `do` body"),
        ("using in value", "requires a namespace before its body"),
        ("using namespace in", "requires a body"),
    ] {
        let diagnostics = parse_compound_expression_fragment(source.as_bytes())
            .expect_err("malformed using expression should be diagnosed");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "`{source}` reported {diagnostics:#?}"
        );
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
fn a_where_in_a_where_binding_scopes_that_binding() {
    // A `where` binding is a definition, so a `where` after its value on its
    // line scopes that value, as parentheses would.
    let parsed = parse_structural("result where x = y where y = 1");
    assert_eq!(parsed, parse_structural("result where x = (y where y = 1)"));
    let SyntaxExpr::Let { bindings, body } = parsed else {
        panic!("the first `where` should scope the body");
    };
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].0, "x");
    assert!(matches!(*body, SyntaxExpr::Name(ref name) if name == "result"));
    assert!(matches!(
        &bindings[0].1,
        SyntaxExpr::Let { bindings, body }
            if bindings[0].0 == "y" && matches!(body.as_ref(), SyntaxExpr::Name(name) if name == "y")
    ));
}

/// A definition, statement or arm takes one `where`, whose group holds all
/// its bindings; parentheses scope a nested one.
#[test]
fn a_body_takes_one_where() {
    for source in [
        "result\n  where x = y\n  where y = 1",
        "result where { x = y } where { y = 1 }",
        "base with { member = value }\n  where x = y\n  where y = 1",
        "do\n  .r a\n    where a = 1\n    where b = 2",
    ] {
        let diagnostics = parse_compound_expression_fragment(source.as_bytes())
            .expect_err("a second `where` is an error");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("takes one `where`")),
            "`{source}` reported {diagnostics:#?}"
        );
    }
    let SyntaxExpr::Let { bindings, body } = parse_structural("(result where x = y) where y = 1")
    else {
        panic!("the outer `where` should scope the parenthesized body");
    };
    assert_eq!(bindings[0].0, "y");
    assert!(matches!(
        body.as_ref(),
        SyntaxExpr::Let { bindings, body }
            if bindings[0].0 == "x" && matches!(body.as_ref(), SyntaxExpr::Name(name) if name == "result")
    ));
}

/// A `where` scopes the whole definition, statement or arm it ends, or a
/// parenthesized expression: it ends the open tails before it, and one that
/// would scope only part of an expression is an error.
#[test]
fn where_scopes_its_definition_statement_or_arm() {
    for (source, equivalent) in [
        (
            "if c then a else b where b = 1",
            "(if c then a else b) where b = 1",
        ),
        ("a if c else b where b = 1", "(a if c else b) where b = 1"),
        (
            "let a = 1 in a + b where b = 2",
            "(let a = 1 in a + b) where b = 2",
        ),
        ("\\y -> y + b where b = 1", "(\\y -> y + b) where b = 1"),
        ("f (a where a = 1) 2", "f (let a = 1 in a) 2"),
        (
            "do\n  p <- f (a where a = 1)\n  if g (b where b = 1) then .r p else .r 0",
            "do\n  p <- f (let a = 1 in a)\n  if g (let b = 1 in b) then .r p else .r 0",
        ),
        (
            "source\n  |> finish\n    where finish = 1",
            "(source\n  |> finish)\n    where finish = 1",
        ),
        (
            "do\n  a <- f b\n    where b = 1\n  g a",
            "do\n  a <- (f b\n    where b = 1)\n  g a",
        ),
        (
            "match v as\n  p => a\n    where a = 1\n  _ => 2",
            "match v as\n  p => (a\n    where a = 1)\n  _ => 2",
        ),
        (
            "a with\n    A := b\n      where b = 2\n    B := 3",
            "a with\n    A := (b\n      where b = 2)\n    B := 3",
        ),
        (
            "let a = b\n      where b = 1\na",
            "let a = (b\n      where b = 1)\na",
        ),
    ] {
        assert_eq!(
            parse_structural(source),
            parse_structural(equivalent),
            "{source}"
        );
    }
    for (source, expected) in [
        (
            "if c then a where a = 1 else b",
            "cannot stand in a `then` branch",
        ),
        (
            "if a where a = 1 then 1 else 2",
            "cannot stand in a conditional's guards",
        ),
        (
            "match a where a = 1 as\n  _ => 1",
            "cannot stand in a match's subject",
        ),
        (
            "match v as\n  p when a where a = 1 => 1",
            "cannot stand in an arm's pattern or guards",
        ),
        (
            "[a where a = 1, 2]",
            "cannot stand in a list or dictionary item",
        ),
        ("(a where a = 1, 2)", "cannot stand in a tuple item"),
        (
            "do\n  a <- f b\n  g a\n  where b = 1",
            "cannot begin a statement, arm or binding",
        ),
        (
            "do\n  .op b -> a\n    where b = 1\n  g a",
            "cannot follow a pattern",
        ),
        (
            "source\n  |> g with\n      A := 1\n    where g = 1\n  |> h",
            "nothing may follow a `where`'s bindings",
        ),
    ] {
        let diagnostics = parse_compound_expression_fragment(source.as_bytes())
            .expect_err("the source is invalid");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "`{source}` reported {diagnostics:#?} instead of `{expected}`"
        );
    }
}

/// An update's base is the chain before its `with` at its level, so a
/// `with` continues an open tail, as in a lambda's body or an `else`
/// branch, and takes an application before it whole.
#[test]
fn an_update_takes_the_chain_before_it() {
    let SyntaxExpr::Lambda(_, body) = parse_structural("\\y -> y with { a := 1 }") else {
        panic!("the lambda should hold the update");
    };
    assert!(matches!(*body, SyntaxExpr::With { .. }));
    let SyntaxExpr::If(conditional) = parse_structural("if c then d else d with { a := 1 }") else {
        panic!("the conditional should hold the update");
    };
    assert!(matches!(*conditional.else_result, SyntaxExpr::With { .. }));
    let SyntaxExpr::With { base, .. } = parse_structural("f d with { a := 1 }") else {
        panic!("the update should take the application");
    };
    assert!(matches!(*base, SyntaxExpr::Apply(..)));
    assert!(matches!(
        parse_structural("f (d with { a := 1 }) 2"),
        SyntaxExpr::Apply(..)
    ));
    let SyntaxExpr::OperatorApply { left, .. } =
        parse_structural("source\n  |> configure with\n    A := 42\n  |> finish")
    else {
        panic!("an operator should resume after the body");
    };
    assert!(matches!(*left, SyntaxExpr::With { .. }));
}

/// Nested update bodies are read by the declaration parser from their views,
/// one level of it per body.
#[test]
fn nested_update_bodies_parse() {
    let depth = 200;
    let source = format!("{}1{}", "d with { a := ".repeat(depth), " }".repeat(depth));
    let started = std::time::Instant::now();
    let mut parsed = parse_structural(&source);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    for _ in 0..depth {
        let SyntaxExpr::With { mut body, .. } = parsed else {
            panic!("each level should be an update");
        };
        let Some(ObjectBodyDefinition {
            kind: ObjectBodyDefinitionKind::Definition(definition),
            ..
        }) = body.pop()
        else {
            panic!("each body should hold one definition");
        };
        parsed = definition.expr.expect("a definition's value parses");
    }
    assert!(matches!(parsed, SyntaxExpr::Number(_)));
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

    let with = parse_structural("(choose 'as value) as alias with\n  value = \"as with = where\"");
    assert!(matches!(
        with,
        SyntaxExpr::With {
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
            realization,
            name,
            alias,
            deps,
            ..
        }) = parsed
        else {
            panic!("parser did not produce an object");
        };
        assert_eq!(
            token_object_header(source),
            ObjectHeader {
                realization,
                name,
                alias,
                deps,
                has_with: true,
            }
        );
    }
}

#[test]
fn update_aliases_name_the_update() {
    for (source, expected) in [
        ("{x:1} with\n  x := 2", None),
        ("{x:1} with { x := 2 }", None),
        ("base as prior with\n  x := _prior.x", Some("prior")),
        ("base as prior with {; x := _prior.x; }", Some("prior")),
        ("base as _ with\n  x := 2", None),
        ("base as _ with {}", None),
    ] {
        let SyntaxExpr::With { alias, .. } = parse_structural(source) else {
            panic!("parser did not produce a with expression");
        };
        assert_eq!(alias.as_deref(), expected, "{source}");
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
