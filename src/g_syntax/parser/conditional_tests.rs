//! Conditionals and matches, read through the structural entry the
//! declaration parser uses.

use crate::g_syntax::{
    ConditionalMode, ConditionalResultMode, IfExpr, MatchCommitment, MatchExpr, MatchOutcome,
    SyntaxExpr, SyntaxGuardClause, SyntaxPatternKind,
};

use super::expression_context::ExpressionContext;

fn parse(source: &str) -> SyntaxExpr {
    super::input::parse_expression_fragment(source.as_bytes(), |view| {
        super::structural::parse_expression_in_context(view, ExpressionContext::for_fragment(view))
    })
    .unwrap_or_else(|diagnostics| panic!("`{source}` reported {diagnostics:#?}"))
}

/// Asserts that `source` fails with an error containing `expected`.
#[track_caller]
fn assert_rejected(source: &str, expected: &str) {
    let diagnostics = super::input::parse_expression_fragment(source.as_bytes(), |view| {
        super::structural::parse_expression_in_context(view, ExpressionContext::for_fragment(view))
    })
    .expect_err("the source is invalid");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(expected)),
        "`{source}` reported {diagnostics:#?} instead of `{expected}`"
    );
}

/// Asserts that `source` fails because a tail form in it stands outside
/// tail position (Syntax.md "Tail Position").
#[track_caller]
fn assert_outside_tail_position(source: &str) {
    assert_rejected(source, "not in tail position");
}

#[test]
fn prefix_if_owns_guards_and_both_results() {
    let SyntaxExpr::If(if_expr) = parse("if value = 42 and _ then value else 0") else {
        panic!("prefix syntax should produce an if expression");
    };
    assert_eq!(if_expr.mode, ConditionalMode::Pure);
    assert_eq!(if_expr.then_mode, ConditionalResultMode::Ordinary);
    assert_eq!(if_expr.guards.len(), 2);
    assert!(matches!(
        &if_expr.guards[0],
        SyntaxGuardClause::ValueBind { pattern, .. }
            if matches!(&pattern.kind, SyntaxPatternKind::Capture(name) if name == "value")
    ));
    assert!(matches!(if_expr.guards[1], SyntaxGuardClause::Pass));
    assert!(matches!(
        if_expr.then_result.as_ref(),
        SyntaxExpr::Name(name) if name == "value"
    ));
    assert!(matches!(
        if_expr.else_result.as_ref(),
        SyntaxExpr::Number(_)
    ));
}

#[test]
fn tentative_result_markers_are_owned_by_their_conditional_branches() {
    let SyntaxExpr::If(if_expr) = parse("if _ then? .r 1 else 2") else {
        panic!("tentative prefix syntax should produce an if expression");
    };
    assert_eq!(if_expr.then_mode, ConditionalResultMode::Tentative);
    assert!(matches!(
        if_expr.then_result.as_ref(),
        SyntaxExpr::Apply(_, _)
    ));

    let SyntaxExpr::Match(subject) = parse("match value as { 1 =>? .fail; _ => 2; }") else {
        panic!("tentative arrow syntax should produce a match expression");
    };
    assert!(matches!(
        subject.arms[0].outcome,
        MatchOutcome::Result {
            mode: ConditionalResultMode::Tentative,
            ..
        }
    ));
    assert!(matches!(
        subject.arms[1].outcome,
        MatchOutcome::Result {
            mode: ConditionalResultMode::Ordinary,
            ..
        }
    ));

    let SyntaxExpr::MatchWhen(nested) = parse("match when { _ when { _ =>? .fail; _ => 3; }; }")
    else {
        panic!("nested tentative syntax should preserve its choice tree");
    };
    let MatchOutcome::Nested(children) = &nested.arms[0].outcome else {
        panic!("outer arm should own child alternatives");
    };
    assert!(matches!(
        children[0].outcome,
        MatchOutcome::Result {
            mode: ConditionalResultMode::Tentative,
            ..
        }
    ));
}

