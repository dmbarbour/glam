use std::path::{Path, PathBuf};

use super::super::expression_context::ExpressionContext;
use super::super::parse_source;
use crate::g_syntax::{Severity, SyntaxExpr};

fn source(expression: &str) -> Vec<u8> {
    format!("language g0\nx = {expression}\n").into_bytes()
}

/// Parses every source, valid or not, to its end: the parser reports what
/// it rejects and never panics. Returns how many it accepted.
fn parse_all(sources: impl IntoIterator<Item = Vec<u8>>) -> usize {
    sources
        .into_iter()
        .filter(|source| {
            parse_source(source)
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity != Severity::Error)
        })
        .count()
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
    // Keyword forms.
    "if c then a else b",
    "f if c then a else b",
    "a if c else b",
    "f x if c else b",
    "a + b if c else d",
    "(a if c else b)",
    "[a if c else b]",
    "[if c then a else b, 1]",
    "(\\a -> b) if c else d",
    "\\a -> (b if c else d)",
    "\\a -> b if c else d",
    "x + \\a -> b if c else d",
    "f \\a -> b if c else d",
    "[\\a -> b if c else d]",
    "a if c else b + 1",
    "a\n  if c else b",
    "f x\n  if c else b",
    "\\a -> b\n  if c else d",
    "[a\n  if c else b]",
    "\\a -> \\b -> c if p else q",
    "a if",
    "do { .r 1 }",
    "f do { .r 1 }",
    "do { .r 1 }.x",
    "match y as { z => z }",
    "match y as\n  z => z\n  _ => 1",
    "[match y as { z => z }, 1]",
    "(1, match y as { z => z })",
    "using d in x",
    "abstract_global_path foo.bar",
    "abstract_global_path module.foo",
    "abstract_global_path module",
    "abstract_global_path foo.[1]",
    "abstract_global_path\n  foo",
    "f abstract_global_path foo",
    "if:()",
    "if:x.y",
    "[[] if:a] :a",
    "f if:a",
    "f {fx.y :if}",
    "{a :b}",
    "_p and:and",
    "x and:y",
    "x.if",
    "'if",
    ".if",
    "then",
    "a then b",
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
fn term_parser_reads_edge_cases() {
    assert!(parse_all(SNIPPETS.iter().map(|snippet| source(snippet))) >= SNIPPETS.len() / 2);
}

fn nested(depth: usize, (open, close, inner): (&str, &str, &str)) -> String {
    format!("{}{inner}{}", open.repeat(depth), close.repeat(depth))
}

