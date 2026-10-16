use super::super::input::parse_expression_fragment;
use super::{parse_complete_pattern, parse_pattern};
use crate::g_syntax::{
    Diagnostic, SyntaxExpr, SyntaxKeyExpr, SyntaxPattern, SyntaxPatternKind, SyntaxPatternLiteral,
};

type Parsed = Result<SyntaxPattern, Vec<Diagnostic>>;

fn nested(text: &str) -> Parsed {
    parse_expression_fragment(text.as_bytes(), parse_pattern)
}

fn complete(text: &str) -> Parsed {
    parse_expression_fragment(text.as_bytes(), parse_complete_pattern)
}

/// A compact reading of a pattern: operators as S-expressions, data as
/// written.
fn show(pattern: &SyntaxPattern) -> String {
    let list = |items: &[SyntaxPattern]| {
        format!(
            "[{}]",
            items.iter().map(show).collect::<Vec<_>>().join(", ")
        )
    };
    match &pattern.kind {
        SyntaxPatternKind::Capture(name) => name.clone(),
        SyntaxPatternKind::Wildcard => "_".to_owned(),
        SyntaxPatternKind::Literal(SyntaxPatternLiteral::Unit) => "()".to_owned(),
        SyntaxPatternKind::Literal(SyntaxPatternLiteral::Number(_)) => "#".to_owned(),
        SyntaxPatternKind::Literal(SyntaxPatternLiteral::Atom(name)) => format!("'{name}"),
        SyntaxPatternKind::Literal(SyntaxPatternLiteral::Text(text)) => format!("{text:?}"),
        SyntaxPatternKind::List {
            prefix,
            middle: None,
            suffix,
        } => {
            assert!(suffix.is_empty(), "a fixed list has no suffix");
            list(prefix)
        }
        SyntaxPatternKind::List {
            prefix,
            middle: Some(middle),
            suffix,
        } => format!("{} ++ {} ++ {}", list(prefix), show(middle), list(suffix)),
        SyntaxPatternKind::Dict { entries, remainder } => {
            let mut members = entries
                .iter()
                .map(|entry| {
                    let marker = if entry.optional { "?:" } else { ":" };
                    format!("{}{marker} {}", keys(&entry.path), show(&entry.pattern))
                })
                .collect::<Vec<_>>();
            members.extend(remainder.iter().map(|remainder| show(remainder)));
            format!("{{{}}}", members.join(", "))
        }
        SyntaxPatternKind::QuotedPath(path) => format!("'.{}", keys(path)),
        SyntaxPatternKind::Group(pattern) => format!("(group {})", show(pattern)),
        SyntaxPatternKind::As(left, right) => format!("(as {} {})", show(left), show(right)),
        SyntaxPatternKind::View { view, pattern } => {
            format!("(view {} {})", expr(view), show(pattern))
        }
        SyntaxPatternKind::Predicate { predicate, pattern } => {
            format!("(pred {} {})", expr(predicate), show(pattern))
        }
        SyntaxPatternKind::Guarded { pattern, guards } => {
            format!("(when {} {})", show(pattern), guards.len())
        }
    }
}