/// Every prefix `if` has an `else`, so an inner one takes the first, as
/// brackets pair; `else` separates, so the inner `if` ends there.
#[test]
fn nested_if_keywords_do_not_escape_to_the_outer_conditional() {
    for source in [
        "if _ then if _ then 1 else 2 else 3",
        "if _ then if _ then 1 else 2\nelse 3",
        "if _ then (if _ then 1 else 2) else 3",
    ] {
        let SyntaxExpr::If(outer) = parse(source) else {
            panic!("`{source}` should produce an outer if expression");
        };
        assert!(
            matches!(outer.then_result.as_ref(), SyntaxExpr::If(_)),
            "{source}"
        );
        assert!(
            matches!(outer.else_result.as_ref(), SyntaxExpr::Number(_)),
            "{source}"
        );
    }
}

#[test]
fn prefix_if_supports_hanging_and_next_line_results() {
    let hanging = parse("if _ then first\n          else second");
    assert!(matches!(hanging, SyntaxExpr::If(_)));

    let next_line = parse("if _ then\n  first\n  else\n    second");
    assert!(matches!(next_line, SyntaxExpr::If(_)));
}

#[test]
fn postfix_if_reorders_its_source_result_behind_the_guards() {
    let SyntaxExpr::If(if_expr) = parse("value if value = 42 else 0") else {
        panic!("postfix syntax should produce the shared if expression");
    };
    assert_eq!(if_expr.mode, ConditionalMode::Pure);
    assert_eq!(if_expr.then_mode, ConditionalResultMode::Ordinary);
    assert!(matches!(
        if_expr.guards.as_slice(),
        [SyntaxGuardClause::ValueBind { pattern, .. }]
            if matches!(&pattern.kind, SyntaxPatternKind::Capture(name) if name == "value")
    ));
    assert!(matches!(
        if_expr.then_result.as_ref(),
        SyntaxExpr::Name(name) if name == "value"
    ));
    assert!(matches!(
        if_expr.else_result.as_ref(),
        SyntaxExpr::Number(_)
    ));
}

#[test]
fn postfix_if_supports_layout_results_and_structural_success_values() {
    for source in [
        "value if value = 42\n      else 0",
        "value if\n  value = 42\nelse\n  0",
        "(value if value = 42 else 0)",
    ] {
        assert!(
            matches!(parse(source), SyntaxExpr::If(_)),
            "`{source}` should produce a postfix conditional"
        );
    }

    let SyntaxExpr::If(structural) = parse("do\n  .r 1\nif _ else .r 0") else {
        panic!("a completed structural result should resume into postfix `if`");
    };
    assert!(matches!(structural.then_result.as_ref(), SyntaxExpr::Do(_)));
}

#[test]
fn postfix_fallback_may_be_a_right_nested_prefix_if() {
    let SyntaxExpr::If(outer) = parse("1 if _ else if _ then 2 else 3") else {
        panic!("source should produce an outer postfix conditional");
    };
    assert!(matches!(outer.then_result.as_ref(), SyntaxExpr::Number(_)));
    assert!(matches!(outer.else_result.as_ref(), SyntaxExpr::If(_)));
}

/// After an operand an `if` is postfix, so a prefix conditional there
/// is parenthesized, `try` alike; elsewhere it stands in tail position.
#[test]
fn prefix_conditionals_are_parenthesized_after_an_operand() {
    for source in [
        "f if _ then 1 else 2",
        "f a try _ then 1 else 2",
        "f a\n  if _ then 1 else 2",
    ] {
        assert_rejected(source, "a prefix conditional cannot follow an operand");
    }
    let SyntaxExpr::Apply(_, argument) = parse("f (if _ then 1 else 2)") else {
        panic!("a parenthesized conditional is an argument");
    };
    assert!(matches!(argument.as_ref(), SyntaxExpr::If(_)));
    assert!(matches!(parse("f a if _ else 2"), SyntaxExpr::If(_)));
    assert!(matches!(
        parse("1 + if _ then 1 else 2"),
        SyntaxExpr::Add(_, right) if matches!(right.as_ref(), SyntaxExpr::If(_))
    ));
}

#[test]
fn malformed_postfix_if_reports_its_missing_guard_or_fallback() {
    for (source, expected) in [
        (
            "value if guard",
            "postfix `if` requires `else` and a fallback result",
        ),
        (
            "value if else fallback",
            "postfix `if` requires guards before `else`",
        ),
        (
            "value if guard else",
            "postfix `if` requires a fallback after `else`",
        ),
    ] {
        let diagnostics = super::input::parse_expression_fragment(source.as_bytes(), |view| {
            super::structural::parse_expression_in_context(
                view,
                ExpressionContext::for_fragment(view),
            )
        })
        .expect_err("malformed postfix conditional should fail");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "`{source}` reported {diagnostics:#?} instead of `{expected}`"
        );
    }
}

