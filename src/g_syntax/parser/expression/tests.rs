use super::*;
use crate::core::Builtin;

fn n(value: i64) -> Number {
    value.into()
}

#[test]
fn parses_escaped_object_scope_names() {
    assert_eq!(
        parse_expr("^prefix.value"),
        Some(SyntaxExpr::Escape(
            1,
            Box::new(SyntaxExpr::Access(
                Box::new(SyntaxExpr::Name("prefix".to_owned())),
                vec![SyntaxKeyExpr::Atom("value".to_owned())],
            )),
        ))
    );
    assert_eq!(
        parse_expr("^^prefix"),
        Some(SyntaxExpr::Escape(
            2,
            Box::new(SyntaxExpr::Name("prefix".to_owned())),
        ))
    );
    assert_eq!(
        parse_expr("^(prefix ++ suffix).tail"),
        Some(SyntaxExpr::Access(
            Box::new(SyntaxExpr::Escape(
                1,
                Box::new(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::Name("prefix".to_owned())),
                    Box::new(SyntaxExpr::Name("suffix".to_owned())),
                )),
            )),
            vec![SyntaxKeyExpr::Atom("tail".to_owned())],
        ))
    );
}

#[test]
fn parses_effect_shorthand_expressions() {
    assert_eq!(parse_expr("()"), Some(SyntaxExpr::Unit));
    assert_eq!(
        parse_expr(".emit"),
        Some(SyntaxExpr::Effect(vec!["emit".to_owned()]))
    );
    assert_eq!(
        parse_expr(".heap.get"),
        Some(SyntaxExpr::Effect(vec![
            "heap".to_owned(),
            "get".to_owned()
        ]))
    );
    assert_eq!(
        parse_expr(".emit 'eax 42"),
        Some(SyntaxExpr::Apply(
            Box::new(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Effect(vec!["emit".to_owned()])),
                Box::new(SyntaxExpr::Atom("eax".to_owned())),
            )),
            Box::new(SyntaxExpr::Number(n(42))),
        ))
    );
}

#[test]
fn parses_quoted_paths_as_list_expressions() {
    assert_eq!(
        parse_expr("'.foo.[42]"),
        Some(SyntaxExpr::List(vec![
            SyntaxExpr::Atom("foo".to_owned()),
            SyntaxExpr::Number(n(42)),
        ]))
    );
    assert_eq!(
        parse_expr("'.foo.([1, 2]).bar"),
        Some(SyntaxExpr::Append(
            Box::new(SyntaxExpr::Append(
                Box::new(SyntaxExpr::List(vec![SyntaxExpr::Atom("foo".to_owned())])),
                Box::new(SyntaxExpr::List(vec![
                    SyntaxExpr::Number(n(1)),
                    SyntaxExpr::Number(n(2)),
                ])),
            )),
            Box::new(SyntaxExpr::List(vec![SyntaxExpr::Atom("bar".to_owned())])),
        ))
    );
    assert_eq!(parse_expr("'.[]"), Some(SyntaxExpr::List(Vec::new())));
    assert_eq!(parse_expr("'foo"), Some(SyntaxExpr::Atom("foo".to_owned())));
}

#[test]
fn parses_operator_sections() {
    assert_eq!(
        parse_expr("(+ 42)"),
        Some(SyntaxExpr::OperatorSection {
            operator: SyntaxOperator::Builtin(Builtin::Add),
            left: None,
            right: Some(Box::new(SyntaxExpr::Number(n(42)))),
        })
    );
    assert_eq!(
        parse_expr("(42 -)"),
        Some(SyntaxExpr::OperatorSection {
            operator: SyntaxOperator::Builtin(Builtin::Subtract),
            left: Some(Box::new(SyntaxExpr::Number(n(42)))),
            right: None,
        })
    );
    assert_eq!(
        parse_expr("(++ suffix)"),
        Some(SyntaxExpr::OperatorSection {
            operator: SyntaxOperator::Builtin(Builtin::Append),
            left: None,
            right: Some(Box::new(SyntaxExpr::Name("suffix".to_owned()))),
        })
    );
    assert_eq!(
        parse_expr("(+)"),
        Some(SyntaxExpr::OperatorSection {
            operator: SyntaxOperator::Builtin(Builtin::Add),
            left: None,
            right: None,
        })
    );
}

