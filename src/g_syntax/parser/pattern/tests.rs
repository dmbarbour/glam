use std::path::{Path, PathBuf};

use super::super::input::parse_expression_fragment;
use super::super::parse_source;
use super::oracle::{Report, observe};
use super::{legacy, parse_complete_pattern, parse_pattern};
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

fn legacy_nested(text: &str) -> Parsed {
    parse_expression_fragment(text.as_bytes(), legacy::parse_pattern)
}

fn legacy_complete(text: &str) -> Parsed {
    parse_expression_fragment(text.as_bytes(), legacy::parse_complete_pattern)
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
/// and invalid alike: in a nested position the two parsers must agree on
/// each, accepting the same patterns as the same syntax.
const NESTED_AGREEMENT: &[&str] = &[
    "x",
    "_x",
    "_",
    "x as y",
    "x as y as z",
    "(x)",
    "((x))",
    "()",
    "{}",
    "{d}",
    "{x: p, y: q, rem}",
    "{x:p,y:q}",
    "{x: p, {y: q}}",
    "{x?: p, rem}",
    "{x?: {}}",
    "{:x, :y, :z}",
    "{:_x}",
    "{foo.bar.baz: p, _}",
    "{selector: key, [key]: p}",
    "{selector: key, root.[key]: p}",
    "{(e): p, _}",
    "{a.(e).[k]: p}",
    "{, a: p,}",
    "tag:p",
    "tag:(p)",
    "a:b:c",
    "[k]:p",
    "[a, b]:p",
    "(path):p",
    "'name",
    "'.a.b",
    "'.foo.[e].(path)",
    "'.[1].['x]",
    "[]",
    "[a, b, c]",
    "[,a]",
    "[x] ++ xs",
    "xs ++ [x]",
    "[x0] ++ xs ++ [xn]",
    "[x0] ++ ([y] ++ ys) ++ [xn]",
    "\"foo\"",
    "\"foo\" ++ xs",
    "(,)",
    "(p,)",
    "(p1, p2, p3)",
    "(a as b, c)",
    "42",
    "_1.23",
    "(view -> p)",
    "(p <- view)",
    "(f x -> [a, b])",
    "(.get -> p)",
    "(\\x -> x -> p)",
    "(Nat n)",
    "(Prefix \"foo-\" text)",
    "(Nat (Even n))",
    "(p when x > 0)",
    "(p when q <- .op and r = 1)",
    "(p when _)",
    "(a:Nat n)",
    "((Nat n) as m)",
    "{a: [x] ++ y}",
    "[x] ++ xs as all",
    // Rejected by both.
    "",
    "x y",
    "x + y",
    "f -> p",
    "Nat n",
    "if",
    "self",
    "x.y",
    ".op",
    ":x",
    "{x: p, x: q}",
    "{rest, x: p}",
    "{a, b}",
    "[x] ++ ys ++ zs",
    "x as",
    "as x",
    "(p when)",
    "(f -> g -> p)",
    "(\\x -> x)",
    "{: x}",
    "{:x y}",
    "{x ?: p}",
    "tag: p",
];

/// Complete positions: a match arm's head, which views and predicates may
/// fill without parentheses.
const COMPLETE_AGREEMENT: &[&str] = &[
    "x",
    "Nat n",
    "Prefix \"foo-\" text",
    "f x -> [a]",
    "view -> p as q",
    "p <- f x",
    "(x)",
    "{x: Nat n}",
    "Nat",
];

#[track_caller]
fn assert_same(text: &str, new: &Parsed, old: &Parsed) {
    match (new, old) {
        (Ok(new), Ok(old)) => assert_eq!(new, old, "{text:?}"),
        (Err(_), Err(_)) => {}
        _ => panic!("{text:?}: legacy {old:?}, cover {new:?}"),
    }
}

#[test]
fn cover_mode_agrees_with_the_legacy_parser() {
    for text in NESTED_AGREEMENT {
        assert_same(text, &nested(text), &legacy_nested(text));
    }
    for text in COMPLETE_AGREEMENT {
        assert_same(text, &complete(text), &legacy_complete(text));
    }
}

/// Where the legacy parser's trial order decided precedence, cover mode
/// follows the expression grammar instead: juxtaposition binds tighter than
/// `++`, `as` is looser than both, and completeness passes through `as`, a
/// view's pattern and a guarded pattern. Each case shows both readings.
const INTENDED_DIFFERENCES: &[(&str, &str, &str)] = &[
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
    for (text, old, new) in INTENDED_DIFFERENCES {
        assert_eq!(reading(&legacy_nested(text)), *old, "legacy {text:?}");
        assert_eq!(reading(&nested(text)), *new, "cover {text:?}");
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

fn sample_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("samples")];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("sample directories are readable") {
            let path = entry.expect("sample entries are readable").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "g") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[track_caller]
fn assert_agrees(report: &Report, minimum_compared: usize) {
    assert!(
        report.disagreements.is_empty(),
        "cover mode disagrees with the legacy pattern parser:\n{}",
        report.disagreements.join("\n")
    );
    assert!(
        report.compared >= minimum_compared,
        "the oracle compared only {} patterns",
        report.compared
    );
    eprintln!(
        "pattern oracle: {} compared ({} accepted)",
        report.compared, report.accepted
    );
}

#[test]
fn cover_mode_agrees_on_every_sample() {
    let files = sample_files();
    assert!(!files.is_empty());
    let report = observe(|| {
        for path in &files {
            let _ = parse_source(&std::fs::read(path).expect("samples are readable"));
        }
    });
    assert_agrees(&report, 100);
}
