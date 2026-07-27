//! Versioned, capture-free text patterns shared by source-facing effects.
//!
//! The accepted language belongs to Glam rather than to the regex backend.
//! Keep validation and the syntax model in this module so dependency upgrades
//! cannot silently expand the language contract.

use regex_lite::{Regex, RegexBuilder};

pub(crate) const MAX_PATTERN_BYTES: usize = 16 * 1024;
pub(crate) const MAX_PATTERN_GROUP_DEPTH: usize = 64;

const MAX_COMPILED_BYTES: usize = 1024 * 1024;

/// A validated `g0` text pattern with an implementation-private matcher.
#[derive(Debug)]
pub(crate) struct TextPattern {
    matcher: Regex,
}

impl TextPattern {
    pub(crate) fn parse(source: &str) -> Result<Self, String> {
        let syntax = Parser::parse(source)?;
        let backend_source = syntax.backend_source();
        let mut builder = RegexBuilder::new(&backend_source);
        builder
            .nest_limit(u32::try_from(MAX_PATTERN_GROUP_DEPTH).expect("small fixed limit"))
            .size_limit(MAX_COMPILED_BYTES);
        let matcher = builder.build().map_err(|error| {
            format!("validated text pattern was rejected by the matcher backend: {error}")
        })?;
        Ok(Self { matcher })
    }

    /// Returns the ordered, greedy match at the beginning of `input`.
    pub(crate) fn match_prefix<'input>(&self, input: &'input str) -> Option<&'input str> {
        self.matcher
            .find(input)
            .filter(|matched| matched.start() == 0)
            .map(|matched| &input[..matched.end()])
    }
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

impl Pattern {
    fn backend_source(&self) -> String {
        let mut output = String::new();
        self.write_backend(&mut output);
        output
    }

    fn write_backend(&self, output: &mut String) {
        for (index, sequence) in self.alternatives.iter().enumerate() {
            if index != 0 {
                output.push('|');
            }
            for repetition in sequence {
                repetition.write_backend(output);
            }
        }
    }
}

impl Repetition {
    fn write_backend(&self, output: &mut String) {
        self.atom.write_backend(output);
        match self.quantifier {
            Quantifier::One => {}
            Quantifier::Optional => output.push('?'),
            Quantifier::ZeroOrMore => output.push('*'),
            Quantifier::OneOrMore => output.push('+'),
        }
    }
}

impl Atom {
    fn write_backend(&self, output: &mut String) {
        match self {
            Self::Literal(scalar) => write_literal(output, *scalar),
            Self::Any => output.push('.'),
            Self::Class(members) => {
                output.push('[');
                for member in members {
                    match member {
                        ClassMember::Scalar(scalar) => write_class_literal(output, *scalar),
                        ClassMember::Range(start, end) => {
                            write_class_literal(output, *start);
                            output.push('-');
                            write_class_literal(output, *end);
                        }
                    }
                }
                output.push(']');
            }
            Self::Group(pattern) => {
                // User-visible groups do not capture. The backend spelling is
                // deliberately absent from the Glam pattern grammar.
                output.push_str("(?:");
                pattern.write_backend(output);
                output.push(')');
            }
        }
    }
}

fn write_literal(output: &mut String, scalar: char) {
    if matches!(
        scalar,
        '\\' | '.' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$'
    ) {
        output.push('\\');
    }
    output.push(scalar);
}

fn write_class_literal(output: &mut String, scalar: char) {
    if matches!(scalar, '\\' | '[' | ']' | '-' | '^') {
        output.push('\\');
    }
    output.push(scalar);
}

struct Parser {
    scalars: Vec<char>,
    cursor: usize,
}

