use super::*;

fn assert_parses(source: &str) {
    parse_expression_fragment(source.as_bytes()).unwrap_or_else(|diagnostics| {
        panic!(
            "expression grammar rejected `{source}`: {}",
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )
    });
}

fn assert_rejects(source: &str) {
    assert!(
        parse_expression_fragment(source.as_bytes()).is_err(),
        "expression grammar unexpectedly accepted `{source}`"
    );
}

#[test]
fn embedded_data_is_an_ordinary_parser_atom() {
    let value = crate::core::Value::Number(crate::number::Number::from(42_i64));
    let values = crate::compiler::test_value_factory();
    let rooted = crate::runtime::RuntimeValueRoot::new(&values, value.duplicate_for_test(&values));
    let lexical = super::super::lexical::embedded_source(rooted.clone());
    let view = TokenView::whole(&lexical);
    let parsed = parse_expression_view(view, ExpressionContext::for_fragment(view)).unwrap();

    assert_eq!(parsed, SyntaxExpr::Embedded(rooted));
}

#[test]
fn ordinary_expressions_parse() {
    const EXPRESSIONS: &[&str] = &[
        "()",
        "name",
        "abstract_global_path foo",
        "abstract_global_path foo.path",
        "abstract_global_path module.foo.path",
        "module",
        "module_origin",
        "self",
        "_prior",
        "^outer",
        "^^outer",
        "^(prefix ++ suffix).tail",
        "'atom",
        "'.foo.[42]",
        "'.foo.([1, 2]).bar",
        "'.[]",
        ".emit",
        ".heap.get",
        "0",
        "_42",
        "1/6",
        "3/4",
        "1.25e3",
        "\"ordinary text\"",
        "\"semicolons; and [brackets] {stay} text\"",
        "\"\"\"\n\" ordinary \"quotes\" are retained\n\"\"\"",
        "(grouped)",
        "(,)",
        "(singleton,)",
        "(,singleton)",
        "(first, second,)",
        "(\n, first\n, second\n,)",
        "[]",
        "[, first, second,]",
        "{}",
        "{, first, second,}",
        "{foo: 1, foo.bar: 2, [0, 1]: 3}",
        ":[tag]",
        ":foo.bar",
        "tag:value",
        "foo.bar:value",
        "[first, second]:value",
        "([first] ++ tail):value",
        "outer:inner:value",
        "g tag:f x",
        "tag:(f x)",
        "\\x y -> x y",
        "f x y z",
        "foo.bar",
        "foo (.bar)",
        "foo <| .bar",
        "f\n  x\n  y",
        "(+)",
        "(+ 42)",
        "(42 -)",
        "(++ suffix)",
        "(+ 42\n  )",
        "value |> f",
        "f <| value",
        "f >> g",
        "g << f",
        "op >>= k",
        "k1 >=> k2",
        "op =>> next",
        "first !> second !> function",
        "function <! first <! second",
        "1 + (2 * 3)",
        "x < y =< z",
        "x and (y or z)",
        "android",
        "'and",
        "'where",
        ".where",
    ];

    for source in EXPRESSIONS {
        assert_parses(source);
    }
}

#[test]
fn dot_leading_application_arguments_require_parentheses() {
    for source in ["foo .bar", "foo .bar.baz", "foo\n  .bar"] {
        let diagnostics = parse_expression_fragment(source.as_bytes())
            .expect_err("dot-leading application argument should require parentheses");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic
                    .message
                    .contains("dot-leading application arguments must be parenthesized")
                    && diagnostic.message.contains("f (.bar)")
                    && diagnostic.message.contains("<|")
            }),
            "missing dot-leading argument diagnostic for `{source}`: {diagnostics:?}"
        );
    }
}

#[test]
fn every_g0_keyword_is_reserved_as_an_ordinary_name() {
    for keyword in crate::g_syntax::keywords::G0_KEYWORDS {
        if matches!(
            keyword.spelling(),
            "abstract_global_path"
                | "do"
                | "if"
                | "match"
                | "module"
                | "module_origin"
                | "self"
                | "try"
                | "try_match"
                | "using"
        ) {
            continue;
        }
        let source = keyword.spelling();
        let diagnostics = parse_expression_fragment(source.as_bytes())
            .expect_err("a bare keyword should not parse as an ordinary name");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.message.contains(keyword.spelling())
                    && diagnostic.message.contains("reserved keyword")
                    && diagnostic
                        .message
                        .contains(&format!("'{}", keyword.spelling()))
            }),
            "missing keyword escape diagnostic for `{}`: {diagnostics:?}",
            keyword.spelling()
        );
    }

    let do_diagnostics = parse_expression_fragment(b"do")
        .expect_err("a bare `do` should be diagnosed as a malformed do expression");
    assert!(do_diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("do expression requires a newline-delimited block")
    }));

    let if_diagnostics = parse_expression_fragment(b"if")
        .expect_err("a bare `if` should be diagnosed as a malformed if expression");
    assert!(if_diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("if expression requires `then` and `else`")
    }));

    let match_diagnostics = parse_expression_fragment(b"match")
        .expect_err("a bare `match` should be diagnosed as a malformed match expression");
    assert!(match_diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("match expression requires `with`")
    }));

    let try_diagnostics = parse_expression_fragment(b"try")
        .expect_err("a bare `try` should be diagnosed as a malformed try expression");
    assert!(try_diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("try expression requires `then` and `else`")
    }));

    let try_match_diagnostics = parse_expression_fragment(b"try_match")
        .expect_err("a bare `try_match` should be diagnosed as a malformed try_match expression");
    assert!(try_match_diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("try_match expression requires `with`")
    }));

    let using_diagnostics = parse_expression_fragment(b"using")
        .expect_err("a bare `using` should be diagnosed as a malformed using expression");
    assert!(using_diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("using expression requires a namespace")
    }));
}

