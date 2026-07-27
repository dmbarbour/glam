//! Phase-zero executable contracts for the eventual `g0` macro parser.
//!
//! This module deliberately remains test-only. It uses the real source lexer
//! to pin macro-head tokenization and a dependency-independent reference
//! parser/matcher to pin the shared text-pattern language. Phase 1 replaces
//! the pattern oracle with production code; Phase 4 consumes macro heads in
//! the staged compiler. No macro expansion runs here.

use super::lexical::{LeadingTrivia, TokenKind, lex_source};

const MAX_PATTERN_BYTES: usize = 16 * 1024;
const MAX_PATTERN_GROUP_DEPTH: usize = 64;

#[derive(Debug, PartialEq, Eq)]
struct MacroHead {
    path: String,
    token_index: usize,
}

fn macro_heads(source: &str) -> Result<Vec<MacroHead>, String> {
    let lexical = lex_source(source);
    if lexical.has_errors() {
        return Err(format!(
            "source failed lexical validation: {:#?}",
            lexical.diagnostics()
        ));
    }

    let tokens = lexical.tokens();
    let mut heads = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if !matches!(tokens[index].kind(), TokenKind::Symbol("@")) {
            index += 1;
            continue;
        }

        let macro_index = index;
        index += 1;
        let Some(first) = tokens.get(index) else {
            return Err("macro head is missing its static path".to_owned());
        };
        let TokenKind::Name(first_name) = first.kind() else {
            return Err("macro head requires a joint static name path".to_owned());
        };
        if first.leading() != LeadingTrivia::Joint {
            return Err("macro head requires its first name to be joint to `@`".to_owned());
        }

        let mut path = (*first_name).to_owned();
        index += 1;
        while tokens.get(index).is_some_and(|token| {
            matches!(token.kind(), TokenKind::Symbol("."))
                && token.leading() == LeadingTrivia::Joint
        }) {
            index += 1;
            let Some(component) = tokens.get(index) else {
                return Err("macro path ends after `.`".to_owned());
            };
            let TokenKind::Name(component_name) = component.kind() else {
                return Err("macro path requires a static name after `.`".to_owned());
            };
            if component.leading() != LeadingTrivia::Joint {
                return Err("macro path components must be joint".to_owned());
            }
            path.push('.');
            path.push_str(component_name);
            index += 1;
        }

        heads.push(MacroHead {
            path,
            token_index: macro_index,
        });
    }
    Ok(heads)
}

#[test]
fn static_macro_heads_use_real_lexer_jointness() {
    let cases: &[(&str, &[&str])] = &[
        // The bootstrap accepts one joint static name or path.
        ("@name", &["name"]),
        ("@name.child", &["name.child"]),
        ("@table.create value", &["table.create"]),
        // A spaced dot begins macro input rather than extending the head.
        ("@name .child", &["name"]),
        // Heads are found structurally inside groups and attached layouts.
        ("value = (@outer @inner input)", &["outer", "inner"]),
        ("value = @outer\n  @inner input", &["outer", "inner"]),
        // Source texts and comments never expose `@` to the macro scanner.
        ("value = \"@text\" # @comment", &[]),
    ];

    for (source, expected) in cases {
        let actual = macro_heads(source)
            .unwrap_or_else(|error| panic!("{source:?} should be accepted: {error}"))
            .into_iter()
            .map(|head| head.path)
            .collect::<Vec<_>>();
        assert_eq!(actual, *expected, "{source:?}");
    }
}

#[test]
fn malformed_or_dynamic_macro_heads_are_reserved_failures() {
    let cases = [
        // Missing and spaced heads do not name a macro.
        "@",
        "@ name",
        // Dynamic lookup remains outside the bootstrap contract.
        "@(name)",
        "@.name",
        "@name.[42]",
        // Every static path component is nonempty and joint.
        "@name.",
        "@name. child",
        "@name..child",
    ];

    for source in cases {
        assert!(
            macro_heads(source).is_err(),
            "{source:?} should be rejected"
        );
    }
}

#[test]
fn declaration_expansion_order_is_right_to_left() {
    let source = "value = @outer (@middle @inner input)";
    let mut heads = macro_heads(source).unwrap();
    heads.sort_by_key(|head| std::cmp::Reverse(head.token_index));
    assert_eq!(
        heads.into_iter().map(|head| head.path).collect::<Vec<_>>(),
        ["inner", "middle", "outer"]
    );
}