#[test]
fn malformed_prefix_if_reports_its_missing_boundary_or_result() {
    for (source, expected) in [
        ("if _ else 0", "requires `then` before `else`"),
        ("if _ then 1", "requires `else`"),
        ("if _ then else 0", "requires a result after `then`"),
        ("if _ then 1 else", "requires a fallback after `else`"),
    ] {
        let diagnostics = super::input::parse_expression_fragment(source.as_bytes(), |view| {
            super::structural::parse_expression_in_context(
                view,
                ExpressionContext::for_fragment(view),
            )
        })
        .expect_err("malformed conditional should fail");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "`{source}` reported {diagnostics:#?} instead of `{expected}`"
        );
    }
}

#[test]
fn malformed_try_variants_report_their_host_construct() {
    for (source, expected) in [
        (
            "try _ else 0",
            "try expression requires `then` before `else`",
        ),
        (
            "try _ then 1",
            "try expression requires `else` and a fallback result",
        ),
        ("try_match value", "try_match expression requires `as`"),
        (
            "try_match value as",
            "layout try_match expression requires at least one arm",
        ),
    ] {
        let diagnostics = super::input::parse_expression_fragment(source.as_bytes(), |view| {
            super::structural::parse_expression_in_context(
                view,
                ExpressionContext::for_fragment(view),
            )
        })
        .expect_err("malformed host conditional should fail");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "`{source}` reported {diagnostics:#?} instead of `{expected}`"
        );
    }
}

#[test]
fn flat_match_parses_layout_and_braced_arms() {
    let SyntaxExpr::Match(layout) = parse(
        "match subject as\n  1 => \"one\"\n  value when value == 2 => \"two\"\n  _ => \"other\"",
    ) else {
        panic!("layout syntax should produce a match expression");
    };
    assert!(matches!(
        layout.subject.as_ref(),
        SyntaxExpr::Name(name) if name == "subject"
    ));
    assert_eq!(layout.arms.len(), 3);
    assert!(layout.arms[0].guards.is_empty());
    assert_eq!(layout.arms[1].guards.len(), 1);
    assert!(matches!(
        &layout.arms[1].pattern.kind,
        SyntaxPatternKind::Capture(name) if name == "value"
    ));

    let SyntaxExpr::Match(dict_pattern) =
        parse("match subject as\n  {} => \"empty\"\n  _ => \"other\"")
    else {
        panic!("a dictionary pattern must not become the braced match body");
    };
    assert_eq!(dict_pattern.arms.len(), 2);
    assert!(matches!(
        dict_pattern.arms[0].pattern.kind,
        SyntaxPatternKind::Dict { .. }
    ));

    let SyntaxExpr::Match(dict_as_pattern) =
        parse("match subject as\n  {x:item} as whole => whole\n  _ => {}")
    else {
        panic!("a complete dictionary `as` pattern should establish layout");
    };
    assert_eq!(dict_as_pattern.arms.len(), 2);
    assert!(matches!(
        dict_as_pattern.arms[0].pattern.kind,
        SyntaxPatternKind::As(_, _)
    ));

    let SyntaxExpr::Match(braced) = parse("match subject as {; [head] ++ tail => head; _ => 0;}")
    else {
        panic!("braced syntax should produce a match expression");
    };
    assert_eq!(braced.arms.len(), 2);

    let SyntaxExpr::Match(empty) = parse("match subject as {}") else {
        panic!("an explicit empty match should remain a match expression");
    };
    assert!(empty.arms.is_empty());
}

