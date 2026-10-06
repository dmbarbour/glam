use std::path::{Path, PathBuf};

use super::super::expression::term_oracle::{Report, observe};
use super::super::expression_context::ExpressionContext;
use super::super::parse_source;
use crate::g_syntax::SyntaxExpr;

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
        "term oracle: {} compared ({} accepted), unsupported {:?}",
        report.compared, report.accepted, report.unsupported_reasons
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
    "f and:a",
    "f or.b:a",
    "f and.b",
    "and:a",
    "f\n  and:a",
    // Quoted-name prefixes in key paths, and operator names at paren edges.
    "['a b]",
    "['a b]:v",
    "['a.b]:v",
    "{['a b]: 1}",
    "{['a b]\n  :a}",
    ":['a b]",
    "x.['a b]",
    "x.['a, b c]",
    "(:and)",
    "('and)",
    "(x.and)",
    "(.and)",
    "(x and)",
    "((a)and)",
    "(x.or y)",
    "(and:a)",
    "(and:1)",
    "(and:a 'a []and)",
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

fn nested(depth: usize, (open, close, inner): (&str, &str, &str)) -> String {
    format!("{}{inner}{}", open.repeat(depth), close.repeat(depth))
}

/// Self-nesting shapes from the parser survey, as open, close and innermost
/// text. The Chumsky grammar is exponential in the depth of most of them.
const NESTING_SHAPES: &[(&str, &str, &str)] = &[
    ("(", ")", "1"),
    ("[", "]", "1"),
    ("{a:", "}", "1"),
    ("([", "])", "x"),
    ("f (", ")", "x"),
    ("a:(", ")", "b"),
    ("[k]:(", ")", "v"),
    ("{(", ")}", "k"),
    ("(\\z -> ", ")", "z"),
    ("(1 + ", ")", "2"),
    ("(", " +)", "1"),
];

#[test]
fn term_parser_agrees_on_nesting() {
    let sources = (1..=6).flat_map(|depth| {
        NESTING_SHAPES
            .iter()
            .map(move |shape| source(&nested(depth, *shape)))
    });
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

/// A seeded xorshift generator, so generated cases are reproducible without a
/// dependency.
struct Generator(u64);

impl Generator {
    fn next(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.0 = state;
        state
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }

    /// Layout between two pieces: mostly a space, sometimes joint, sometimes
    /// a continuation line.
    fn layout(&mut self) -> &'static str {
        self.pick(&[" ", " ", " ", " ", "", "\n  ", "\n    "])
    }

    /// Balanced token soup: mostly invalid, which tests that both parsers
    /// reject the same inputs.
    fn soup(&mut self, depth: usize, out: &mut String) {
        const TOKENS: &[&str] = &[
            "a", "b", "f", "1", "\"s\"", "_p", "'", ":", ".", "^", "\\", "->", ",", "+", "*", "-",
            "==", "and", "|>", "'a", ":a", ".e", "^a", "x.y", "a:b",
        ];
        for index in 0..1 + self.below(5) {
            if index > 0 {
                out.push_str(self.layout());
            }
            if depth < 3 && self.chance(4) {
                let (open, close) = [("(", ")"), ("[", "]"), ("{", "}")][self.below(3)];
                out.push_str(open);
                if !self.chance(6) {
                    self.soup(depth + 1, out);
                }
                out.push_str(close);
            } else {
                out.push_str(self.pick(TOKENS));
            }
        }
    }

    /// A grammar-directed expression: mostly valid, which tests that both
    /// parsers build the same tree.
    fn expression(&mut self, depth: usize, out: &mut String) {
        if depth < 3 && self.chance(8) {
            out.push_str(self.pick(&["\\x -> ", "\\x y -> ", "\\_ ->\n  "]));
        }
        self.application(depth, out);
        while self.chance(3) {
            out.push_str(self.pick(&[" ", " ", "\n  "]));
            out.push_str(self.pick(&[
                "+", "*", "-", "==", "<", "and", "or", "|>", "<|", "++", ">>=",
            ]));
            out.push_str(self.pick(&[" ", " ", "\n  "]));
            self.application(depth, out);
        }
    }

    fn application(&mut self, depth: usize, out: &mut String) {
        self.atom(depth, out);
        while self.chance(3) {
            out.push_str(self.pick(&[" ", " ", "\n  "]));
            self.atom(depth, out);
        }
    }

    fn atom(&mut self, depth: usize, out: &mut String) {
        if depth >= 3 {
            out.push_str(self.pick(&["a", "b", "1", "\"s\"", "'a", "x.y"]));
            return;
        }
        let depth = depth + 1;
        match self.below(16) {
            0 => out.push_str(self.pick(&["a", "b", "f", "x.y", "_p", "^a", "^^a.b"])),
            1 => out.push_str(self.pick(&["1", "2.5", "\"s\"", "'a", "'.a.b", ":a", ":a.b"])),
            2 => out.push_str("()"),
            3 | 4 => {
                out.push('(');
                self.expression(depth, out);
                out.push(')');
            }
            5 => {
                out.push('(');
                self.expression(depth, out);
                out.push_str(self.pick(&[", ", ",\n  ", ","]));
                if !self.chance(3) {
                    self.expression(depth, out);
                }
                out.push(')');
            }
            6 => {
                out.push('(');
                out.push_str(self.pick(&["+ ", "- ", "|> ", "and "]));
                self.expression(depth, out);
                out.push(')');
            }
            7 => {
                out.push('(');
                self.expression(depth, out);
                out.push_str(self.pick(&[" +)", " -)", " ++)", " or)"]));
            }
            8 | 9 => {
                out.push('[');
                for index in 0..self.below(4) {
                    if index > 0 {
                        out.push_str(self.pick(&[", ", ",\n  "]));
                    }
                    if self.chance(5) {
                        out.push_str("'k");
                    } else {
                        self.expression(depth, out);
                    }
                }
                out.push(']');
            }
            10 | 11 => {
                out.push('{');
                for index in 0..self.below(4) {
                    if index > 0 {
                        out.push_str(self.pick(&[", ", ",\n  "]));
                    }
                    match self.below(4) {
                        0 => out.push_str(self.pick(&[":a", ":b"])),
                        1 => self.expression(depth, out),
                        _ => {
                            out.push_str(self.pick(&["a", "a.b", "[k]", "(k)", "['k, j]"]));
                            out.push_str(self.pick(&[": ", ":", " : ", ":\n  "]));
                            self.expression(depth, out);
                        }
                    }
                }
                out.push('}');
            }
            12 => {
                out.push_str(self.pick(&["a:", "[k]:", "(k):", "a.b:"]));
                self.atom(depth, out);
            }
            13 => {
                self.atom(depth, out);
                out.push_str(self.pick(&[".a", ".[k]", ".(k)", ".a.b"]));
            }
            14 => {
                out.push_str("(\\x -> ");
                self.expression(depth, out);
                out.push(')');
            }
            _ => {
                out.push_str("(.e");
                out.push_str(self.pick(&[")", " x)", ".f)"]));
            }
        }
    }
}