#[test]
fn textual_macro_output_reserves_source_markers() {
    for accepted in ["", "plain source", "\"ordinary source text\""] {
        assert!(valid_textual_write(accepted), "{accepted:?}");
    }
    for rejected in [
        "@",
        "#",
        "left@right",
        "text # comment",
        "\"@ still rejected by write.text\"",
    ] {
        assert!(!valid_textual_write(rejected), "{rejected:?}");
    }

    // `.write.data` is intentionally absent: its arbitrary Value payload is
    // atomic and must never be scanned for either reserved source marker.
}

fn valid_textual_write(text: &str) -> bool {
    !text.contains(['@', '#'])
}

#[derive(Debug, PartialEq, Eq)]
struct Pattern {
    alternatives: Vec<Sequence>,
}

type Sequence = Vec<Repetition>;

#[derive(Debug, PartialEq, Eq)]
struct Repetition {
    atom: Atom,
    quantifier: Quantifier,
}

#[derive(Debug, PartialEq, Eq)]
enum Atom {
    Literal(char),
    Any,
    Class(Vec<ClassMember>),
    Group(Pattern),
}

#[derive(Debug, PartialEq, Eq)]
enum ClassMember {
    Scalar(char),
    Range(char, char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quantifier {
    One,
    Optional,
    ZeroOrMore,
    OneOrMore,
}

struct PatternParser {
    scalars: Vec<char>,
    cursor: usize,
}

impl PatternParser {
    fn parse(source: &str) -> Result<Pattern, String> {
        if source.len() > MAX_PATTERN_BYTES {
            return Err(format!(
                "pattern exceeds the {MAX_PATTERN_BYTES}-byte g0 limit"
            ));
        }
        let mut parser = Self {
            scalars: source.chars().collect(),
            cursor: 0,
        };
        let pattern = parser.parse_pattern(None, 0)?;
        if parser.peek().is_some() {
            return Err("unexpected closing pattern delimiter".to_owned());
        }
        Ok(pattern)
    }

    fn parse_pattern(&mut self, closing: Option<char>, depth: usize) -> Result<Pattern, String> {
        let mut alternatives = vec![self.parse_sequence(closing, depth)?];
        while self.peek() == Some('|') {
            self.cursor += 1;
            alternatives.push(self.parse_sequence(closing, depth)?);
        }
        Ok(Pattern { alternatives })
    }

    fn parse_sequence(&mut self, closing: Option<char>, depth: usize) -> Result<Sequence, String> {
        let mut sequence = Vec::new();
        while let Some(next) = self.peek() {
            if Some(next) == closing || next == '|' {
                break;
            }
            let atom = self.parse_atom(depth)?;
            let quantifier = match self.peek() {
                Some('?') => Quantifier::Optional,
                Some('*') => Quantifier::ZeroOrMore,
                Some('+') => Quantifier::OneOrMore,
                _ => Quantifier::One,
            };
            if quantifier != Quantifier::One {
                self.cursor += 1;
            }
            sequence.push(Repetition { atom, quantifier });
        }
        Ok(sequence)
    }

    fn parse_atom(&mut self, depth: usize) -> Result<Atom, String> {
        let next = self
            .next()
            .ok_or_else(|| "pattern atom expected".to_owned())?;
        match next {
            '\\' => self.parse_escape().map(Atom::Literal),
            '.' => Ok(Atom::Any),
            '(' => {
                if depth >= MAX_PATTERN_GROUP_DEPTH {
                    return Err(format!(
                        "pattern exceeds the {MAX_PATTERN_GROUP_DEPTH}-group g0 nesting limit"
                    ));
                }
                let group = self.parse_pattern(Some(')'), depth + 1)?;
                if self.next() != Some(')') {
                    return Err("unclosed pattern group".to_owned());
                }
                Ok(Atom::Group(group))
            }
            '[' => self.parse_class().map(Atom::Class),
            ')' => Err("unmatched closing pattern parenthesis".to_owned()),
            ']' => Err("unmatched closing pattern class".to_owned()),
            '?' | '*' | '+' => Err("pattern quantifier has no preceding atom".to_owned()),
            '|' => unreachable!("alternation is handled by parse_pattern"),
            ch if is_reserved_pattern_scalar(ch) => {
                Err(format!("reserved pattern scalar `{ch}` must be escaped"))
            }
            ch => Ok(Atom::Literal(ch)),
        }
    }

    fn parse_class(&mut self) -> Result<Vec<ClassMember>, String> {
        if self.peek() == Some('^') {
            return Err("g0 text patterns do not support negated classes".to_owned());
        }

        let mut members = Vec::new();
        if self.peek() == Some('-') {
            self.cursor += 1;
            members.push(ClassMember::Scalar('-'));
        }

        loop {
            match self.peek() {
                None => return Err("unclosed pattern class".to_owned()),
                Some(']') => {
                    self.cursor += 1;
                    break;
                }
                Some('-') if self.peek_second() == Some(']') => {
                    self.cursor += 1;
                    members.push(ClassMember::Scalar('-'));
                }
                Some('-') => {
                    return Err(
                        "literal `-` must be first, last, or escaped in a pattern class".to_owned(),
                    );
                }
                _ => {
                    let start = self.parse_class_scalar()?;
                    if self.peek() == Some('-') && self.peek_second() != Some(']') {
                        self.cursor += 1;
                        let end = self.parse_class_scalar()?;
                        if start > end {
                            return Err("pattern class range is reversed".to_owned());
                        }
                        members.push(ClassMember::Range(start, end));
                    } else {
                        members.push(ClassMember::Scalar(start));
                    }
                }
            }
        }

        if members.is_empty() {
            Err("pattern class must contain at least one scalar".to_owned())
        } else {
            Ok(members)
        }
    }

    fn parse_class_scalar(&mut self) -> Result<char, String> {
        let scalar = self
            .next()
            .ok_or_else(|| "pattern class scalar expected".to_owned())?;
        match scalar {
            '\\' => self.parse_escape(),
            '[' | ']' | '-' => Err(format!("pattern class scalar `{scalar}` must be escaped")),
            scalar => Ok(scalar),
        }
    }

    fn parse_escape(&mut self) -> Result<char, String> {
        let escaped = self
            .next()
            .ok_or_else(|| "pattern ends after escape marker".to_owned())?;
        if is_escapable_pattern_scalar(escaped) {
            Ok(escaped)
        } else {
            Err(format!("unsupported pattern escape `\\{escaped}`"))
        }
    }

    fn peek(&self) -> Option<char> {
        self.scalars.get(self.cursor).copied()
    }

    fn peek_second(&self) -> Option<char> {
        self.scalars.get(self.cursor + 1).copied()
    }

    fn next(&mut self) -> Option<char> {
        let scalar = self.peek()?;
        self.cursor += 1;
        Some(scalar)
    }
}

fn is_reserved_pattern_scalar(scalar: char) -> bool {
    matches!(scalar, '|' | '?' | '*' | '+' | '{' | '}' | '^' | '$' | '\\')
}

fn is_escapable_pattern_scalar(scalar: char) -> bool {
    matches!(
        scalar,
        '\\' | '.' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '-'
    )
}

#[test]
fn g0_text_pattern_acceptance_is_dependency_independent() {
    let accepted = [
        "",
        "plain",
        "λ.",
        "a|b",
        "|a|",
        "a||b",
        "(ab|c)+",
        "a?b*c+",
        "[A-Za-z0-9_+-]+",
        "[-a]",
        "[a-]",
        r"[\]\-\\]",
        r"\.\|\?\*\+\(\)\[\]\\\{\}\^\$\-",
        "#@",
    ];
    let rejected = [
        "(",
        ")",
        "[",
        "[]",
        "[^a]",
        "[z-a]",
        "[a--b]",
        "a*?",
        "a**",
        "*a",
        "a{2}",
        "^a",
        "a$",
        "(?:a)",
        "(?<name>a)",
        "(?i:a)",
        "(?=a)",
        r"\1",
        r"\d",
        r"\p{L}",
        "\\",
    ];

    for pattern in accepted {
        PatternParser::parse(pattern)
            .unwrap_or_else(|error| panic!("{pattern:?} should be accepted: {error}"));
    }
    for pattern in rejected {
        assert!(
            PatternParser::parse(pattern).is_err(),
            "{pattern:?} should be rejected"
        );
    }
}

#[test]
fn g0_text_pattern_limits_are_portable_contracts() {
    assert!(PatternParser::parse(&"a".repeat(MAX_PATTERN_BYTES)).is_ok());
    assert!(PatternParser::parse(&"a".repeat(MAX_PATTERN_BYTES + 1)).is_err());

    let deepest = format!(
        "{}a{}",
        "(".repeat(MAX_PATTERN_GROUP_DEPTH),
        ")".repeat(MAX_PATTERN_GROUP_DEPTH)
    );
    let too_deep = format!("({deepest})");
    assert!(PatternParser::parse(&deepest).is_ok());
    assert!(PatternParser::parse(&too_deep).is_err());
}

#[test]
fn g0_text_pattern_matching_is_anchored_ordered_and_greedy() {
    let cases = [
        ("a", "ba", None),
        ("a|ab", "ab", Some("a")),
        ("(a|ab)c", "abc", Some("abc")),
        ("a+", "aaab", Some("aaa")),
        ("a*ab", "aaab", Some("aaab")),
        (".", "λx", Some("λ")),
        ("[A-Z]+", "ABCd", Some("ABC")),
        ("", "anything", Some("")),
        ("|a", "a", Some("")),
        ("a|", "b", Some("")),
    ];

    for (source, input, expected) in cases {
        let pattern = PatternParser::parse(source).unwrap();
        assert_eq!(match_prefix(&pattern, input), expected, "{source:?}");
    }
}

fn match_prefix<'input>(pattern: &Pattern, input: &'input str) -> Option<&'input str> {
    match_pattern(pattern, input, 0)
        .into_iter()
        .next()
        .map(|end| &input[..end])
}