/// A subject's `as` is its match's, and an update names its base in its
/// body, so an update in a subject needs no parentheses.
#[test]
fn a_subject_update_needs_no_parentheses() {
    for source in [
        "match base with { x := 1 } as { {x:item} => item; _ => 0; }",
        "match (base with { x := 1 }) as { {x:item} => item; _ => 0; }",
        "match base with { alias b; x := 1 } as { {x:item} => item; _ => 0; }",
        "match (base with { alias b; x := 1 }) as { {x:item} => item; _ => 0; }",
    ] {
        let SyntaxExpr::Match(found) = parse(source) else {
            panic!("`{source}` should parse as a match");
        };
        assert!(
            matches!(found.subject.as_ref(), SyntaxExpr::With { .. }),
            "{source}"
        );
    }
    assert_rejected(
        "match base as b with { x := 1 } as { {x:item} => item; _ => 0; }",
        "an alias is its body's first member",
    );
}

#[test]
fn try_variants_reuse_conditional_shapes_in_host_mode() {
    let SyntaxExpr::If(try_expr) = parse("try value = 42 and _ then value else 0") else {
        panic!("try syntax should produce the shared if expression");
    };
    assert_eq!(try_expr.mode, ConditionalMode::Host);
    assert_eq!(try_expr.guards.len(), 2);

    let SyntaxExpr::Match(subject) = parse("try_match value as { 42 => \"yes\"; _ => \"no\"; }")
    else {
        panic!("try_match subject syntax should produce the shared match expression");
    };
    assert_eq!(subject.mode, ConditionalMode::Host);
    assert_eq!(subject.commitment, MatchCommitment::Cut);
    assert_eq!(subject.arms.len(), 2);

    let SyntaxExpr::MatchWhen(guard_only) =
        parse("try_match when { _ when { _ => \"nested\"; }; }")
    else {
        panic!("try_match when should produce the shared guard-only match expression");
    };
    assert_eq!(guard_only.mode, ConditionalMode::Host);
    assert_eq!(guard_only.commitment, MatchCommitment::Cut);
    assert!(matches!(
        guard_only.arms[0].outcome,
        MatchOutcome::Nested(_)
    ));

    let SyntaxExpr::MatchWhen(empty) = parse("try_match when {}") else {
        panic!("explicit empty try_match when should remain a host search");
    };
    assert_eq!(empty.mode, ConditionalMode::Host);
    assert_eq!(empty.commitment, MatchCommitment::Cut);
    assert!(empty.arms.is_empty());
}

#[test]
fn starred_match_forms_are_joint_open_choices() {
    let SyntaxExpr::Match(subject) = parse("match* value as { 42 => \"yes\"; _ => \"no\"; }")
    else {
        panic!("match* subject syntax should produce a match expression");
    };
    assert_eq!(subject.mode, ConditionalMode::Pure);
    assert_eq!(subject.commitment, MatchCommitment::Open);
    assert_eq!(subject.arms.len(), 2);

    let SyntaxExpr::MatchWhen(guard_only) =
        parse("try_match* when { _ when { _ => \"nested\"; }; }")
    else {
        panic!("try_match* when should produce a match-when expression");
    };
    assert_eq!(guard_only.mode, ConditionalMode::Host);
    assert_eq!(guard_only.commitment, MatchCommitment::Open);
    assert!(matches!(
        guard_only.arms[0].outcome,
        MatchOutcome::Nested(_)
    ));

    assert!(matches!(
        parse("(try_match* value as { _ => value; })"),
        SyntaxExpr::Match(MatchExpr {
            mode: ConditionalMode::Host,
            commitment: MatchCommitment::Open,
            ..
        })
    ));

    let diagnostics =
        super::input::parse_expression_fragment(b"match * value as { _ => 1; }", |view| {
            super::structural::parse_expression_in_context(
                view,
                ExpressionContext::for_fragment(view),
            )
        })
        .expect_err("whitespace must not separate a starred match head");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("open form is spelled `match*` without intervening whitespace")
    }));
}

#[test]
fn nested_if_and_try_boundaries_remain_owned_by_their_heads() {
    let SyntaxExpr::If(outer_try) = parse("try _ then if _ then 1 else 2 else 3") else {
        panic!("source should produce an outer try expression");
    };
    assert_eq!(outer_try.mode, ConditionalMode::Host);
    assert!(matches!(
        outer_try.then_result.as_ref(),
        SyntaxExpr::If(IfExpr {
            mode: ConditionalMode::Pure,
            ..
        })
    ));

    let SyntaxExpr::If(outer_if) = parse("if _ then try _ then 1 else 2 else 3") else {
        panic!("source should produce an outer if expression");
    };
    assert_eq!(outer_if.mode, ConditionalMode::Pure);
    assert!(matches!(
        outer_if.then_result.as_ref(),
        SyntaxExpr::If(IfExpr {
            mode: ConditionalMode::Host,
            ..
        })
    ));
}

