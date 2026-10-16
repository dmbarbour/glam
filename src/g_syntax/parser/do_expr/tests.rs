use crate::g_syntax::{
    DoExpr, DoStepKind, SyntaxExpr, SyntaxGuardClause, SyntaxKeyExpr, SyntaxPattern,
    SyntaxPatternKind,
};

use super::super::structural::parse_compound_expression_fragment;

fn parse_do_expression(source: &str) -> SyntaxExpr {
    parse_compound_expression_fragment(source.as_bytes())
        .unwrap_or_else(|diagnostics| panic!("parser rejected `{source}`: {diagnostics:#?}"))
}

/// Asserts that `source` fails because a tail form in it stands outside
/// tail position (Syntax.md "Tail position").
#[track_caller]
fn assert_outside_tail_position(source: &str) {
    let diagnostics = parse_compound_expression_fragment(source.as_bytes())
        .expect_err("a tail form outside tail position fails");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("not in tail position")),
        "`{source}` reported {diagnostics:#?}"
    );
}

#[test]
fn layout_do_parses() {
    for source in [
        "do value",
        "do first <- .read\n   second <- .read\n   .r [first, second]",
        "do # the first significant member establishes next-line layout\n  first <- .read\n\n  # comments do not change the anchor\n  second <- .read\n  .r [first, second]",
        "do\n.read 'left -> left\nright <- .read 'right\ntotal = left + right\n.write total\n.r total",
        "do\ndeclare left, _right\n.r (\\_ -> left) -> use_left\nleft = 1\nright <- .r 2\n.r (use_left ())",
        "do\nvalue <- do\n  input <- source\n  .r input\nwritten <- write\n  value\n.r written",
        "interaction_net do\n.bind -> ports\n.r ports",
        "\\api -> do\n.r api",
        "\\api -> interaction_net do\n.bind -> ports\n.r ports",
        "(do .r 1)",
        "(do .r ()\n    .r 1)",
        "consume (do .r ()\n            .r 1)",
        "(do .r 1, do .r 2)",
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
        DoStepKind::Bind { pattern, operation }
            if pattern.captures() == ["function"]
                && matches!(operation, SyntaxExpr::Lambda(_, _))
    ));
    assert!(matches!(
        &steps[1].kind,
        DoStepKind::ValueBind { pattern, value }
            if pattern.captures() == ["identity"]
                && matches!(value, SyntaxExpr::Lambda(_, _))
    ));
}

/// A keyword form owns every `<-` and `=` after its head, since a pattern
/// holds no keyword form; each of these statements was once split at one.
#[test]
fn keyword_forms_own_their_binders() {
    for (source, form) in [
        ("do { if (a, b) = .r 1 then .r a else .r 0 }", "if"),
        ("do { if a <- .op then .r a else .r 0 }", "if"),
        ("do { let x = 1 in .r x }", "let"),
    ] {
        let SyntaxExpr::Do(DoExpr { steps, result, .. }) = parse_do_expression(source) else {
            panic!("expected a do expression");
        };
        assert!(steps.is_empty(), "{source}: {steps:?}");
        let parsed = match result.as_ref() {
            SyntaxExpr::If(_) => "if",
            SyntaxExpr::Let { .. } => "let",
            _ => "other",
        };
        assert_eq!(parsed, form, "{source}");
    }

    // A binder before the keyword's head binds, and `as` belongs to patterns.
    let SyntaxExpr::Do(DoExpr { steps, .. }) = parse_do_expression(
        "do { x = if (a, b) = .r 1 then a else 0; y <- if c then .a else .b; p as q <- .op; .r [x, y, p, q] }",
    ) else {
        panic!("expected a do expression");
    };
    assert!(matches!(
        &steps[0].kind,
        DoStepKind::ValueBind { pattern, value: SyntaxExpr::If(_) } if pattern.captures() == ["x"]
    ));
    assert!(matches!(
        &steps[1].kind,
        DoStepKind::Bind { pattern, operation: SyntaxExpr::If(_) } if pattern.captures() == ["y"]
    ));
    assert!(matches!(
        &steps[2].kind,
        DoStepKind::Bind { pattern, .. } if pattern.captures() == ["p", "q"]
    ));
}