fn match_pattern(pattern: &Pattern, input: &str, cursor: usize) -> Vec<usize> {
    let mut matches = Vec::new();
    for alternative in &pattern.alternatives {
        for end in match_sequence(alternative, input, 0, cursor) {
            push_unique(&mut matches, end);
        }
    }
    matches
}

fn match_sequence(sequence: &Sequence, input: &str, piece: usize, cursor: usize) -> Vec<usize> {
    let Some(repetition) = sequence.get(piece) else {
        return vec![cursor];
    };
    let mut matches = Vec::new();
    for end in repetition_positions(repetition, input, cursor) {
        for result in match_sequence(sequence, input, piece + 1, end) {
            push_unique(&mut matches, result);
        }
    }
    matches
}

fn repetition_positions(repetition: &Repetition, input: &str, cursor: usize) -> Vec<usize> {
    match repetition.quantifier {
        Quantifier::One => match_atom(&repetition.atom, input, cursor),
        Quantifier::Optional => {
            let mut positions = match_atom(&repetition.atom, input, cursor);
            push_unique(&mut positions, cursor);
            positions
        }
        Quantifier::ZeroOrMore => repeat_atom(&repetition.atom, input, cursor),
        Quantifier::OneOrMore => {
            let mut positions = Vec::new();
            for end in match_atom(&repetition.atom, input, cursor) {
                if end == cursor {
                    push_unique(&mut positions, cursor);
                } else {
                    for repeated in repeat_atom(&repetition.atom, input, end) {
                        push_unique(&mut positions, repeated);
                    }
                }
            }
            positions
        }
    }
}