#[test]
fn parses_pipe_and_composition_operators() {
    assert_eq!(
        parse_expr("value |> f"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::PipeForward,
            left: Box::new(SyntaxExpr::Name("value".to_owned())),
            right: Box::new(SyntaxExpr::Name("f".to_owned())),
        })
    );
    assert_eq!(
        parse_expr("f <| value"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::PipeBackward,
            left: Box::new(SyntaxExpr::Name("f".to_owned())),
            right: Box::new(SyntaxExpr::Name("value".to_owned())),
        })
    );
    assert_eq!(
        parse_expr("f >> g"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::ComposeForward,
            left: Box::new(SyntaxExpr::Name("f".to_owned())),
            right: Box::new(SyntaxExpr::Name("g".to_owned())),
        })
    );
    assert_eq!(
        parse_expr("g << f"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::ComposeBackward,
            left: Box::new(SyntaxExpr::Name("g".to_owned())),
            right: Box::new(SyntaxExpr::Name("f".to_owned())),
        })
    );
    assert_eq!(
        parse_expr("op >>= k"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::EffectBind,
            left: Box::new(SyntaxExpr::Name("op".to_owned())),
            right: Box::new(SyntaxExpr::Name("k".to_owned())),
        })
    );
    assert_eq!(
        parse_expr("k1 >=> k2"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::KleisliCompose,
            left: Box::new(SyntaxExpr::Name("k1".to_owned())),
            right: Box::new(SyntaxExpr::Name("k2".to_owned())),
        })
    );
    assert_eq!(
        parse_expr("op =>> next"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::EffectThen,
            left: Box::new(SyntaxExpr::Name("op".to_owned())),
            right: Box::new(SyntaxExpr::Name("next".to_owned())),
        })
    );
    assert_eq!(
        parse_expr("(|> f)"),
        Some(SyntaxExpr::OperatorSection {
            operator: SyntaxOperator::PipeForward,
            left: None,
            right: Some(Box::new(SyntaxExpr::Name("f".to_owned()))),
        })
    );
    assert_eq!(
        parse_expr("(>>)"),
        Some(SyntaxExpr::OperatorSection {
            operator: SyntaxOperator::ComposeForward,
            left: None,
            right: None,
        })
    );
    assert_eq!(
        parse_expr("(>>= k)"),
        Some(SyntaxExpr::OperatorSection {
            operator: SyntaxOperator::EffectBind,
            left: None,
            right: Some(Box::new(SyntaxExpr::Name("k".to_owned()))),
        })
    );
}

#[test]
fn parses_comparison_and_boolean_operators() {
    assert_eq!(
        parse_expr("x < y"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::Builtin(Builtin::Less),
            left: Box::new(SyntaxExpr::Name("x".to_owned())),
            right: Box::new(SyntaxExpr::Name("y".to_owned())),
        })
    );
    assert_eq!(
        parse_expr("x >= y"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::Builtin(Builtin::GreaterEqual),
            left: Box::new(SyntaxExpr::Name("x".to_owned())),
            right: Box::new(SyntaxExpr::Name("y".to_owned())),
        })
    );
    assert_eq!(
        parse_expr("x and y or z"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::BoolOr,
            left: Box::new(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::BoolAnd,
                left: Box::new(SyntaxExpr::Name("x".to_owned())),
                right: Box::new(SyntaxExpr::Name("y".to_owned())),
            }),
            right: Box::new(SyntaxExpr::Name("z".to_owned())),
        })
    );
    assert_eq!(
        parse_expr("x < y =< z"),
        Some(SyntaxExpr::ComparisonChain {
            first: Box::new(SyntaxExpr::Name("x".to_owned())),
            rest: vec![
                (
                    SyntaxOperator::Builtin(Builtin::Less),
                    SyntaxExpr::Name("y".to_owned()),
                ),
                (
                    SyntaxOperator::Builtin(Builtin::LessEqual),
                    SyntaxExpr::Name("z".to_owned()),
                ),
            ],
        })
    );
    assert_eq!(parse_expr("and"), None);
    assert_eq!(
        parse_expr("android"),
        Some(SyntaxExpr::Name("android".to_owned()))
    );
    assert_eq!(parse_expr("'and"), Some(SyntaxExpr::Atom("and".to_owned())));
}