/// In a binding, an open tail ends at a top-level `->`, a separator that a
/// pattern follows; elsewhere `->` after a tail is an error, as it was.
#[test]
fn open_tails_end_at_a_forward_binding() {
    for (source, form) in [
        ("do { if c then .a else .b -> y; .r y }", "if"),
        ("do { (if c then .a else .b) -> y; .r y }", "if"),
        ("do { .r 1 if c else .r 2 -> y; .r y }", "if"),
        ("do { let z = 1 in .r z -> y; .r y }", "let"),
        ("do { using m in .r 1 -> y; .r y }", "using"),
        (
            "do { if c then .a else do { .b -> z; .r z } -> y; .r y }",
            "if",
        ),
        ("do { .r 1 |> \\v -> v -> y; .r y }", "pipe"),
        ("do { .r 1 |> (\\v -> v) -> y; .r y }", "pipe"),
        ("do { .r d with { a := 1 } -> y; .r y }", "with"),
    ] {
        let SyntaxExpr::Do(DoExpr { steps, .. }) = parse_do_expression(source) else {
            panic!("expected a do expression");
        };
        let DoStepKind::Bind { pattern, operation } = &steps[0].kind else {
            panic!("{source}: {:?}", steps[0].kind);
        };
        assert_eq!(pattern.captures(), ["y"], "{source}");
        let parsed = match operation {
            SyntaxExpr::If(_) => "if",
            SyntaxExpr::Let { .. } => "let",
            SyntaxExpr::Using { .. } => "using",
            SyntaxExpr::OperatorApply { .. } => "pipe",
            SyntaxExpr::With { .. } => "with",
            _ => "other",
        };
        assert_eq!(parsed, form, "{source}");
    }

    // A layout block in an `else` keeps its own arrows.
    let SyntaxExpr::Do(DoExpr { steps, .. }) =
        parse_do_expression("do\n  if c then .a else do\n      .b -> z\n      .r z\n  .r 1")
    else {
        panic!("expected a do expression");
    };
    assert!(
        matches!(&steps[0].kind, DoStepKind::Then(SyntaxExpr::If(_))),
        "{:?}",
        steps[0].kind
    );

    assert!(parse_compound_expression_fragment(b"if c then a else b -> y").is_err());
}

/// Guards are an `and` chain whose operands may bind, so an `or` among
/// them is parenthesized, as in any `and` chain.
#[test]
fn guards_mix_or_and_and_only_in_parentheses() {
    let diagnostics = parse_compound_expression_fragment(b"if a or b and c then 1 else 2")
        .expect_err("`or` and `and` have no precedence relationship");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("no precedence relationship")),
        "{diagnostics:#?}"
    );
    for source in [
        "if a or b then 1 else 2",
        "if (a or b) and c then 1 else 2",
        "if x <- (a or b) and c then 1 else 2",
    ] {
        parse_do_expression(source);
    }
}

/// Guard clauses bind by the same rule as statements, split at `and`
/// wherever it stands as a keyword, but not as a tag's key.
#[test]
fn guard_clauses_bind_by_the_statement_rule() {
    let SyntaxExpr::If(conditional) = parse_do_expression(
        "if (a, b) = e and a > b and .op -> c and d <- .op and _ and f and:x then a else b",
    ) else {
        panic!("expected an if expression");
    };
    let clauses = conditional
        .guards
        .iter()
        .map(|guard| match guard {
            SyntaxGuardClause::ValueBind { .. } => "value",
            SyntaxGuardClause::EffectBind { .. } => "bind",
            SyntaxGuardClause::Effect(_) => "effect",
            SyntaxGuardClause::Pass => "pass",
        })
        .collect::<Vec<_>>();
    assert_eq!(
        clauses,
        ["value", "effect", "bind", "bind", "pass", "effect"]
    );
}

/// A conditional bound in a guard clause that another follows is
/// parenthesized: its `else` branch would end at the `and`.
#[test]
fn a_conditional_bound_in_guards_is_parenthesized() {
    assert_outside_tail_position("if x <- if a then b else c and y then 1 else 2");
    assert_outside_tail_position("match v as {p when x <- if a then b else c and y => 1}");
    let SyntaxExpr::If(conditional) =
        parse_do_expression("if x <- (if a then b else c) and y then 1 else 2")
    else {
        panic!("expected an if expression");
    };
    assert!(matches!(
        conditional.guards.as_slice(),
        [
            SyntaxGuardClause::EffectBind { operation: SyntaxExpr::If(inner), .. },
            SyntaxGuardClause::Effect(SyntaxExpr::Name(y)),
        ] if *inner.else_result == SyntaxExpr::Name("c".to_owned()) && y == "y"
    ));
    let SyntaxExpr::Match(found) =
        parse_do_expression("match v as {p when x <- (if a then b else c) and y => 1}")
    else {
        panic!("expected a match expression");
    };
    assert_eq!(found.arms[0].guards.len(), 2);
}