#[test]
fn match_when_and_hierarchical_outcomes_parse_as_choice_trees() {
    let SyntaxExpr::MatchWhen(guard_only) = parse(
        "match when\n  value = 1 when\n    value == 2 => \"bad\"\n    value == 1 => value\n  _ => 0",
    ) else {
        panic!("guard-only syntax should produce a match-when expression");
    };
    assert_eq!(guard_only.arms.len(), 2);
    let MatchOutcome::Nested(children) = &guard_only.arms[0].outcome else {
        panic!("the first guard-only arm should own nested choices");
    };
    assert_eq!(children.len(), 2);

    let SyntaxExpr::Match(subject) = parse(
        "match subject as\n  value when value == 1 when\n    next = value + 1 => next\n    _ => value\n  _ => 0",
    ) else {
        panic!("source should produce a subject match");
    };
    let MatchOutcome::Nested(children) = &subject.arms[0].outcome else {
        panic!("the first subject arm should own nested choices");
    };
    assert_eq!(subject.arms[0].guards.len(), 1);
    assert_eq!(children.len(), 2);

    let SyntaxExpr::MatchWhen(braced) =
        parse("match when { _ when { 1 == 2 => \"bad\"; _ => \"ok\"; }; }")
    else {
        panic!("braced nested choices should produce a match-when expression");
    };
    assert!(matches!(braced.arms[0].outcome, MatchOutcome::Nested(_)));

    let SyntaxExpr::MatchWhen(empty) = parse("match when {}") else {
        panic!("an explicit empty guard-only search should remain a match-when expression");
    };
    assert!(empty.arms.is_empty());
}

#[test]
fn an_open_lambda_body_takes_a_postfix_if() {
    fn innermost_body(mut expr: &SyntaxExpr) -> &SyntaxExpr {
        loop {
            expr = match expr {
                SyntaxExpr::Lambda(_, body) => return body,
                SyntaxExpr::Apply(_, argument) => argument,
                SyntaxExpr::OperatorApply { right, .. } => right,
                SyntaxExpr::Add(_, right) => right,
                SyntaxExpr::List(items) => items.last().expect("a lambda item"),
                other => panic!("no lambda in {other:?}"),
            };
        }
    }
    for source in [
        "\\a -> b if c else d",
        "\\a -> b\n  if c else d",
        "x + \\a -> b if c else d",
        "f \\a -> b if c else d",
        "[\\a -> b if c else d]",
    ] {
        assert!(
            matches!(innermost_body(&parse(source)), SyntaxExpr::If(_)),
            "`{source}` should put the postfix `if` in the lambda body"
        );
    }
    assert!(matches!(
        parse("(\\a -> b) if c else d"),
        SyntaxExpr::If(if_expr) if matches!(if_expr.then_result.as_ref(), SyntaxExpr::Lambda(..))
    ));
}

#[test]
fn a_match_owns_its_with_inside_a_lambda_or_a_tuple() {
    for source in [
        "\\w -> match w as { z => w }",
        "(\\w -> match w as { z => w })",
        "\\w -> match w as\n  z => d with { x := 1 }",
    ] {
        let SyntaxExpr::Lambda(_, body) = parse(source) else {
            panic!("`{source}` should produce a lambda");
        };
        assert!(
            matches!(body.as_ref(), SyntaxExpr::Match(_)),
            "`{source}` should produce a match body, not an update"
        );
    }
    for source in [
        "(1, match y as { z => z })",
        "(match y as { z => z }, 1)",
        "(match y as\n  z => z, 1)",
    ] {
        let SyntaxExpr::Tuple(items) = parse(source) else {
            panic!("`{source}` should produce a tuple");
        };
        assert_eq!(items.len(), 2, "`{source}`");
        assert!(
            items
                .iter()
                .any(|item| matches!(item, SyntaxExpr::Match(_))),
            "`{source}` should hold a match member"
        );
    }
}