#[test]
fn parses_let_and_where_expressions() {
    assert_eq!(
        parse_expr("let x = 1 in x + x"),
        Some(SyntaxExpr::Let {
            bindings: vec![("x".to_owned(), SyntaxExpr::Number(n(1)))],
            body: Box::new(SyntaxExpr::Add(
                Box::new(SyntaxExpr::Name("x".to_owned())),
                Box::new(SyntaxExpr::Name("x".to_owned())),
            )),
        })
    );
    assert_eq!(
        parse_expr("let x = 1; _y = 2 in x"),
        Some(SyntaxExpr::Let {
            bindings: vec![
                ("x".to_owned(), SyntaxExpr::Number(n(1))),
                ("_y".to_owned(), SyntaxExpr::Number(n(2))),
            ],
            body: Box::new(SyntaxExpr::Name("x".to_owned())),
        })
    );
    assert_eq!(
        parse_expr("let x = 1\n    y = 2\nx + y"),
        Some(SyntaxExpr::Let {
            bindings: vec![
                ("x".to_owned(), SyntaxExpr::Number(n(1))),
                ("y".to_owned(), SyntaxExpr::Number(n(2))),
            ],
            body: Box::new(SyntaxExpr::Add(
                Box::new(SyntaxExpr::Name("x".to_owned())),
                Box::new(SyntaxExpr::Name("y".to_owned())),
            )),
        })
    );
    assert_eq!(
        parse_expr("x + y where x = 1; y = 2"),
        Some(SyntaxExpr::Let {
            bindings: vec![
                ("x".to_owned(), SyntaxExpr::Number(n(1))),
                ("y".to_owned(), SyntaxExpr::Number(n(2))),
            ],
            body: Box::new(SyntaxExpr::Add(
                Box::new(SyntaxExpr::Name("x".to_owned())),
                Box::new(SyntaxExpr::Name("y".to_owned())),
            )),
        })
    );
}

#[test]
fn dotted_paths_require_tight_dots() {
    assert!(matches!(
        parse_expr("foo.[  42  ].bar"),
        Some(SyntaxExpr::Access(_, _))
    ));
    assert!(matches!(
        parse_expr("foo.([1,2] ++ [3]).bar"),
        Some(SyntaxExpr::Access(_, _))
    ));

    assert_eq!(
        parse_expr("foo  .[42].bar"),
        None,
        "whitespace before `.` should be rejected"
    );
    assert_eq!(
        parse_expr("foo .bar"),
        Some(SyntaxExpr::Apply(
            Box::new(SyntaxExpr::Name("foo".to_owned())),
            Box::new(SyntaxExpr::Effect(vec!["bar".to_owned()])),
        )),
        "whitespace before `.` should parse `.bar` as a separate effect expression"
    );
    assert_eq!(
        parse_expr("foo.[42].  bar"),
        None,
        "whitespace after `.` should be rejected"
    );
    assert_eq!(
        parse_expr("foo. bar"),
        None,
        "whitespace after `.` should prevent dotted-path parsing"
    );
    assert_eq!(
        parse_expr("foo. [42].bar"),
        None,
        "whitespace between `.` and `[` should be rejected"
    );
    assert_eq!(
        parse_expr("foo. ([1,2] ++ [3]).bar"),
        None,
        "whitespace between `.` and `(` should be rejected"
    );
}

#[test]
fn parses_dotted_paths_on_literal_expressions() {
    assert_eq!(
        parse_expr("{ hello:\"Hello\" }.hello"),
        Some(SyntaxExpr::Access(
            Box::new(SyntaxExpr::DictUnion(vec![SyntaxExpr::SingletonDict(
                SyntaxKeyExpr::Atom("hello".to_owned()),
                Box::new(SyntaxExpr::Text("Hello".to_owned())),
            )])),
            vec![SyntaxKeyExpr::Atom("hello".to_owned())],
        ))
    );
    assert_eq!(
        parse_expr("[\"Hello\"].[0]"),
        Some(SyntaxExpr::Access(
            Box::new(SyntaxExpr::List(vec![SyntaxExpr::Text("Hello".to_owned())])),
            vec![SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(0))))],
        ))
    );
    assert_eq!(
        parse_expr("(foo).bar"),
        Some(SyntaxExpr::Access(
            Box::new(SyntaxExpr::Name("foo".to_owned())),
            vec![SyntaxKeyExpr::Atom("bar".to_owned())],
        ))
    );
}