/// Self-nesting shapes from the parser survey, as open, close and innermost
/// text. The retired Chumsky grammar was exponential in most of them.
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
fn term_parser_reads_nesting() {
    let sources = (1..=6).flat_map(|depth| {
        NESTING_SHAPES
            .iter()
            .map(move |shape| source(&nested(depth, *shape)))
    });
    assert_eq!(parse_all(sources), 6 * NESTING_SHAPES.len());
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
fn term_parser_reads_every_sample() {
    let files = sample_files();
    assert!(!files.is_empty());
    parse_all(
        files
            .iter()
            .map(|path| std::fs::read(path).expect("samples are readable")),
    );
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
            "==", "and", "|>", "'a", ":a", ".e", "^a", "x.y", "a:b", "if", "then", "else", "match",
            "with", "as", "do", "=>", ";",
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
        match self.below(19) {
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
                            out.push_str(self.pick(&[":", ":", ":", ": ", " :", ":\n  "]));
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
            15 => {
                out.push_str("(.e");
                out.push_str(self.pick(&[")", " x)", ".f)"]));
            }
            16 => {
                out.push_str("(if ");
                self.expression(depth, out);
                out.push_str(" then ");
                self.expression(depth, out);
                out.push_str(self.pick(&[" else ", "\n  else "]));
                self.expression(depth, out);
                out.push(')');
            }
            17 => {
                out.push('(');
                self.application(depth, out);
                out.push_str(" if ");
                self.expression(depth, out);
                out.push_str(" else ");
                self.expression(depth, out);
                out.push(')');
            }
            _ => {
                let grouped = self.chance(2);
                out.push_str(if grouped { "(match " } else { "match " });
                self.atom(depth, out);
                out.push_str(" as { z => ");
                self.expression(depth, out);
                out.push_str("; _ => ");
                self.atom(depth, out);
                out.push_str("; }");
                if grouped {
                    out.push(')');
                }
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
fn term_parser_reads_generated_token_soup() {
    parse_all(generated_sources(
        0x5eed_0001,
        generated_count(10_000),
        |generator, out| generator.soup(0, out),
    ));
}

#[test]
fn term_parser_reads_generated_expressions() {
    parse_all(generated_sources(
        0x5eed_0002,
        generated_count(800),
        |generator, out| generator.expression(0, out),
    ));
}

/// Parses and resolves `text` with the term parser alone, within `limit`.
#[track_caller]
fn assert_term_parses_quickly(text: &str, limit: std::time::Duration) -> SyntaxExpr {
    let started = std::time::Instant::now();
    let result = super::super::input::parse_expression_fragment(text.as_bytes(), |view| {
        super::parse_term_chain(view, ExpressionContext::for_owner(view))
            .and_then(|(chain, _)| {
                chain
                    .resolve()
                    .map_err(|message| super::Fail::Error { at: None, message })
            })
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

/// Keyword frames nested in each position a frame closes from: for
/// conditionals a group, an `else` branch, a `then` group, an argument, a
/// postfix fallback, a lambda body and a guard; for braced `do` blocks a
/// statement's operation and value; for layout `do` blocks a hanging first
/// statement, bare and in a group; for matches a subject, an arm's result
/// and guard, a `match when` arm's result, nested arms, and a statement's
/// operation. Frames cost heap, not Rust stack, so each shape parses 2,000
/// deep.
const KEYWORD_FRAME_NESTING_SHAPES: &[(&str, &str, &str)] = &[
    ("(if c then ", " else 0)", "1"),
    ("if c then 1 else ", "", "0"),
    ("if c then (", ") else 0", "1"),
    ("(try c then ", " else 0)", "1"),
    ("f (if c then ", " else 0)", "1"),
    ("(x if c else ", ")", "0"),
    ("(\\z -> if c then ", " else z)", "1"),
    ("if (", ") then 1 else 0", "c"),
    ("(do { ", " -> z; .r z })", ".r 1"),
    ("do { x <- ", "; .r x }", ".r 1"),
    ("do { x = ", "; .r x }", "1"),
    ("do ", "", ".r 1"),
    ("(do ", ")", ".r 1"),
    ("match ", " as {_ => 1}", "x"),
    ("match x as {_ => ", "}", "1"),
    ("match x as {_ when ", " => 1}", "c"),
    ("match when {c => ", "}", "1"),
    ("match x as {_ when {c => ", "}}", "1"),
    ("do { x <- match y as {_ => ", "}; .r x }", ".r 1"),
    ("let a = 1 in ", "", "a"),
    ("let a = ", " in a", "1"),
    ("(let a = 1 in ", ")", "a"),
    ("let {a = ", "} in a", "1"),
    ("1 + let a = 1 in ", "", "a"),
    ("using m in ", "", "x"),
    ("using ", " in x", "m"),
    ("(a where a = ", ")", "1"),
    ("let a = b where b = ", " in a", "1"),
    ("do { .r a where a = ", "; .r 1 }", "1"),
];

/// Layout arms nested in arms' results, each a line deeper.
#[test]
fn layout_arms_nest_in_linear_time_without_recursion() {
    let depth = 1_000;
    let mut text = String::new();
    for level in 0..depth {
        text.push_str("match x as\n");
        text.push_str(&"  ".repeat(level + 1));
        text.push_str("_ => ");
    }
    text.push('1');
    assert_term_parses_quickly(&text, std::time::Duration::from_secs(2));
}

#[test]
fn keyword_frame_nesting_parses_in_linear_time_without_recursion() {
    for shape in KEYWORD_FRAME_NESTING_SHAPES {
        assert_term_parses_quickly(&nested(2_000, *shape), std::time::Duration::from_secs(2));
    }
}

/// Shapes the structural conditional parser mishandled, which conditional
/// frames read as the rest of the grammar does.
#[test]
fn conditional_frames_read_branches_as_expressions() {
    let parse = |text: &str| {
        super::super::structural::parse_compound_expression_fragment(text.as_bytes())
            .unwrap_or_else(|diagnostics| panic!("{text}: {diagnostics:?}"))
    };
    // A form in a parenthesized branch ended at its group, past `else`.
    assert!(matches!(
        parse("(if a then using m in y else b)"),
        SyntaxExpr::If(conditional) if matches!(*conditional.then_result, SyntaxExpr::Using { .. })
    ));
    // A postfix conditional in a `then` branch ends at the outer `else`.
    assert!(matches!(
        parse("if a then x if c else d else b"),
        SyntaxExpr::If(conditional)
            if matches!(*conditional.then_result, SyntaxExpr::If(_))
                && matches!(*conditional.else_result, SyntaxExpr::Name(_))
    ));
    // A `with` after a tag's `match` payload is the match's own, not an
    // update's, in a branch as at the head of a view.
    for text in [
        "f (k):match s as { z => 1 }",
        "if c then 1 else f (k):match s as { z => 1 }",
    ] {
        parse(text);
    }
}

/// A conditional's guards may span lines, and its parent branch still
/// checks those lines against its floor: here the fallback begins on a line
/// indented 2, so its continuation must be indented further.
#[test]
fn conditional_guard_lines_count_for_the_enclosing_floor() {
    let parsed =
        parse_source(b"language g0\nx = a if b\n  - c else (if d\n  and e then f else g)\n");
    assert!(
        parsed.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("indented 2 spaces; expected at least 3")),
        "{:?}",
        parsed.diagnostics
    );
}

/// A group's lines, here a braced match's arms in a branch, count toward
/// the floor of the branch around it.
#[test]
fn group_lines_count_for_the_enclosing_floor() {
    let parsed = parse_source(
        b"language g0\nx = f if g\n  - h else (match b as { z => y\n  y; _ => y; })\n",
    );
    assert!(
        parsed.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("indented 2 spaces; expected at least 3")),
        "{:?}",
        parsed.diagnostics
    );
}

/// Keyword forms nested in groups, as open, close and innermost text. Each
/// group is interpreted once: a second interpretation at each level would
/// double the work below it.
const KEYWORD_NESTING_SHAPES: &[(&str, &str, &str)] = &[
    ("(if c then ", " else 0)", "1"),
    ("f (if c then ", " else 0)", "1"),
    ("(try c then ", " else 0)", "1"),
    ("(match x as {_ => ", "})", "1"),
    ("(let a = 1 in ", ")", "a"),
    ("(do { .r ", " })", "1"),
    ("(\\z -> if c then ", " else z)", "1"),
    ("(do { ", " -> z; .r z })", ".r 1"),
    ("(if f -> z then ", " else 0)", "1"),
    ("(match x as {(f -> p) => ", "})", "1"),
];

/// Keywords nested where they head no form: as path keys, quoted atoms, tag
/// keys and tag path roots, dict keys, and contextual keywords as path and
/// tag keys. None opens a keyword region, so the term parser interprets each
/// group itself, once; a region it took anyway would cost a level of Rust
/// recursion.
const KEYWORD_KEY_NESTING_SHAPES: &[(&str, &str, &str)] = &[
    ("(x.if ", ")", "1"),
    ("(f 'match ", ")", "1"),
    ("(x.do.y ", ")", "1"),
    ("(:if ", ")", "1"),
    ("{if: ", "}", "1"),
    ("(if.x:", ")", "1"),
    ("(f if:", ")", "1"),
    ("(then:", ")", "1"),
    ("(f where:", ")", "1"),
    ("[x.try, ", "]", "1"),
    ("(foo.then ", ")", "1"),
    ("(f 'else ", ")", "1"),
];

/// Counts group interpretations rather than timing the parse. Twelve levels
/// tell once from twice (4,095 interpretations of 12 groups), and a
/// regression fails at once instead of hanging the suite.
#[track_caller]
fn assert_each_group_interpreted_once(shapes: &[(&str, &str, &str)]) {
    for shape in shapes {
        let text = nested(12, *shape);
        let groups = text
            .chars()
            .filter(|c| matches!(c, '(' | '[' | '{'))
            .count();
        super::INTERPRETED_GROUPS.with(|count| count.set(0));
        let parsed = super::super::structural::parse_compound_expression_fragment(text.as_bytes());
        let interpreted = super::INTERPRETED_GROUPS.with(std::cell::Cell::get);
        let preview: String = text.chars().take(24).collect();
        assert!(parsed.is_ok(), "{preview}…: {parsed:?}");
        assert!(
            interpreted <= groups,
            "{preview}… interpreted {interpreted} groups of {groups}"
        );
    }
}

/// Self-nesting pattern shapes, as open, close and innermost text. The
/// legacy pattern parser's split-and-retry was quadratic in most of them.
const PATTERN_NESTING_SHAPES: &[(&str, &str, &str)] = &[
    ("(", " as q)", "(p)"),
    ("(", " ++ [q])", "[p]"),
    ("[", ", q]", "p"),
    ("(", ", q)", "p"),
    ("{a: ", ", r}", "p"),
    ("t:(", ")", "p"),
    ("(Nat ", ")", "n"),
    ("(f -> ", ")", "p"),
    ("(", " <- f)", "p"),
    ("(", " when g)", "p"),
];

#[test]
fn pattern_nesting_interprets_each_group_once() {
    for shape in PATTERN_NESTING_SHAPES {
        let text = nested(12, *shape);
        let groups = text
            .chars()
            .filter(|c| matches!(c, '(' | '[' | '{'))
            .count();
        super::INTERPRETED_GROUPS.with(|count| count.set(0));
        let parsed = super::super::input::parse_expression_fragment(
            text.as_bytes(),
            super::super::pattern::parse_pattern,
        );
        let interpreted = super::INTERPRETED_GROUPS.with(std::cell::Cell::get);
        let preview: String = text.chars().take(24).collect();
        assert!(parsed.is_ok(), "{preview}…: {parsed:?}");
        assert!(
            interpreted <= groups,
            "{preview}… interpreted {interpreted} groups of {groups}"
        );
    }
}

#[test]
fn keyword_forms_in_groups_interpret_each_group_once() {
    assert_each_group_interpreted_once(KEYWORD_NESTING_SHAPES);
}

#[test]
fn keywords_as_keys_interpret_each_group_once() {
    assert_each_group_interpreted_once(KEYWORD_KEY_NESTING_SHAPES);
}

/// A keyword region the term parser takes anyway interprets its groups on
/// demand, one level of Rust recursion each, so keys must open none: nested
/// as deep as plain groups, they parse without recursion too.
#[test]
fn keywords_as_keys_nest_without_recursion() {
    for shape in KEYWORD_KEY_NESTING_SHAPES {
        assert_term_parses_quickly(&nested(2_000, *shape), std::time::Duration::from_secs(2));
    }
}

/// Keywords where they head no form, each before a group a region would
/// defer: as path keys, quoted atoms and quoted paths, tag keys and
/// constructors, dict keys, effect names, and names that only contain one.
const KEYWORDS_AS_KEYS: &[&str] = &[
    "x.if (a)",
    "x.match [a] (b)",
    "x.do {a: 1}",
    "x.try (a) (b)",
    "x.try_match (a)",
    "x.using (a)",
    "a.b.if.c (d)",
    "x.do.y (a)",
    "f 'if (a)",
    "f 'do [a]",
    "f '.a.match (b)",
    "if:(a)",
    "match:[a] (b)",
    "do.x:(a)",
    "if.x:(a)",
    "f if:(a)",
    "f where:(a)",
    "if.x (a)",
    "if.x then (a) else b",
    "x where.y (a)",
    ":if (a)",
    ":do.x (a)",
    "{if: (a), b: (c)}",
    "{do: 1, using: (a)}",
    "{:if, :match}",
    "{x.if: (a)}",
    ".do (a)",
    ".if (a) (b)",
    "[x.try, (a)]",
    "(x.match, (a))",
    "foo.then (a)",
    "f 'else (a)",
    "x.where (a)",
    "{then: (a), else: [b]}",
    "f 'with (a)",
    "x.in (a)",
    "x.as (a)",
    "iff (a)",
    "do_it (a)",
    "if_ (a)",
    "matcher [a]",
    "f (x.if) (if c then (a) else (b))",
    "{if: (if c then (a) else b)}",
];

#[test]
fn keywords_as_keys_parse() {
    // A keyword rooting a path is an error; the rest are keys.
    assert!(
        parse_all(KEYWORDS_AS_KEYS.iter().map(|text| source(text))) >= KEYWORDS_AS_KEYS.len() / 2
    );
}

/// Regions only decide which parser interprets a group: the parse with them
/// and without them is the same, trees and diagnostics alike.
#[test]
fn keyword_regions_never_change_a_parse() {
    let shapes = NESTING_SHAPES
        .iter()
        .chain(KEYWORD_NESTING_SHAPES)
        .chain(KEYWORD_KEY_NESTING_SHAPES);
    let sources = sample_files()
        .into_iter()
        .map(|path| std::fs::read(path).expect("samples are readable"))
        .chain(
            SNIPPETS
                .iter()
                .chain(KEYWORDS_AS_KEYS)
                .map(|text| source(text)),
        )
        .chain(shapes.flat_map(|shape| (1..=6).map(move |depth| source(&nested(depth, *shape)))))
        .chain(generated_sources(
            0x5eed_0001,
            generated_count(10_000),
            |generator, out| generator.soup(0, out),
        ))
        .chain(generated_sources(
            0x5eed_0002,
            generated_count(800),
            |generator, out| generator.expression(0, out),
        ));
    let mut compared = 0;
    for source in sources {
        let with = parse_source(&source);
        let without = super::without_regions(|| parse_source(&source));
        assert_eq!(
            with,
            without,
            "keyword regions changed the parse of {:?}",
            String::from_utf8_lossy(&source)
        );
        compared += 1;
    }
    eprintln!("keyword regions: {compared} parses unchanged");
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

/// The tail rule (Syntax.md "Tail Position"): a tail form runs to the end of
/// its region, which a closer, a separator, a line end or a separator
/// keyword ends, and only an operator or a keyword resumes after its `}` or
/// a dedent.
#[test]
fn tail_forms_stand_alone_only_in_tail_position() {
    const OUTSIDE: &[&str] = &[
        "f match a as {_ => 1} y",
        "match a as {_ => 1} y",
        "f do { a } y",
        "f do\n    a\n  y",
        "f match a as\n    _ => 1\n  y",
        "do { .r 1 }.x",
        "if y <- if a then b else c and d then 1 else 2",
        "match v as {p when x <- \\z -> z and d => 1}",
        "if y <- let a = 1 in a and d then 1 else 2",
    ];
    const INSIDE: &[&str] = &[
        "f (match a as {_ => 1}) y",
        "f match a as\n    _ => 1",
        "f match a as\n    _ => 1\n  |> g",
        "(do { .r 1 }).x",
        "do { .r 1 } >>= k",
        "do\n    (if c then .a else .b) -> p\n    .r p",
        "do\n    if c then .a else .b -> p\n    .r p",
        "do\n    f \\a -> a -> p\n    .r p",
        "do\n    a if c else \\z -> z -> p\n    .r p",
        "if a then if b then c else d else e",
        "if a then if b then c else d\n  else e",
        "if a then \\z -> z else b",
        "if any \\z -> z then 1 else 2",
        "if c then do a else b",
        "match v as {p when any \\z -> z => 1}",
        "match \\z -> z as {_ => 1}",
        "match if a then b else c as {_ => 1}",
        "match (d with { alias e; a := 1 }) as {_ => 1}",
        "do\n    a if c else b -> p\n    .r p",
        "do\n    p <- f \\a -> a\n    .r p",
        "if y <- (if a then b else c) and d then 1 else 2",
        "if a then (if b then c else d) else e",
        "if c then do { a } else do { b }",
        "if c then do\n      a\n  else do\n      b",
        "match match a as {_ => 1} as {_ => 2}",
        "1 + if a then b else c",
        "map xs \\z -> z",
        "if a then b else c where c = 1",
        "[if a then b else c, \\z -> z]",
        "1 + let a = 1 in a",
        "f let a = 1 in a",
        "if c then let a = 1 in a else b",
        "match let a = 1 in a as {_ => 1}",
        "do\n    let a = 1 in .r a -> p\n    .r p",
    ];
    for text in OUTSIDE {
        let parsed = parse_source(&source(text));
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("not in tail position")),
            "{text:?} reported {:#?}",
            parsed.diagnostics
        );
    }
    for text in INSIDE {
        let parsed = parse_source(&source(text));
        assert!(
            parsed
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity != Severity::Error),
            "{text:?} reported {:#?}",
            parsed.diagnostics
        );
    }
}