#[test]
fn complete_match_patterns_accept_unparenthesized_views_and_predicates() {
    let SyntaxExpr::Match(forward) = parse("match subject as { inspect -> value => value; }")
    else {
        panic!("source should produce a match expression");
    };
    assert!(matches!(
        forward.arms[0].pattern.kind,
        SyntaxPatternKind::View { .. }
    ));

    let SyntaxExpr::Match(backward) = parse("match subject as { value <- inspect => value; }")
    else {
        panic!("source should produce a match expression");
    };
    assert!(matches!(
        backward.arms[0].pattern.kind,
        SyntaxPatternKind::View { .. }
    ));

    let SyntaxExpr::Match(predicate) = parse("match subject as { equals 42 value => value; }")
    else {
        panic!("source should produce a match expression");
    };
    assert!(matches!(
        predicate.arms[0].pattern.kind,
        SyntaxPatternKind::Predicate { .. }
    ));
}

#[test]
fn malformed_match_reports_its_missing_boundary_or_arm_part() {
    for (source, expected) in [
        ("match value", "requires `as`"),
        (
            "match value as",
            "layout match expression requires at least one arm",
        ),
        ("match value as { 1 }", "requires `=>`"),
        ("match value as { => 1 }", "requires a pattern before `=>`"),
        ("match value as { 1 => }", "requires a result after `=>`"),
        (
            "match value as { value when => value }",
            "requires guards after `when`",
        ),
        (
            "match value as { _ => 1 => 2 }",
            "exactly one top-level `=>`",
        ),
    ] {
        let diagnostics = super::input::parse_expression_fragment(source.as_bytes(), |view| {
            super::structural::parse_expression_in_context(
                view,
                ExpressionContext::for_fragment(view),
            )
        })
        .expect_err("malformed match should fail");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "`{source}` reported {diagnostics:#?} instead of `{expected}`"
        );
    }
}

#[test]
fn an_argument_cannot_follow_a_match() {
    for source in [
        "match a as {z => 1} b",
        "match a as\n    z => 1\n  b",
        "f match a as {z => 1} b",
    ] {
        assert_outside_tail_position(source);
    }
    let SyntaxExpr::Apply(function, argument) = parse("(match a as {z => 1}) b") else {
        panic!("a parenthesized match applies");
    };
    assert!(matches!(*function, SyntaxExpr::Match(_)));
    assert_eq!(*argument, SyntaxExpr::Name("b".to_owned()));
}

/// The first `as` after a match's head ends its subject, so braces after
/// it hold `;`-separated arms while an as-pattern's braces hold a
/// `,`-separated dictionary, and an update that names its base is
/// parenthesized in a subject.
#[test]
fn a_subjects_as_begins_the_arms() {
    let SyntaxExpr::Match(braced) = parse("match v as { p as {a:y, b:z} => y; _ => 0 }") else {
        panic!("braced arms should parse");
    };
    assert_eq!(braced.arms.len(), 2);
    for source in [
        "match v as p as {a:y, b:z} => y",
        "match v as\n  p as {a:y, b:z} => y\n  _ => 0",
        "match d with { a := 1 } as { {a:y} => y; _ => 0 }",
        "match (d with { alias e; a := 1 }) as { _ => 0 }",
        "match if a then b else c as { _ => 0 }",
    ] {
        assert!(matches!(parse(source), SyntaxExpr::Match(_)), "{source}");
    }
    assert_rejected(
        "match d as e with { a := 1 }",
        "an alias is its body's first member",
    );
    assert_rejected(
        "match d with\n  1 => 2",
        "a `with` in its subject begins an update",
    );
}

/// A `{` on the line of a match's `as` delimits its arms, so a first
/// arm with a dictionary pattern begins on the next line.
#[test]
fn a_brace_after_as_delimits_the_arms() {
    let SyntaxExpr::Match(braced) = parse("match v as {1 => 2; _ => 3}") else {
        panic!("braced arms should parse");
    };
    assert_eq!(braced.arms.len(), 2);
    let SyntaxExpr::Match(layout) = parse("match v as\n  {a:z} => z\n  _ => 0") else {
        panic!("a dictionary pattern may begin a layout arm");
    };
    assert!(matches!(
        layout.arms[0].pattern.kind,
        SyntaxPatternKind::Dict { .. }
    ));
    assert_rejected(
        "match v as {a:z} => z",
        "a `{` on the line of `as` delimits the arms",
    );
    assert_rejected(
        "match v as\n  {1 => 2; _ => 3}",
        "a `{` delimits arms only on the line of its `as`",
    );
}