#[test]
fn parses_dictionary_with_expressions() {
    assert!(matches!(
    parse_expr("{ x:1 } with\nx := 2\ny = x + 1"),
    Some(SyntaxExpr::With {
    alias: None,
    body,
    ..
    }) if body.len() == 2
    ));
    assert!(matches!(
    parse_expr("d as prior with\nx := _prior.x + 1"),
    Some(SyntaxExpr::With {
    alias: Some(alias),
    body,
    ..
    }) if alias == "prior" && body.len() == 1
    ));
    assert!(matches!(
    parse_expr("d as _prior with\nx := _prior.x + 1"),
    Some(SyntaxExpr::With {
    alias: Some(alias),
    body,
    ..
    }) if alias == "_prior" && body.len() == 1
    ));
    assert!(matches!(
    parse_expr("d as _ with\nx := 1"),
    Some(SyntaxExpr::With {
    alias: None,
    body,
    ..
    }) if body.len() == 1
    ));
}

#[test]
fn parentheses_disambiguate_division_chains() {
    assert_eq!(parse_expr("3/4/5"), None);
    assert_eq!(parse_expr("3/4 / 5"), None);
    assert_eq!(
        parse_expr("(3/4) / 5"),
        Some(SyntaxExpr::Divide(
            Box::new(SyntaxExpr::Divide(
                Box::new(SyntaxExpr::Number(n(3))),
                Box::new(SyntaxExpr::Number(n(4))),
            )),
            Box::new(SyntaxExpr::Number(n(5))),
        ))
    );
    assert_eq!(
        parse_expr("3 / (4/5)"),
        Some(SyntaxExpr::Divide(
            Box::new(SyntaxExpr::Number(n(3))),
            Box::new(SyntaxExpr::Divide(
                Box::new(SyntaxExpr::Number(n(4))),
                Box::new(SyntaxExpr::Number(n(5))),
            )),
        ))
    );
    assert_eq!(
        parse_expr("2 * 3 / 4"),
        Some(SyntaxExpr::Divide(
            Box::new(SyntaxExpr::Multiply(
                Box::new(SyntaxExpr::Number(n(2))),
                Box::new(SyntaxExpr::Number(n(3))),
            )),
            Box::new(SyntaxExpr::Number(n(4))),
        ))
    );
    assert_eq!(parse_expr("3 - 4 - 5"), None);
    assert_eq!(
        parse_expr("(3 - 4) - 5"),
        Some(SyntaxExpr::Subtract(
            Box::new(SyntaxExpr::Subtract(
                Box::new(SyntaxExpr::Number(n(3))),
                Box::new(SyntaxExpr::Number(n(4))),
            )),
            Box::new(SyntaxExpr::Number(n(5))),
        ))
    );
    assert_eq!(
        parse_expr("3 - (4 - 5)"),
        Some(SyntaxExpr::Subtract(
            Box::new(SyntaxExpr::Number(n(3))),
            Box::new(SyntaxExpr::Subtract(
                Box::new(SyntaxExpr::Number(n(4))),
                Box::new(SyntaxExpr::Number(n(5))),
            )),
        ))
    );
    assert_eq!(parse_expr("3 + 4 - 5"), None);
    assert_eq!(
        parse_expr("(3 + 4) - 5"),
        Some(SyntaxExpr::Subtract(
            Box::new(SyntaxExpr::Add(
                Box::new(SyntaxExpr::Number(n(3))),
                Box::new(SyntaxExpr::Number(n(4))),
            )),
            Box::new(SyntaxExpr::Number(n(5))),
        ))
    );
    assert_eq!(
        parse_expr("3 + (4 - 5)"),
        Some(SyntaxExpr::Add(
            Box::new(SyntaxExpr::Number(n(3))),
            Box::new(SyntaxExpr::Subtract(
                Box::new(SyntaxExpr::Number(n(4))),
                Box::new(SyntaxExpr::Number(n(5))),
            )),
        ))
    );
}
