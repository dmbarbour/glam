use std::path::{Path, PathBuf};

use super::super::expression::term_oracle::{Report, observe};
use super::super::expression_context::ExpressionContext;
use super::super::parse_source;

fn source(expression: &str) -> Vec<u8> {
    format!("language g0\nx = {expression}\n").into_bytes()
}

fn oracle_over(sources: impl IntoIterator<Item = Vec<u8>>) -> Report {
    observe(|| {
        for source in sources {
            let _ = parse_source(&source);
        }
    })
}

#[track_caller]
fn assert_agrees(report: &Report, minimum_compared: usize) {
    assert!(
        report.disagreements.is_empty(),
        "the term parser disagrees with the Chumsky grammar:\n{}",
        report.disagreements.join("\n")
    );
    assert!(
        report.compared >= minimum_compared,
        "the oracle compared only {} expressions ({} unsupported: {:?})",
        report.compared,
        report.unsupported,
        report.unsupported_reasons
    );
    eprintln!(
        "term oracle: {} compared, unsupported {:?}",
        report.compared, report.unsupported_reasons
    );
}

/// Edge cases of every covered construct, valid and invalid alike: the two
/// parsers must agree on each.
const SNIPPETS: &[&str] = &[
    // Grouping, unit, tuples and sections.
    "()",
    "( )",
    "(1)",
    "((1))",
    "(1,)",
    "(1, 2)",
    "(1, 2,)",
    "(, 1)",
    "(,)",
    "(,,)",
    "(1,,2)",
    "(, 1, 2,)",
    "(+)",
    "(+ 1)",
    "(1 +)",
    "(a + b *)",
    "(- 1)",
    "(+ 1, 2)",
    "(1 + -)",
    "(+ 1 +)",
    "(++)",
    "(and)",
    "(a and)",
    "(f x)",
    "( 1 )",
    // Lists.
    "[]",
    "[,]",
    "[,,]",
    "[1,]",
    "[,1]",
    "[1,,2]",
    "[1, 2]",
    "[[1], [2, [3]]]",
    "['a]",
    "['a, b]",
    "['a.b]",
    "['_a]",
    "[ ]",
    // Dicts.
    "{}",
    "{,}",
    "{a}",
    "{a:1}",
    "{a: 1}",
    "{a :1}",
    "{a:b c}",
    "{:a}",
    "{:a b}",
    "{:a, :b}",
    "{a.b:1}",
    "{a.b}",
    "{[k]:1}",
    "{[k, 'j]:1}",
    "{(k):1}",
    "{(k)}",
    "{[]}",
    "{[]:1}",
    "{a:}",
    "{f x}",
    "{a, b}",
    "{{a:1}}",
    "{a:{b:1}}",
    "{if:1}",
    "{x y: 1}",
    // Tags and constructors.
    "a:b",
    "a:b:c",
    "a:(b)",
    "a:[b]",
    "a:{b}",
    "[k]:v",
    "[k, j]:v",
    "(k):v",
    "[]:v",
    "(1, 2):v",
    "a: b",
    "a :b",
    ":a",
    ":a.b",
    ":[k]",
    ":(k)",
    ": a",
    "f :a",
    "a.b:c",
    "a:b.c",
    "a:b c",
    // Names and paths.
    "x",
    "x.y",
    "x.y.z",
    "x.[a, 'b]",
    "x.[]",
    "x.(k)",
    "x.(k, j)",
    "x.(+)",
    "x .y",
    "x. y",
    "_prior",
    "_prior.x",
    "_",
    "_1",
    "__x",
    "_module_origin",
    "_self",
    "module",
    "module.x",
    "self",
    "module_origin",
    "X",
    "x1",
    // Quoted literals and effects.
    "'a",
    "'a.b",
    "'.a.b",
    "'.[k]",
    "'.(k)",
    "' a",
    "'.",
    "'1",
    ".eff",
    ".eff.sub",
    ".eff .sub",
    "f .eff",
    "f (.eff)",
    ".eff x",
    // Escapes.
    "^a",
    "^^a",
    "^^a.b",
    "^(a)",
    "^(a).b",
    "^ a",
    "^(a, b)",
    "^1",
    // Applications and operators.
    "f x y",
    "f(x)",
    "f (x)",
    "f [x]",
    "f {x}",
    "f 'a",
    "f :a",
    "f ^a",
    "a + b * c",
    "a < b < c",
    "a == b",
    "x and y or z",
    "a ++ b ++ c",
    "1 +",
    "+ 1",
    "f -1",
    "a+b",
    "a +b",
    "f x + g y",
    "a |> f <| b",
    "a >>= b =>> c",
    "x and",
    "1 2",
    "\"s\".x",
    "[1, 2].len",
    "{a:1}.a",
    "(a).b",
    "().x",
    // Lambdas.
    "\\x -> x",
    "\\x y -> x + y",
    "\\_ -> 1",
    "\\_x -> 1",
    "f \\x -> x",
    "f x \\y -> y",
    "a >>= \\x -> b >>= \\y -> c",
    "(\\x -> x +)",
    "\\x -> \\y -> x",
    "\\ -> x",
    "\\x x",
    "\\x ->",
    "\\1 -> x",
    "(\\x -> x) y",
    "[\\x -> x, \\y -> y]",
    "{a:\\x -> x}",
    "f(\\x -> x)",
    "\\x -> x y z",
    "a + \\x -> x",
    // Line breaks: continuation arguments, tail lambdas and anchors.
    "f\n  x",
    "f\n  x\n  y",
    "f x\n  y",
    "f\n\n  x",
    "x\n  y\n    z",
    "f\n  .x",
    "f\n  (x)",
    "f\n  [x]",
    "f\n  :a",
    "f\n  'a",
    "f\n  ^a",
    "f\n  -1",
    "f\n  - 1",
    "x\n  .y",
    "a:\n  b",
    "f\n  \\x -> x",
    "f\n  x\n  \\y -> y",
    "f \\x ->\n  x",
    "a\n  + b",
    "a\n  + b\n  + c",
    "a\n  + b\n    + c",
    "a +\n  b",
    "a\n  +\n  b",
    "a\n  and b",
    "a\n  or b",
    "a\n  |> f",
    "a\n+ b",
    "\\x ->\n  x",
    "\\x\n  -> x",
    "\\\n  x -> x",
    "\\x -> a\n  + b",
    "\\x -> a\n  + b\n  + c",
    "a + \\x -> b\n  + c",
    // Line breaks inside groups.
    "(f\n  x)",
    "(a\n  + b)",
    "(a\n + b\n + c)",
    "(a\n + b\n   + c)",
    "(+ 1\n  )",
    "(+\n  1)",
    "(1\n  +)",
    "(1 +\n  )",
    "(\n  , 1)",
    "(\n)",
    "(\n  1\n)",
    "(1,\n  2)",
    "[1,\n  2]",
    "[\n  1,\n  2,\n]",
    "[\n1,\n2\n]",
    "[f\nx]",
    "[a\n  + b, c\n      + d]",
    "{a:\n  1}",
    "{a\n  : 1}",
    "{a.b\n  :1}",
    "{[k]\n  :1}",
    "{\n  a: 1,\n  b: 2\n}",
    "{:a\n  , :b}",
    "{:\n  a}",
];

