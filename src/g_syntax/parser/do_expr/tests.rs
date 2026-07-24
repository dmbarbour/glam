use crate::g_syntax::{DoExpr, DoStepKind, SyntaxExpr, SyntaxKeyExpr, SyntaxPatternKind};

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
        ".r tag:1 -> :tag\n",
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
                    .contains("expected a capture, literal, list pattern")
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
        DoStepKind::ValueBind { pattern, .. } if pattern.captures() == ["y"]
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
            "do\n[first,,second] <- .read\n.r first",
            "empty item between commas",
        ),
        (
            "do\n.read -> first second\n.r ()",
            "expected a capture, literal, list pattern",
        ),
        (
            "do\n.r 1 -> left as\n.r left",
            "pattern `as` requires a pattern on both sides",
        ),
        (
            "do\nas right <- .r 1\n.r right",
            "pattern `as` requires a pattern on both sides",
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
            "do\n[head] ++ 42 ++ [last] <- .r []\n.r head",
            "variable-length list segment must be an irrefutable pattern",
        ),
        (
            "do\n1 as value <- .r 1\n.r value",
            "requires irrefutable patterns on both sides",
        ),
        (
            "do\n'.foo.() <- .r []\n.r ()",
            "path splice requires an expression",
        ),
        (
            "do\n.r {} -> []:value\n.r value",
            "requires at least one path component",
        ),
        (
            "do\n.r {} -> ():value\n.r value",
            "path splice requires an expression",
        ),
        (
            "do\n.r {} -> {[first,,second]:value}\n.r value",
            "contains an empty key between commas",
        ),
        (
            "do\n.r {} -> {foo:value, rest, bar:other}\n.r value",
            "remainder must be the final pattern item",
        ),
        (
            "do\n.r {} -> {foo:value, []}\n.r value",
            "remainder must be an irrefutable pattern",
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
        (
            "do\n.r {} -> :42\n.r ()",
            "`:name` pattern shorthand requires one capture name",
        ),
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