/// Generated cases per test. `GLAM_TERM_FUZZ=<n>` multiplies them for a
/// deeper local search.
fn generated_count(base: usize) -> usize {
    base * std::env::var("GLAM_TERM_FUZZ")
        .ok()
        .and_then(|factor| factor.parse().ok())
        .unwrap_or(1)
}

fn generated_sources(
    seed: u64,
    count: usize,
    mut make: impl FnMut(&mut Generator, &mut String),
) -> Vec<Vec<u8>> {
    let mut generator = Generator(seed);
    (0..count)
        .map(|_| {
            let mut expression = String::new();
            make(&mut generator, &mut expression);
            source(&expression)
        })
        .collect()
}

#[test]
fn term_parser_agrees_on_generated_token_soup() {
    let report = oracle_over(generated_sources(
        0x5eed_0001,
        generated_count(10_000),
        |generator, out| generator.soup(0, out),
    ));
    assert_agrees(&report, 3_000);
}

#[test]
fn term_parser_agrees_on_generated_expressions() {
    let report = oracle_over(generated_sources(
        0x5eed_0002,
        generated_count(800),
        |generator, out| generator.expression(0, out),
    ));
    assert_agrees(&report, 600);
}

/// Parses and resolves `text` with the term parser alone, within `limit`.
#[track_caller]
fn assert_term_parses_quickly(text: &str, limit: std::time::Duration) -> SyntaxExpr {
    let started = std::time::Instant::now();
    let result = super::super::input::parse_expression_fragment(text.as_bytes(), |view| {
        super::parse_term_chain(view, ExpressionContext::for_owner(view))
            .and_then(|chain| chain.resolve().map_err(super::Fail::Error))
            .map_err(|fail| vec![crate::g_syntax::Diagnostic::error(1, format!("{fail:?}"))])
    });
    let elapsed = started.elapsed();
    let preview: String = text.chars().take(24).collect();
    assert!(elapsed < limit, "{preview}… took {elapsed:?}");
    result.unwrap_or_else(|diagnostics| panic!("{preview}…: {diagnostics:?}"))
}

#[test]
fn deep_nesting_parses_in_linear_time_without_recursion() {
    for shape in NESTING_SHAPES {
        assert_term_parses_quickly(&nested(2_000, *shape), std::time::Duration::from_secs(2));
    }
}

#[test]
fn long_flat_chains_parse_without_recursion() {
    const TERMS: usize = 100_000;
    let limit = std::time::Duration::from_secs(5);
    let trees = [
        assert_term_parses_quickly(&vec!["1"; TERMS].join(" + "), limit),
        assert_term_parses_quickly(&format!("f{}", " x".repeat(TERMS)), limit),
    ];
    // Parsing and resolution are iterative, but each tree is 100k deep, and
    // dropping a syntax tree recurses. Drop them where that fits.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || drop(trees))
        .expect("a dropping thread starts")
        .join()
        .expect("the trees drop");
}
