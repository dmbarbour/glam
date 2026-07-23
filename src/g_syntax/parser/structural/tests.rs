use super::*;
use crate::g_syntax::{ObjectExpr, SyntaxExpr};

fn parse_structural(source: &str) -> SyntaxExpr {
    parse_compound_expression_fragment(source.as_bytes())
        .unwrap_or_else(|diagnostics| panic!("parser rejected `{source}`: {diagnostics:#?}"))
}

fn token_object_header(source: &str) -> ObjectHeader {
    super::super::input::parse_expression_fragment(source.as_bytes(), |view| {
        parse_object_header(view).expect("source should begin with an object expression")
    })
    .unwrap()
}

fn token_with_header(source: &str) -> WithHeader {
    super::super::input::parse_expression_fragment(source.as_bytes(), |view| {
        parse_with_header(view).expect("source should contain a with-expression header")
    })
    .unwrap()
}

#[test]
fn let_and_where_parse() {
    for source in [
        "let x = 1 in x + x",
        "let x = 1; _y = 2 in x",
        "let x = {in:\"where =\"} in x",
        "let x = (left == right) in x",
        "let x = 1 in x where y = 2",
        "let x = 1\n    y = 2\nx + y",
        "x + y where x = 1; y = 2",
        "x where x = y where y = 1",
        "f (where) where x = \"in = where\"",
        "x + y where\nx = 1\ny = 2",
    ] {
        parse_structural(source);
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
fn token_keywords_ignore_nested_groups_and_text() {
    parse_structural("let x = {in:\"where =\"} in x");
    parse_structural("f (where) where x = \"in where = as\"");

    let object = token_object_header(
        "object (choose as extends) as alias extends (parent with), right with\n  value = 1",
    );
    assert!(matches!(
        object,
        ObjectHeader {
            alias: Some(alias),
            deps,
            ..
        } if alias == "alias" && deps.len() == 2
    ));

    let with = token_with_header("(choose as value) as alias with\n  value = \"as with = where\"");
    assert!(matches!(
        with,
        WithHeader {
            alias: Some(alias),
            ..
        } if alias == "alias"
    ));

    assert!(matches!(
        parse_compound_expression_fragment(b"f .where"),
        Ok(SyntaxExpr::Apply(_, argument))
            if matches!(*argument, SyntaxExpr::Effect(ref path) if path == &["where"])
    ));
}

#[test]
fn object_headers_match_the_complete_structural_parse() {
    for source in [
        "object \"child\" with\n  value = 1",
        "object \"child\" as _self extends left, right with\n  value = 1",
        "object _ as _ with\n  value = 1",
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
        "base as prior with\n  x := _prior.x",
        "base as _ with\n  x := 2",
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
    assert!(
        parse_compound_expression_fragment(b"x + y where\n  x = 1\n  y = 2").is_ok(),
        "raw source indentation should be interpreted without normalizing text"
    );

    let let_diagnostics =
        parse_compound_expression_fragment(b"let first = 1\n  second = 2\nfirst + second")
            .expect_err("misaligned let bindings must be rejected");
    assert!(let_diagnostics.iter().any(|diagnostic| {
        diagnostic.line == 2 && diagnostic.message.contains("binding names must align")
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
        diagnostic.line == 3 && diagnostic.message.contains("binding names must align")
    }));
}
