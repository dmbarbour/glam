use super::*;
use crate::core::Builtin;

fn n(value: i64) -> Number {
    value.into()
}

#[test]
fn parses_multiline_text_blocks_without_a_final_line_feed() {
    assert_eq!(
        parse_expr("\"\"\"\n\" first  \n\" second # retained\n\" \"\"\" retained\n\"\"\""),
        Some(SyntaxExpr::Text(
            "first  \nsecond # retained\n\"\"\" retained".to_owned()
        ))
    );
    assert_eq!(
        parse_expr("\"\"\"\n\" ordinary \"quotes\" are retained\n\"\"\""),
        Some(SyntaxExpr::Text(
            "ordinary \"quotes\" are retained".to_owned()
        ))
    );
    assert_eq!(
        parse_expr("\"\"\"\n\"\"\""),
        Some(SyntaxExpr::Text(String::new()))
    );
    assert_eq!(parse_expr("\"\"\"\n\"missing separator\n\"\"\""), None);
}

#[test]
fn parses_tagged_data_and_constructors() {
    assert_eq!(
        parse_expr("tag:value"),
        Some(SyntaxExpr::PathDict(
            vec![SyntaxKeyExpr::Atom("tag".to_owned())],
            Box::new(SyntaxExpr::Name("value".to_owned())),
        ))
    );
    assert_eq!(
        parse_expr("[tag]:value"),
        Some(SyntaxExpr::PathDict(
            vec![SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Name(
                "tag".to_owned(),
            )))],
            Box::new(SyntaxExpr::Name("value".to_owned())),
        ))
    );
    assert_eq!(
        parse_expr(":tag"),
        Some(SyntaxExpr::TaggedConstructor(vec![SyntaxKeyExpr::Atom(
            "tag".to_owned(),
        )]))
    );
    assert_eq!(
        parse_expr(":[tag]"),
        Some(SyntaxExpr::TaggedConstructor(vec![SyntaxKeyExpr::Index(
            Box::new(SyntaxExpr::Name("tag".to_owned())),
        )]))
    );
    assert_eq!(
        parse_expr("outer:inner:value"),
        Some(SyntaxExpr::PathDict(
            vec![SyntaxKeyExpr::Atom("outer".to_owned())],
            Box::new(SyntaxExpr::PathDict(
                vec![SyntaxKeyExpr::Atom("inner".to_owned())],
                Box::new(SyntaxExpr::Name("value".to_owned())),
            )),
        ))
    );
    assert_eq!(
        parse_expr("foo.bar:value"),
        Some(SyntaxExpr::PathDict(
            vec![
                SyntaxKeyExpr::Atom("foo".to_owned()),
                SyntaxKeyExpr::Atom("bar".to_owned()),
            ],
            Box::new(SyntaxExpr::Name("value".to_owned())),
        ))
    );
    assert_eq!(
        parse_expr("[first,second]:value"),
        Some(SyntaxExpr::PathDict(
            vec![
                SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Name("first".to_owned()))),
                SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Name("second".to_owned()))),
            ],
            Box::new(SyntaxExpr::Name("value".to_owned())),
        ))
    );
    assert_eq!(
        parse_expr("([first] ++ tail):value"),
        Some(SyntaxExpr::PathDict(
            vec![SyntaxKeyExpr::PathIndex(Box::new(SyntaxExpr::Append(
                Box::new(SyntaxExpr::List(vec![
                    SyntaxExpr::Name("first".to_owned(),)
                ])),
                Box::new(SyntaxExpr::Name("tail".to_owned())),
            )))],
            Box::new(SyntaxExpr::Name("value".to_owned())),
        ))
    );
    assert_eq!(
        parse_expr(":foo.bar"),
        Some(SyntaxExpr::TaggedConstructor(vec![
            SyntaxKeyExpr::Atom("foo".to_owned()),
            SyntaxKeyExpr::Atom("bar".to_owned()),
        ]))
    );
    assert_eq!(
        parse_expr(":[first,second]"),
        Some(SyntaxExpr::TaggedConstructor(vec![
            SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Name("first".to_owned()))),
            SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Name("second".to_owned()))),
        ]))
    );
    assert_eq!(
        parse_expr(":([first] ++ tail)"),
        Some(SyntaxExpr::TaggedConstructor(vec![
            SyntaxKeyExpr::PathIndex(Box::new(SyntaxExpr::Append(
                Box::new(SyntaxExpr::List(vec![
                    SyntaxExpr::Name("first".to_owned(),)
                ])),
                Box::new(SyntaxExpr::Name("tail".to_owned())),
            ))),
        ]))
    );
}