#[test]
fn initial_patterns_are_shared_by_all_do_binding_directions() {
    let expr = parse_do_expression(
        "do\n.r 1 -> (forward)\n(backward) <- .r 2\n((pure)) = 3\n_ <- .r 4\n.r [forward, backward, pure]",
    );
    let SyntaxExpr::Do(DoExpr { steps, .. }) = expr else {
        panic!("expected a do expression");
    };

    assert!(matches!(
        &steps[0].kind,
        DoStepKind::Bind { pattern, .. } if pattern.captures() == ["forward"]
    ));
    assert!(matches!(
        &steps[1].kind,
        DoStepKind::Bind { pattern, .. } if pattern.captures() == ["backward"]
    ));
    assert!(matches!(
        &steps[2].kind,
        DoStepKind::ValueBind { pattern, .. } if pattern.captures() == ["pure"]
    ));
    assert!(matches!(
        &steps[3].kind,
        DoStepKind::Bind { pattern, .. } if pattern.captures().is_empty()
    ));
}

#[test]
fn irrefutable_as_patterns_capture_each_view_in_source_order() {
    let expr = parse_do_expression(
        "do\n.r 1 -> left as right\n(first as _) <- .r 2\npure as second = 3\n.r [left, right, first, pure, second]",
    );
    let SyntaxExpr::Do(DoExpr { steps, .. }) = expr else {
        panic!("expected a do expression");
    };

    assert!(matches!(
        &steps[0].kind,
        DoStepKind::Bind { pattern, .. } if pattern.captures() == ["left", "right"]
    ));
    assert!(matches!(
        &steps[1].kind,
        DoStepKind::Bind { pattern, .. } if pattern.captures() == ["first"]
    ));
    assert!(matches!(
        &steps[2].kind,
        DoStepKind::ValueBind { pattern, .. } if pattern.captures() == ["pure", "second"]
    ));
}

#[test]
fn literal_list_and_quoted_path_patterns_parse_structurally() {
    let expr = parse_do_expression(concat!(
        "do\n",
        ".r () -> ()\n",
        ".r 42 -> 42\n",
        ".r 'tag -> 'tag\n",
        ".r \"text\" -> \"text\"\n",
        ".r [1,2,3,4] -> [1, first] ++ middle ++ [last, 4]\n",
        ".r ['foo,42,'bar] -> '.foo.[42,'bar]\n",
        ".r [first, middle, last]\n",
    ));
    let SyntaxExpr::Do(DoExpr { steps, .. }) = expr else {
        panic!("expected a do expression");
    };
    for step in steps.iter().take(4) {
        assert!(matches!(
            &step.kind,
            DoStepKind::Bind {
                pattern: crate::g_syntax::SyntaxPattern {
                    kind: SyntaxPatternKind::Literal(_),
                },
                ..
            }
        ));
    }
    assert!(matches!(
        &steps[4].kind,
        DoStepKind::Bind { pattern, .. }
            if pattern.captures() == ["first", "middle", "last"]
                && matches!(
                    &pattern.kind,
                    SyntaxPatternKind::List {
                        prefix,
                        middle: Some(_),
                        suffix,
                    } if prefix.len() == 2 && suffix.len() == 2
                )
    ));
    assert!(matches!(
        &steps[5].kind,
        DoStepKind::Bind { pattern, .. }
            if pattern.captures().is_empty()
                && matches!(
                    &pattern.kind,
                    SyntaxPatternKind::List {
                        prefix,
                        middle: None,
                        suffix,
                    } if prefix.len() == 3 && suffix.is_empty()
                )
    ));
}