impl Parser {
    fn parse(source: &str) -> Result<Pattern, String> {
        if source.len() > MAX_PATTERN_BYTES {
            return Err(format!(
                "text pattern exceeds the supported {MAX_PATTERN_BYTES}-byte limit"
            ));
        }
        let mut parser = Self {
            scalars: source.chars().collect(),
            cursor: 0,
        };
        let pattern = parser.parse_pattern(None, 0)?;
        if parser.peek().is_some() {
            return Err("unexpected closing text-pattern delimiter".to_owned());
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
            .ok_or_else(|| "text-pattern atom expected".to_owned())?;
        match next {
            '\\' => self.parse_escape().map(Atom::Literal),
            '.' => Ok(Atom::Any),
            '(' => {
                if depth >= MAX_PATTERN_GROUP_DEPTH {
                    return Err(format!(
                        "text pattern exceeds the supported {MAX_PATTERN_GROUP_DEPTH}-group nesting limit"
                    ));
                }
                let group = self.parse_pattern(Some(')'), depth + 1)?;
                if self.next() != Some(')') {
                    return Err("unclosed text-pattern group".to_owned());
                }
                Ok(Atom::Group(group))
            }
            '[' => self.parse_class().map(Atom::Class),
            ')' => Err("unmatched closing text-pattern parenthesis".to_owned()),
            ']' => Err("unmatched closing text-pattern class".to_owned()),
            '?' | '*' | '+' => Err("text-pattern quantifier has no preceding atom".to_owned()),
            '|' => unreachable!("alternation is handled by parse_pattern"),
            scalar if is_reserved_scalar(scalar) => Err(format!(
                "reserved text-pattern scalar `{scalar}` must be escaped"
            )),
            scalar => Ok(Atom::Literal(scalar)),
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
                None => return Err("unclosed text-pattern class".to_owned()),
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
                        "literal `-` must be first, last, or escaped in a text-pattern class"
                            .to_owned(),
                    );
                }
                _ => {
                    let start = self.parse_class_scalar()?;
                    if self.peek() == Some('-') && self.peek_second() != Some(']') {
                        self.cursor += 1;
                        let end = self.parse_class_scalar()?;
                        if start > end {
                            return Err("text-pattern class range is reversed".to_owned());
                        }
                        members.push(ClassMember::Range(start, end));
                    } else {
                        members.push(ClassMember::Scalar(start));
                    }
                }
            }
        }

        if members.is_empty() {
            Err("text-pattern class must contain at least one scalar".to_owned())
        } else {
            Ok(members)
        }
    }

    fn parse_class_scalar(&mut self) -> Result<char, String> {
        let scalar = self
            .next()
            .ok_or_else(|| "text-pattern class scalar expected".to_owned())?;
        match scalar {
            '\\' => self.parse_escape(),
            '[' | ']' | '-' => Err(format!(
                "text-pattern class scalar `{scalar}` must be escaped"
            )),
            scalar => Ok(scalar),
        }
    }

    fn parse_escape(&mut self) -> Result<char, String> {
        let escaped = self
            .next()
            .ok_or_else(|| "text pattern ends after escape marker".to_owned())?;
        if is_escapable_scalar(escaped) {
            Ok(escaped)
        } else {
            Err(format!("unsupported text-pattern escape `\\{escaped}`"))
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

fn is_reserved_scalar(scalar: char) -> bool {
    matches!(scalar, '|' | '?' | '*' | '+' | '{' | '}' | '^' | '$' | '\\')
}

fn is_escapable_scalar(scalar: char) -> bool {
    matches!(
        scalar,
        '\\' | '.' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '-'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_and_rejected_syntax_is_owned_by_glam() {
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
            TextPattern::parse(pattern)
                .unwrap_or_else(|error| panic!("{pattern:?} should be accepted: {error}"));
        }
        for pattern in rejected {
            assert!(
                TextPattern::parse(pattern).is_err(),
                "{pattern:?} should be rejected"
            );
        }
    }

    #[test]
    fn portable_limits_are_checked_before_the_backend() {
        assert!(TextPattern::parse(&"a".repeat(MAX_PATTERN_BYTES)).is_ok());
        assert!(TextPattern::parse(&"a".repeat(MAX_PATTERN_BYTES + 1)).is_err());

        let deepest = format!(
            "{}a{}",
            "(".repeat(MAX_PATTERN_GROUP_DEPTH),
            ")".repeat(MAX_PATTERN_GROUP_DEPTH)
        );
        assert!(TextPattern::parse(&deepest).is_ok());
        assert!(TextPattern::parse(&format!("({deepest})")).is_err());
    }

    #[test]
    fn backend_preserves_anchored_ordered_and_greedy_matching() {
        let cases = [
            ("a", "ba", None),
            ("a|ab", "ab", Some("a")),
            ("(a|ab)c", "abc", Some("abc")),
            ("a+", "aaab", Some("aaa")),
            ("a*ab", "aaab", Some("aaab")),
            (".", "λx", Some("λ")),
            ("[A-Z]+", "ABCd", Some("ABC")),
            (r"\.", ".tail", Some(".")),
            (r"[\]\-\\]+", "]-\\tail", Some("]-\\")),
            ("#@", "#@tail", Some("#@")),
            ("", "anything", Some("")),
            ("|a", "a", Some("")),
            ("a|", "b", Some("")),
        ];

        for (source, input, expected) in cases {
            let pattern = TextPattern::parse(source).unwrap();
            assert_eq!(pattern.match_prefix(input), expected, "{source:?}");
        }
    }
}