fn repeat_atom(atom: &Atom, input: &str, cursor: usize) -> Vec<usize> {
    let mut positions = Vec::new();
    for end in match_atom(atom, input, cursor) {
        if end == cursor {
            continue;
        }
        for repeated in repeat_atom(atom, input, end) {
            push_unique(&mut positions, repeated);
        }
    }
    push_unique(&mut positions, cursor);
    positions
}

fn match_atom(atom: &Atom, input: &str, cursor: usize) -> Vec<usize> {
    match atom {
        Atom::Literal(expected) => next_scalar(input, cursor)
            .filter(|(_, actual)| actual == expected)
            .map_or_else(Vec::new, |(end, _)| vec![end]),
        Atom::Any => next_scalar(input, cursor).map_or_else(Vec::new, |(end, _)| vec![end]),
        Atom::Class(members) => next_scalar(input, cursor)
            .filter(|(_, scalar)| class_contains(members, *scalar))
            .map_or_else(Vec::new, |(end, _)| vec![end]),
        Atom::Group(group) => match_pattern(group, input, cursor),
    }
}

fn next_scalar(input: &str, cursor: usize) -> Option<(usize, char)> {
    let scalar = input.get(cursor..)?.chars().next()?;
    Some((cursor + scalar.len_utf8(), scalar))
}

fn class_contains(members: &[ClassMember], scalar: char) -> bool {
    members.iter().any(|member| match member {
        ClassMember::Scalar(expected) => *expected == scalar,
        ClassMember::Range(start, end) => *start <= scalar && scalar <= *end,
    })
}

fn push_unique(values: &mut Vec<usize>, value: usize) {
    if !values.contains(&value) {
        values.push(value);
    }
}