#[test]
fn tagged_payloads_are_single_application_atoms() {
    assert_eq!(
        parse_expr("g tag:f x"),
        Some(SyntaxExpr::Apply(
            Box::new(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Name("g".to_owned())),
                Box::new(SyntaxExpr::PathDict(
                    vec![SyntaxKeyExpr::Atom("tag".to_owned())],
                    Box::new(SyntaxExpr::Name("f".to_owned())),
                )),
            )),
            Box::new(SyntaxExpr::Name("x".to_owned())),
        ))
    );
    assert_eq!(
        parse_expr("tag:(f x)"),
        Some(SyntaxExpr::PathDict(
            vec![SyntaxKeyExpr::Atom("tag".to_owned())],
            Box::new(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Name("f".to_owned())),
                Box::new(SyntaxExpr::Name("x".to_owned())),
            )),
        ))
    );
    assert_eq!(parse_expr("tag: value"), None);
    assert_eq!(parse_expr(": tag"), None);
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
fn parses_tuple_expressions_without_changing_grouping_or_unit() {
    assert_eq!(
        parse_expr("(first, second)"),
        Some(SyntaxExpr::Tuple(vec![
            SyntaxExpr::Name("first".to_owned()),
            SyntaxExpr::Name("second".to_owned()),
        ]))
    );
    assert_eq!(
        parse_expr("(first, second,)"),
        Some(SyntaxExpr::Tuple(vec![
            SyntaxExpr::Name("first".to_owned()),
            SyntaxExpr::Name("second".to_owned()),
        ]))
    );
    assert_eq!(
        parse_expr("(grouped)"),
        Some(SyntaxExpr::Name("grouped".to_owned()))
    );
    assert_eq!(parse_expr("()"), Some(SyntaxExpr::Unit));
    assert_eq!(parse_expr("(singleton,)"), None);
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
    assert_eq!(
        parse_expr("(!>)"),
        Some(SyntaxExpr::OperatorSection {
            operator: SyntaxOperator::ApplicativeForward,
            left: None,
            right: None,
        })
    );
    assert_eq!(
        parse_expr("(<!)"),
        Some(SyntaxExpr::OperatorSection {
            operator: SyntaxOperator::ApplicativeBackward,
            left: None,
            right: None,
        })
    );
}

#[test]
fn parses_applicatives_in_their_written_directions() {
    let named = |name: &str| SyntaxExpr::Name(name.to_owned());
    assert_eq!(
        parse_expr("function <! first <! second"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::ApplicativeBackward,
            left: Box::new(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::ApplicativeBackward,
                left: Box::new(named("function")),
                right: Box::new(named("first")),
            }),
            right: Box::new(named("second")),
        })
    );
    assert_eq!(
        parse_expr("first !> second !> function"),
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::ApplicativeForward,
            left: Box::new(named("first")),
            right: Box::new(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::ApplicativeForward,
                left: Box::new(named("second")),
                right: Box::new(named("function")),
            }),
        })
    );
    assert!(
        parse_expr_result("first !> function <! argument")
            .expect_err("opposing applicative directions need parentheses")
            .contains("no precedence relationship")
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
            Box::new(SyntaxExpr::DictUnion(vec![SyntaxExpr::PathDict(
                vec![SyntaxKeyExpr::Atom("hello".to_owned())],
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
fn parses_general_dictionary_entry_paths() {
    assert_eq!(
        parse_expr("{ [0]:zero, [1,2]:deep, ([1] ++ [3,4]):computed, named.path:value }"),
        Some(SyntaxExpr::DictUnion(vec![
            SyntaxExpr::PathDict(
                vec![SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(0))))],
                Box::new(SyntaxExpr::Name("zero".to_owned())),
            ),
            SyntaxExpr::PathDict(
                vec![
                    SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(1)))),
                    SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(2)))),
                ],
                Box::new(SyntaxExpr::Name("deep".to_owned())),
            ),
            SyntaxExpr::PathDict(
                vec![SyntaxKeyExpr::PathIndex(Box::new(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::List(vec![SyntaxExpr::Number(n(1))])),
                    Box::new(SyntaxExpr::List(vec![
                        SyntaxExpr::Number(n(3)),
                        SyntaxExpr::Number(n(4)),
                    ])),
                )))],
                Box::new(SyntaxExpr::Name("computed".to_owned())),
            ),
            SyntaxExpr::PathDict(
                vec![
                    SyntaxKeyExpr::Atom("named".to_owned()),
                    SyntaxKeyExpr::Atom("path".to_owned()),
                ],
                Box::new(SyntaxExpr::Name("value".to_owned())),
            ),
        ])),
    );
    assert_eq!(
        parse_expr("{ [1,2], (path) }"),
        Some(SyntaxExpr::DictUnion(vec![
            SyntaxExpr::List(vec![SyntaxExpr::Number(n(1)), SyntaxExpr::Number(n(2))]),
            SyntaxExpr::Name("path".to_owned()),
        ])),
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