#[test]
fn term_parser_agrees_on_edge_cases() {
    let report = oracle_over(SNIPPETS.iter().map(|snippet| source(snippet)));
    assert_agrees(&report, SNIPPETS.len() / 2);
}

fn nested(depth: usize, open: &str, close: &str, inner: &str) -> String {
    format!("{}{inner}{}", open.repeat(depth), close.repeat(depth))
}

#[test]
fn term_parser_agrees_on_nesting() {
    let mut sources = Vec::new();
    for depth in 1..=6 {
        sources.push(source(&nested(depth, "(", ")", "1")));
        sources.push(source(&nested(depth, "[", "]", "1")));
        sources.push(source(&nested(depth, "{a:", "}", "1")));
        sources.push(source(&nested(depth, "([", "])", "x")));
        sources.push(source(&nested(depth, "f (", ")", "x")));
        sources.push(source(&nested(depth, "a:(", ")", "b")));
        sources.push(source(&nested(depth, "[k]:(", ")", "v")));
        sources.push(source(&nested(depth, "{(", ")}", "k")));
        sources.push(source(&nested(depth, "(\\z -> ", ")", "z")));
        sources.push(source(&nested(depth, "(1 + ", ")", "2")));
        sources.push(source(&nested(depth, "(", " +)", "1")));
    }
    let report = oracle_over(sources);
    assert_agrees(&report, 50);
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

#[test]
fn term_parser_agrees_on_every_sample() {
    let files = sample_files();
    assert!(!files.is_empty());
    let report = oracle_over(
        files
            .iter()
            .map(|path| std::fs::read(path).expect("samples are readable")),
    );
    assert_agrees(&report, 100);
}

#[test]
fn deep_nesting_parses_in_linear_time_without_recursion() {
    for (open, close) in [("(", ")"), ("[", "]"), ("{a:", "}"), ("f (", ")")] {
        let text = nested(2_000, open, close, "1");
        let started = std::time::Instant::now();
        let result = super::super::input::parse_expression_fragment(text.as_bytes(), |view| {
            super::parse_term_chain(view, ExpressionContext::for_owner(view))
                .map(|_| ())
                .map_err(|fail| vec![crate::g_syntax::Diagnostic::error(1, format!("{fail:?}"))])
        });
        assert!(result.is_ok(), "{open}…{close}: {result:?}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{open}…{close} took {:?}",
            started.elapsed()
        );
    }
}