#[test]
fn dictionary_tag_and_tuple_patterns_share_dictionary_structure() {
    let expr = parse_do_expression(concat!(
        "do\n",
        ".r {} -> {}\n",
        ".r {foo:{bar:1}} -> {foo.bar:value}\n",
        ".r {foo:1, other:2} -> {foo:first, rest}\n",
        ".r tag:1 -> tag:payload\n",
        ".r tag:1 -> {:tag}\n",
        ".r (1,2) -> (left,right)\n",
        ".r {foo:1} -> {whole}\n",
        "{:backward} <- .r {backward:3}\n",
        ".r {} -> {optional?:value}\n",
        ".r [value, first, rest, payload, tag, left, right, whole, backward]\n",
    ));
    let SyntaxExpr::Do(DoExpr { steps, .. }) = expr else {
        panic!("expected a do expression");
    };
    for step in &steps {
        if let DoStepKind::Bind { pattern, .. } = &step.kind {
            assert!(matches!(pattern.kind, SyntaxPatternKind::Dict { .. }));
        }
    }
    assert!(matches!(
        &steps[1].kind,
        DoStepKind::Bind { pattern, .. } if pattern.captures() == ["value"]
    ));
    assert!(matches!(
        &steps[2].kind,
        DoStepKind::Bind { pattern, .. } if pattern.captures() == ["first", "rest"]
    ));
    assert!(matches!(
        &steps[4].kind,
        DoStepKind::Bind { pattern, .. } if pattern.captures() == ["tag"]
    ));
    assert!(matches!(
        &steps[5].kind,
        DoStepKind::Bind { pattern, .. } if pattern.captures() == ["left", "right"]
    ));
    assert!(matches!(
        &steps[8].kind,
        DoStepKind::Bind {
            pattern: crate::g_syntax::SyntaxPattern {
                kind: SyntaxPatternKind::Dict { entries, .. },
            },
            ..
        } if entries.len() == 1
            && matches!(
                entries[0].path.as_slice(),
                [SyntaxKeyExpr::Atom(name)] if name == "optional"
            )
            && entries[0].optional
            && entries[0].pattern.captures() == ["value"]
    ));
}

#[test]
fn computed_dictionary_quoted_and_tag_paths_parse_as_owned_expressions() {
    let expr = parse_do_expression(concat!(
        "do\n",
        ".r {selector:'target,target:1} -> {selector:key, [key]:value}\n",
        ".r {root:{target:2}} -> {root.[key]:nested}\n",
        ".r {target:3} -> {(path):spliced}\n",
        ".r {} -> {[key]?:{}}\n",
        ".r (42,['foo,42]) -> (index, '.foo.[index])\n",
        ".r (['foo,42],['foo,42]) -> (whole_path, '.(whole_path))\n",
        ".r ('target,target:4) -> (tag, [tag]:tagged)\n",
        ".r (['root,'target],root.target:5) -> (tag_path, (tag_path):deep_tagged)\n",
        ".r [value, nested, spliced, index, tagged, deep_tagged]\n",
    ));
    let SyntaxExpr::Do(DoExpr { steps, .. }) = expr else {
        panic!("expected a do expression");
    };

    let DoStepKind::Bind { pattern, .. } = &steps[0].kind else {
        panic!("expected a dictionary-pattern binding");
    };
    let SyntaxPatternKind::Dict { entries, .. } = &pattern.kind else {
        panic!("expected a dictionary pattern");
    };
    assert_eq!(entries.len(), 2);
    assert!(matches!(
        entries[1].path.as_slice(),
        [SyntaxKeyExpr::Index(expr)] if matches!(expr.as_ref(), SyntaxExpr::Name(name) if name == "key")
    ));
    assert_eq!(pattern.captures(), ["key", "value"]);

    let DoStepKind::Bind { pattern, .. } = &steps[4].kind else {
        panic!("expected a tuple-pattern binding");
    };
    let SyntaxPatternKind::Dict { entries, .. } = &pattern.kind else {
        panic!("tuple patterns use dictionary structure");
    };
    let SyntaxPatternKind::List { prefix, .. } = &entries[0].pattern.kind else {
        panic!("tuple payload should be a fixed list pattern");
    };
    assert!(matches!(
        &prefix[1].kind,
        SyntaxPatternKind::QuotedPath(path)
            if matches!(
                path.as_slice(),
                [SyntaxKeyExpr::Atom(name), SyntaxKeyExpr::Index(expr)]
                    if name == "foo"
                        && matches!(expr.as_ref(), SyntaxExpr::Name(index) if index == "index")
            )
    ));

    let DoStepKind::Bind { pattern, .. } = &steps[6].kind else {
        panic!("expected a tuple-pattern binding");
    };
    let SyntaxPatternKind::Dict { entries, .. } = &pattern.kind else {
        panic!("tuple patterns use dictionary structure");
    };
    let SyntaxPatternKind::List { prefix, .. } = &entries[0].pattern.kind else {
        panic!("tuple payload should be a fixed list pattern");
    };
    assert!(matches!(
        &prefix[1].kind,
        SyntaxPatternKind::Dict { entries, .. }
            if matches!(
                entries[0].path.as_slice(),
                [SyntaxKeyExpr::Index(expr)]
                    if matches!(expr.as_ref(), SyntaxExpr::Name(name) if name == "tag")
            )
    ));
    assert_eq!(pattern.captures(), ["tag", "tagged"]);

    let DoStepKind::Bind { pattern, .. } = &steps[7].kind else {
        panic!("expected a tuple-pattern binding");
    };
    let SyntaxPatternKind::Dict { entries, .. } = &pattern.kind else {
        panic!("tuple patterns use dictionary structure");
    };
    let SyntaxPatternKind::List { prefix, .. } = &entries[0].pattern.kind else {
        panic!("tuple payload should be a fixed list pattern");
    };
    assert!(matches!(
        &prefix[1].kind,
        SyntaxPatternKind::Dict { entries, .. }
            if matches!(
                entries[0].path.as_slice(),
                [SyntaxKeyExpr::PathIndex(expr)]
                    if matches!(expr.as_ref(), SyntaxExpr::Name(name) if name == "tag_path")
            )
    ));
    assert_eq!(pattern.captures(), ["tag_path", "deep_tagged"]);
}