/// A `when` ending its line, or before a `{`, heads nested arms; any
/// other begins guards, which may continue onto later lines before the
/// arm's arrow.
#[test]
fn a_when_ending_its_line_heads_nested_arms() {
    let SyntaxExpr::Match(nested) = parse("match v as\n  p when\n    c => 1\n  _ => 2") else {
        panic!("nested arms should parse");
    };
    assert!(matches!(nested.arms[0].outcome, MatchOutcome::Nested(_)));
    for source in [
        "match v as\n  p when c\n      => 1\n  _ => 2",
        "match v as\n  p when c\n      and d => 1\n  _ => 2",
        "match v as\n  p when ({}) <- e => 1\n  _ => 2",
    ] {
        let SyntaxExpr::Match(guarded) = parse(source) else {
            panic!("`{source}` should parse");
        };
        assert!(
            matches!(guarded.arms[0].outcome, MatchOutcome::Result { .. }),
            "{source}"
        );
        assert!(!guarded.arms[0].guards.is_empty(), "{source}");
    }
    let SyntaxExpr::Match(braced) = parse("match v as\n  p when {c => 1}\n  _ => 2") else {
        panic!("braced nested arms should parse");
    };
    assert!(matches!(braced.arms[0].outcome, MatchOutcome::Nested(_)));
    assert_rejected(
        "match v as\n  p when {} <- e => 1\n  _ => 2",
        "parenthesize a dictionary pattern that begins a guard",
    );
}

/// A later arrow on an arm's line belongs to a nested match's hanging
/// arms; two arrows of one arm are an error.
#[test]
fn a_later_arrow_on_an_arms_line_belongs_to_a_nested_match() {
    let SyntaxExpr::Match(outer) = parse("match v as\n  p => match b as q => 1\n  _ => 2") else {
        panic!("the nested match should parse");
    };
    assert_eq!(outer.arms.len(), 2);
    assert!(matches!(
        &outer.arms[0].outcome,
        MatchOutcome::Result { expression: SyntaxExpr::Match(inner), .. } if inner.arms.len() == 1
    ));
    for source in [
        "match v as { _ => 1 => 2 }",
        "match v as\n  p => 1 => 2\n  _ => 3",
    ] {
        assert_rejected(source, "permits exactly one top-level `=>`");
    }
}

#[test]
fn a_match_in_a_subject_takes_the_first_as_after_its_head() {
    for source in [
        "match match a as {z => z} as {_ => 1}",
        "match match a as\n    z => z\n  as {_ => 1}",
    ] {
        let SyntaxExpr::Match(outer) = parse(source) else {
            panic!("`{source}` should parse as a match");
        };
        assert!(
            matches!(outer.subject.as_ref(), SyntaxExpr::Match(inner) if inner.arms.len() == 1),
            "{source}"
        );
        assert_eq!(outer.arms.len(), 1, "{source}");
    }
}

#[test]
fn malformed_hierarchical_match_reports_missing_prefixes_and_children() {
    for (source, expected) in [
        (
            "match x as\n  _ when\n      c => 1\n    d",
            "continues after its nested `when` block",
        ),
        (
            "match x as\n  _ when {c => 1} d",
            "continues after its nested `when` block",
        ),
        (
            "match when",
            "layout `match when` requires at least one arm",
        ),
        (
            "match when\n  _ when",
            "nested `when` requires at least one child arm",
        ),
        (
            "match value as\n  when\n    _ => 1",
            "requires a pattern before nested `when`",
        ),
        ("match when\n  => 1", "requires guards before `=>`"),
    ] {
        let diagnostics = super::input::parse_expression_fragment(source.as_bytes(), |view| {
            super::structural::parse_expression_in_context(
                view,
                ExpressionContext::for_fragment(view),
            )
        })
        .expect_err("malformed hierarchical match should fail");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "`{source}` reported {diagnostics:#?} instead of `{expected}`"
        );
    }
}