fn keys(path: &[SyntaxKeyExpr]) -> String {
    path.iter()
        .map(|key| match key {
            SyntaxKeyExpr::Atom(name) => name.clone(),
            SyntaxKeyExpr::Index(index) => format!("[{}]", expr(index)),
            SyntaxKeyExpr::PathIndex(index) => format!("({})", expr(index)),
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn expr(expression: &SyntaxExpr) -> String {
    match expression {
        SyntaxExpr::Name(name) => name.clone(),
        SyntaxExpr::Text(text) => format!("{text:?}"),
        SyntaxExpr::Effect(path) => format!(".{}", path.join(".")),
        SyntaxExpr::Apply(function, argument) => {
            format!("({} {})", expr(function), expr(argument))
        }
        SyntaxExpr::PathDict(path, payload) => format!("{}:{}", keys(path), expr(payload)),
        SyntaxExpr::Lambda(params, body) => format!("(\\{} -> {})", params.join(" "), expr(body)),
        _ => "…".to_owned(),
    }
}

/// The pattern forms of the syntax reference and their edge cases, valid
/// and invalid alike, with their reading in a nested position. Each reading
/// matched the split-and-retry parser that cover mode replaced, when both
/// existed (2026-10-16).
const NESTED_READINGS: &[(&str, &str)] = &[
    ("x", "x"),
    ("_x", "_x"),
    ("_", "_"),
    ("x as y", "(as x y)"),
    ("x as y as z", "(as (as x y) z)"),
    ("(x)", "(group x)"),
    ("((x))", "(group (group x))"),
    ("()", "()"),
    ("{}", "{}"),
    ("{d}", "{d}"),
    ("{x: p, y: q, rem}", "{x: p, y: q, rem}"),
    ("{x:p,y:q}", "{x: p, y: q}"),
    ("{x: p, {y: q}}", "{x: p, {y: q}}"),
    ("{x?: p, rem}", "{x?: p, rem}"),
    ("{x?: {}}", "{x?: {}}"),
    ("{:x, :y, :z}", "{x: x, y: y, z: z}"),
    ("{:_x}", "{x: _x}"),
    ("{foo.bar.baz: p, _}", "{foo.bar.baz: p, _}"),
    ("{selector: key, [key]: p}", "{selector: key, [key]: p}"),
    (
        "{selector: key, root.[key]: p}",
        "{selector: key, root.[key]: p}",
    ),
    ("{(e): p, _}", "{(e): p, _}"),
    ("{a.(e).[k]: p}", "{a.(e).[k]: p}"),
    ("{, a: p,}", "{a: p}"),
    ("tag:p", "{tag: p}"),
    ("tag:(p)", "{tag: (group p)}"),
    ("a:b:c", "{a: {b: c}}"),
    ("[k]:p", "{[k]: p}"),
    ("[a, b]:p", "{[a].[b]: p}"),
    ("(path):p", "{(path): p}"),
    ("'name", "'name"),
    ("'.a.b", "['a, 'b]"),
    ("'.foo.[e].(path)", "'.foo.[e].(path)"),
    ("'.[1].['x]", "[#, 'x]"),
    ("[]", "[]"),
    ("[a, b, c]", "[a, b, c]"),
    ("[,a]", "[a]"),
    ("[x] ++ xs", "[x] ++ xs ++ []"),
    ("xs ++ [x]", "[] ++ xs ++ [x]"),
    ("[x0] ++ xs ++ [xn]", "[x0] ++ xs ++ [xn]"),
    (
        "[x0] ++ ([y] ++ ys) ++ [xn]",
        "[x0] ++ (group [y] ++ ys ++ []) ++ [xn]",
    ),
    ("\"foo\"", "\"foo\""),
    ("\"foo\" ++ xs", "[#, #, #] ++ xs ++ []"),
    ("(,)", "{tuple: []}"),
    ("(p,)", "{tuple: [p]}"),
    ("(p1, p2, p3)", "{tuple: [p1, p2, p3]}"),
    ("(a as b, c)", "{tuple: [(as a b), c]}"),
    ("42", "#"),
    ("_1.23", "#"),
    ("(view -> p)", "(view view p)"),
    ("(p <- view)", "(view view p)"),
    ("(f x -> [a, b])", "(view (f x) [a, b])"),
    ("(.get -> p)", "(view .get p)"),
    ("(\\x -> x -> p)", "(view (\\x -> x) p)"),
    ("(Nat n)", "(pred Nat n)"),
    ("(Prefix \"foo-\" text)", "(pred (Prefix \"foo-\") text)"),
    ("(Nat (Even n))", "(pred Nat (pred Even n))"),
    ("(p when x > 0)", "(when p 1)"),
    ("(p when q <- .op and r = 1)", "(when p 2)"),
    ("(p when _)", "(when p 1)"),
    ("(a:Nat n)", "(pred a:Nat n)"),
    ("((Nat n) as m)", "(group (as (pred Nat n) m))"),
    ("{a: [x] ++ y}", "{a: [x] ++ y ++ []}"),
    ("[x] ++ xs as all", "(as [x] ++ xs ++ [] all)"),
    ("", "error"),
    ("x y", "error"),
    ("x + y", "error"),
    ("f -> p", "error"),
    ("Nat n", "error"),
    ("if", "error"),
    ("self", "error"),
    ("x.y", "error"),
    (".op", "error"),
    (":x", "error"),
    ("{x: p, x: q}", "error"),
    ("{rest, x: p}", "error"),
    ("{a, b}", "error"),
    ("[x] ++ ys ++ zs", "error"),
    ("x as", "error"),
    ("as x", "error"),
    ("(p when)", "error"),
    ("(f -> g -> p)", "error"),
    ("(\\x -> x)", "error"),
    ("{: x}", "error"),
    ("{:x y}", "error"),
    ("{x ?: p}", "error"),
    ("tag: p", "error"),
];

/// Complete positions: a match arm's head, which views and predicates may
/// fill without parentheses.
const COMPLETE_READINGS: &[(&str, &str)] = &[
    ("x", "x"),
    ("Nat n", "(pred Nat n)"),
    ("Prefix \"foo-\" text", "(pred (Prefix \"foo-\") text)"),
    ("f x -> [a]", "(view (f x) [a])"),
    ("view -> p as q", "(view view (as p q))"),
    ("p <- f x", "(view (f x) p)"),
    ("(x)", "(group x)"),
    ("{x: Nat n}", "error"),
    ("Nat", "Nat"),
];

#[test]
fn cover_mode_reads_every_pattern_form() {
    for (text, expected) in NESTED_READINGS {
        assert_eq!(reading(&nested(text)), *expected, "nested {text:?}");
    }
    for (text, expected) in COMPLETE_READINGS {
        assert_eq!(reading(&complete(text)), *expected, "complete {text:?}");
    }
}

/// Where the split-and-retry parser's trial order decided precedence, cover
/// mode follows the expression grammar: juxtaposition binds tighter than
/// `++`, `as` is looser than both, and completeness passes through `as`, a
/// view's pattern and a guarded pattern. Each case shows the old reading,
/// then the new (`patterns-parse-as-cover-terms` in the decisions log).
const PRECEDENCE_CHANGES: &[(&str, &str, &str)] = &[
    // The predicate passes its input on, so both bind the same.
    (
        "(Nat n as m)",
        "(pred Nat (as n m))",
        "(group (as (pred Nat n) m))",
    ),
    ("(p as Nat n)", "error", "(group (as p (pred Nat n)))"),
    ("(Nat n when n > 0)", "error", "(when (pred Nat n) 1)"),
    ("(f -> Nat n)", "error", "(view f (pred Nat n))"),
    ("(f <| g x)", "(pred … x)", "error"),
    (
        "(Prefix \"x\" rest ++ [y])",
        "(pred (Prefix \"x\") [] ++ rest ++ [y])",
        "error",
    ),
];

fn reading(parsed: &Parsed) -> String {
    parsed.as_ref().map_or_else(|_| "error".to_owned(), show)
}

#[test]
fn cover_mode_resolves_precedence_by_the_expression_grammar() {
    for (text, _, new) in PRECEDENCE_CHANGES {
        assert_eq!(reading(&nested(text)), *new, "{text:?}");
    }
    assert_eq!(
        reading(&complete("Nat n as m")),
        "(as (pred Nat n) m)",
        "a complete position's `as` passes completeness to its operands"
    );
}

#[test]
fn cover_mode_reads_patterns_by_position() {
    let cases: &[(&str, &str, &str)] = &[
        // text, nested, complete
        ("Nat n", "error", "(pred Nat n)"),
        ("f -> p", "error", "(view f p)"),
        ("(f -> p)", "(view f p)", "(view f p)"),
        ("x as y", "(as x y)", "(as x y)"),
        ("[Nat n]", "error", "error"),
        ("[(Nat n)]", "[(pred Nat n)]", "[(pred Nat n)]"),
        ("a:(Nat n)", "{a: (pred Nat n)}", "{a: (pred Nat n)}"),
        (
            "{a: (b as c)}",
            "{a: (group (as b c))}",
            "{a: (group (as b c))}",
        ),
        ("(x as y -> z)", "error", "error"),
        (
            "((p when q) when r)",
            "(when (when p 1) 1)",
            "(when (when p 1) 1)",
        ),
    ];
    for (text, in_nested, in_complete) in cases {
        assert_eq!(reading(&nested(text)), *in_nested, "nested {text:?}");
        assert_eq!(reading(&complete(text)), *in_complete, "complete {text:?}");
    }
}

#[test]
fn cover_mode_reports_what_a_pattern_cannot_hold() {
    let cases: &[(&str, &str)] = &[
        ("(f <| g x)", "only `++` may join patterns"),
        ("[Nat n]", "a predicate pattern must be parenthesized"),
        ("[f -> p]", "a view pattern must be parenthesized"),
        ("x when y", "a local pattern guard must be parenthesized"),
        ("(f -> g -> p)", "a pattern takes one view arrow"),
        ("(x as y -> z)", "`as` joins two patterns"),
        ("(_ x)", "expected a name after `_`"),
        ("then", "reserved keyword"),
        (":x", "dictionary shorthand is valid only inside braces"),
        ("{x: p, x: q}", "repeats the same path"),
        ("[x] ++ ys ++ zs", "only one variable-length segment"),
    ];
    for (text, message) in cases {
        let diagnostics = nested(text).expect_err(text);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(message)),
            "{text:?}: {diagnostics:?}"
        );
    }
}