#[test]
fn dictionary_remainders_accept_refutable_patterns() {
    let expr = parse_do_expression(concat!(
        "do\n",
        ".r {first:1,second:2} -> {first:left,{second:right}}\n",
        ".r [left,right]\n",
    ));
    let SyntaxExpr::Do(DoExpr { steps, .. }) = expr else {
        panic!("expected a do expression");
    };
    assert!(matches!(
        &steps[0].kind,
        DoStepKind::Bind {
            pattern: SyntaxPattern {
                kind:
                    SyntaxPatternKind::Dict {
                        entries,
                        remainder: Some(remainder),
                    },
            },
            ..
        } if entries[0].pattern.captures() == ["left"]
            && matches!(&remainder.kind, SyntaxPatternKind::Dict { .. })
            && remainder.captures() == ["right"]
    ));
}

#[test]
fn variable_length_list_segments_accept_refutable_patterns() {
    let expr = parse_do_expression(concat!(
        "do\n",
        ".r [0,1,2,9] -> [0] ++ ([head] ++ tail) ++ [9]\n",
        ".r [42,'foo,42,9] -> [key] ++ '.foo.[key] ++ [9]\n",
        ".r [head,tail]\n",
    ));
    let SyntaxExpr::Do(DoExpr { steps, .. }) = expr else {
        panic!("expected a do expression");
    };

    let DoStepKind::Bind { pattern, .. } = &steps[0].kind else {
        panic!("expected a list-pattern binding");
    };
    let SyntaxPatternKind::List {
        prefix,
        middle: Some(middle),
        suffix,
    } = &pattern.kind
    else {
        panic!("expected a variable-length list pattern");
    };
    assert_eq!(prefix.len(), 1);
    assert_eq!(suffix.len(), 1);
    assert!(matches!(&middle.kind, SyntaxPatternKind::Group(_)));
    assert_eq!(middle.captures(), ["head", "tail"]);

    let DoStepKind::Bind { pattern, .. } = &steps[1].kind else {
        panic!("expected a list-pattern binding");
    };
    assert!(matches!(
        &pattern.kind,
        SyntaxPatternKind::List {
            middle: Some(middle),
            ..
        } if matches!(&middle.kind, SyntaxPatternKind::QuotedPath(_))
    ));
    assert_eq!(pattern.captures(), ["key"]);
}