#[test]
fn module_origin_is_a_special_reference_without_a_prior_form() {
    assert_eq!(
        parse_expression_fragment(b"module_origin").unwrap(),
        SyntaxExpr::Name("module_origin".to_owned())
    );
    assert_rejects("_module_origin");
}

#[test]
fn abstract_global_path_parses_only_static_relative_name_paths() {
    assert_eq!(
        parse_expression_fragment(b"abstract_global_path foo.bar").unwrap(),
        SyntaxExpr::AbstractGlobalPath {
            explicit_module: false,
            path: vec!["foo".to_owned(), "bar".to_owned()],
        }
    );
    assert_eq!(
        parse_expression_fragment(b"abstract_global_path module.foo.bar").unwrap(),
        SyntaxExpr::AbstractGlobalPath {
            explicit_module: true,
            path: vec!["foo".to_owned(), "bar".to_owned()],
        }
    );

    for source in [
        "abstract_global_path",
        "abstract_global_path module",
        "abstract_global_path self.foo",
        "abstract_global_path foo.[42]",
        "_abstract_global_path",
    ] {
        assert_rejects(source);
    }
}

#[test]
fn keyword_data_escapes_remain_available() {
    for keyword in crate::g_syntax::keywords::G0_KEYWORDS {
        assert_parses(&format!("'{}", keyword.spelling()));
        assert_parses(&format!("root.{}", keyword.spelling()));
        assert_parses(&format!("root.['{}]", keyword.spelling()));
        assert_parses(&format!("{}:()", keyword.spelling()));
        assert_parses(&format!(":{}", keyword.spelling()));
    }
}

#[test]
fn key_path_items_are_ordinary_expressions() {
    let SyntaxExpr::PathDict(path, _) = parse_expression_fragment(b"['a, ('b), 'c d]:v").unwrap()
    else {
        panic!("a computed tag should parse as a path dictionary");
    };
    // Constant atoms become static keys however they are written.
    assert_eq!(
        path,
        vec![
            SyntaxKeyExpr::Atom("a".to_owned()),
            SyntaxKeyExpr::Atom("b".to_owned()),
            SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Atom("c".to_owned())),
                Box::new(SyntaxExpr::Name("d".to_owned())),
            ))),
        ]
    );
}

#[test]
fn dict_path_member_colons_are_joint_to_their_paths() {
    assert_eq!(
        parse_expression_fragment(b"{a:f x}").unwrap(),
        SyntaxExpr::DictUnion(vec![SyntaxExpr::PathDict(
            vec![SyntaxKeyExpr::Atom("a".to_owned())],
            Box::new(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Name("f".to_owned())),
                Box::new(SyntaxExpr::Name("x".to_owned())),
            )),
        )])
    );
    // A space may follow the colon, never precede it.
    assert_eq!(
        parse_expression_fragment(b"{a: 1}").unwrap(),
        parse_expression_fragment(b"{a:\n  1}").unwrap(),
    );
    // Colon joint to the name instead: an expression member applying `f`
    // to a constructor.
    assert_eq!(
        parse_expression_fragment(b"{f :tag}").unwrap(),
        SyntaxExpr::DictUnion(vec![SyntaxExpr::Apply(
            Box::new(SyntaxExpr::Name("f".to_owned())),
            Box::new(SyntaxExpr::TaggedConstructor(vec![SyntaxKeyExpr::Atom(
                "tag".to_owned()
            )])),
        )])
    );
    for source in ["{a : 1}", "{a\n  :1}", "{a\n  : 1}", "{a:}"] {
        assert_rejects(source);
    }
}

#[test]
fn a_joint_colon_after_an_operator_name_makes_a_tag() {
    assert_eq!(
        parse_expression_fragment(b"(and:a)").unwrap(),
        SyntaxExpr::PathDict(
            vec![SyntaxKeyExpr::Atom("and".to_owned())],
            Box::new(SyntaxExpr::Name("a".to_owned())),
        )
    );
    assert!(matches!(
        parse_expression_fragment(b"(and :a)").unwrap(),
        SyntaxExpr::OperatorSection { left: None, .. }
    ));
}

#[test]
fn ordinary_expression_rejections_are_preserved() {
    for source in [
        "and",
        "tag: value",
        ": tag",
        "(,,)",
        "(first,,second)",
        "(,,first)",
        "[first,,second]",
        "{first,,second}",
        "first !> function <! argument",
        "1 + 2 * 3",
        "1 * 2 + 3",
        "1 * 2 / 3",
        "1 / 2 * 3",
        "x and y or z",
        "x or y and z",
        "1 / 2 / 3",
        "foo. bar",
        "1e999999999999999999999",
    ] {
        assert_rejects(source);
    }
}

#[test]
fn operator_relation_diagnostic_is_preserved() {
    for source in [
        "first !> function <! argument",
        "1 + 2 * 3",
        "1 * 2 / 3",
        "left and middle or right",
    ] {
        let diagnostics = parse_expression_fragment(source.as_bytes())
            .expect_err("unrelated operators should require parentheses");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("no precedence relationship")),
            "missing operator-relation diagnostic for `{source}`: {diagnostics:?}"
        );
    }
}

#[test]
fn invalid_number_diagnostic_is_preserved() {
    let diagnostics = parse_expression_fragment(b"1e999999999999999999999")
        .expect_err("an exponent beyond the supported bound must be rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("invalid number literal")),
        "{diagnostics:?}"
    );
}

#[test]
fn non_associative_operator_diagnostic_is_preserved() {
    let diagnostics =
        parse_expression_fragment(b"1 / 2 / 3").expect_err("division chains need parentheses");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("non-associative")),
        "{diagnostics:?}"
    );
}