#[test]
fn effectful_patterns_parse_with_progressive_capture_order() {
    let expr = parse_do_expression(concat!(
        "do\n",
        ".r [1,2] -> [left,2] as whole\n",
        ".r 3 -> (increment -> viewed)\n",
        ".r 4 -> (backward <- increment)\n",
        ".r 5 -> (positive kept)\n",
        ".r [6,7] -> ([first,second] when first < second and .r (second + 1) -> next and doubled = next + next)\n",
        ".r [left,whole,viewed,backward,kept,first,second,next,doubled]\n",
    ));
    let SyntaxExpr::Do(DoExpr { steps, .. }) = expr else {
        panic!("expected a do expression");
    };

    assert!(matches!(
        &steps[0].kind,
        DoStepKind::Bind {
            pattern: SyntaxPattern {
                kind: SyntaxPatternKind::As(left, right),
            },
            ..
        } if matches!(&left.kind, SyntaxPatternKind::List { .. })
            && right.captures() == ["whole"]
            && left.captures() == ["left"]
    ));
    assert!(matches!(
        &steps[1].kind,
        DoStepKind::Bind {
            pattern: SyntaxPattern {
                kind: SyntaxPatternKind::View { view, pattern },
            },
            ..
        } if matches!(view.as_ref(), SyntaxExpr::Name(name) if name == "increment")
            && pattern.captures() == ["viewed"]
    ));
    assert!(matches!(
        &steps[2].kind,
        DoStepKind::Bind {
            pattern: SyntaxPattern {
                kind: SyntaxPatternKind::View { view, pattern },
            },
            ..
        } if matches!(view.as_ref(), SyntaxExpr::Name(name) if name == "increment")
            && pattern.captures() == ["backward"]
    ));
    assert!(matches!(
        &steps[3].kind,
        DoStepKind::Bind {
            pattern: SyntaxPattern {
                kind: SyntaxPatternKind::Predicate { predicate, pattern },
            },
            ..
        } if matches!(predicate.as_ref(), SyntaxExpr::Name(name) if name == "positive")
            && pattern.captures() == ["kept"]
    ));
    let DoStepKind::Bind { pattern, .. } = &steps[4].kind else {
        panic!("expected a guarded pattern");
    };
    let SyntaxPatternKind::Guarded {
        pattern: guarded,
        guards,
    } = &pattern.kind
    else {
        panic!("expected a guarded pattern");
    };
    assert_eq!(guarded.captures(), ["first", "second"]);
    assert!(matches!(&guards[0], SyntaxGuardClause::Effect(_)));
    assert!(matches!(
        &guards[1],
        SyntaxGuardClause::EffectBind { pattern, .. } if pattern.captures() == ["next"]
    ));
    assert!(matches!(
        &guards[2],
        SyntaxGuardClause::ValueBind { pattern, .. } if pattern.captures() == ["doubled"]
    ));
    assert_eq!(pattern.captures(), ["first", "second", "next", "doubled"]);
}

#[test]
fn wildcard_pattern_guard_is_a_pass_clause() {
    let expr = parse_do_expression("do { .r 1 -> (_ when _); .r () }");
    let SyntaxExpr::Do(DoExpr { steps, .. }) = expr else {
        panic!("expected a do expression");
    };
    let DoStepKind::Bind { pattern, .. } = &steps[0].kind else {
        panic!("expected a guarded pattern binding");
    };
    let SyntaxPatternKind::Guarded { guards, .. } = &pattern.kind else {
        panic!("expected a guarded pattern");
    };

    assert_eq!(guards, &[SyntaxGuardClause::Pass]);
}

/// A `{` delimits a `do` block only on the `do`'s line; on the next line it
/// begins a statement, such as a binding of a dictionary pattern.
#[test]
fn a_brace_delimits_a_do_block_only_on_its_line() {
    let SyntaxExpr::Do(DoExpr { steps, .. }) =
        parse_do_expression("do\n  {a:y, b:z} <- .r v\n  .r y")
    else {
        panic!("expected a do expression");
    };
    assert!(matches!(
        &steps[0].kind,
        DoStepKind::Bind { pattern, .. } if pattern.captures() == ["y", "z"]
    ));
    let SyntaxExpr::Do(DoExpr { steps, .. }) = parse_do_expression("do {\n  x <- .r v;\n  .r x }")
    else {
        panic!("expected a do expression");
    };
    assert_eq!(steps.len(), 1);
}

/// A braced `do` block stands where its braces end it, as a dedent would:
/// only an operator or a keyword may follow it, not an argument or a path.
#[test]
fn braced_do_stands_in_containers_and_other_do_blocks() {
    assert_outside_tail_position("consume do { .r 1 } next");
    assert_outside_tail_position("do { .r {answer: 42} }.answer");
    for source in [
        "consume [do { .r 1 }, do { text = \"a;b\"; .r text }, do { x = do .r 2; .r x }]",
        "consume (do { .r 1 }) next",
        "result:do { .r 1 }",
        "(do { .r {answer: 42} }).answer",
        "do { .r 1 } >>= next",
        "(do { .r 1 }, do { .r 2 })",
        "do { nested <- do { .r 1 }; .r nested }",
        "do {\n; text = \"\"\"\n  \" semicolon; remains text\n\"\"\"\n; .r text\n}",
        "do {}",
        "do {   }",
    ] {
        parse_do_expression(source);
    }
}

/// A `where` heads no pattern, so a statement holding one before any `=` is
/// an operation, and the semicolon ends its bindings, parenthesized or not.
#[test]
fn braced_do_semicolons_separate_unparenthesized_where_expressions() {
    let diagnostics = parse_compound_expression_fragment(b"do { result where x = 1; y = 2 }")
        .expect_err("a do block cannot end with a binding");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("cannot end with a binding")),
        "{diagnostics:#?}"
    );

    for source in [
        "do { result where x = 1; y = 2; .r y }",
        "do { (result where x = 1); y = 2; .r y }",
    ] {
        let parsed = parse_do_expression(source);
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
            DoStepKind::ValueBind { pattern, .. } if pattern.captures() == ["y"]
        ));
    }

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
            "do\n[first,,second] <- .read\n.r first",
            "expected an item between separators",
        ),
        (
            "do\n.read -> first second\n.r ()",
            "a predicate pattern must be parenthesized",
        ),
        (
            "do\n.r 1 -> left as\n.r left",
            "expected a pattern, found the end",
        ),
        (
            "do\nas right <- .r 1\n.r right",
            "expected a pattern, found `as`",
        ),
        (
            "do\n-> result\n.r result",
            "requires an operation before `->`",
        ),
        ("do\n<- .r 1\n.r ()", "requires a pattern before `<-`"),
        ("do\n.r 1 ->\n.r ()", "requires a pattern after `->`"),
        ("do\n= 1\n.r ()", "requires a pattern before `=`"),
        (
            "do\n.r 1 -> binary\n.r ()",
            "`binary` is a reserved keyword",
        ),
        (
            "do\n[head] ++ first ++ second <- .r []\n.r head",
            "only one variable-length segment",
        ),
        (
            "do\n.r 1 -> (increment ->)\n.r ()",
            "view pattern requires a view before `->` and a pattern after it",
        ),
        (
            "do\n.r 1 -> (_ when)\n.r ()",
            "local pattern guard requires a guard after `when`",
        ),
        (
            "do\n.r 1 -> (_ when .r () and and .r ())\n.r ()",
            "local pattern guard contains an empty clause",
        ),
        (
            "do\n'.foo.() <- .r []\n.r ()",
            "path splice requires an expression",
        ),
        (
            "do\n.r {} -> []:value\n.r value",
            "dictionary paths cannot be empty",
        ),
        (
            "do\n.r {} -> ():value\n.r value",
            "path splice requires an expression",
        ),
        (
            "do\n.r {} -> {[first,,second]:value}\n.r value",
            "expected an item between separators",
        ),
        (
            "do\n.r {} -> {foo:value, rest, bar:other}\n.r value",
            "remainder must be the final pattern item",
        ),
        (
            "do\n.r {} -> {foo:value, foo:other}\n.r value",
            "repeats the same path expression",
        ),
        (
            "do\n.r {} -> {[key]:value, [key]:other}\n.r value",
            "repeats the same path expression",
        ),
        (
            "do\n.r {} -> {?:value}\n.r value",
            "requires a path before `?:`",
        ),
        (
            "do\n.r {} -> {foo ?:value}\n.r value",
            "marker `?` must be joint with its path",
        ),
        (
            "do\n.r {} -> {foo? :value}\n.r value",
            "must be written as joint `?:`",
        ),
        (
            "do\n.r {} -> {foo?:}\n.r ()",
            "requires a payload after `?:`",
        ),
        ("do\n.r {} -> :42\n.r ()", "expected a path after `:`"),
        (
            "do\n.r {} -> :value\n.r ()",
            "dictionary shorthand is valid only inside braces",
        ),
        (
            "do\ndeclare\n.r ()",
            "requires one or more comma-separated local names",
        ),
        (
            "do\ndeclare _\n.r ()",
            "cannot use the inaccessible `_` name",
        ),
        ("do\ndeclare value", "cannot end with a `declare` statement"),
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
