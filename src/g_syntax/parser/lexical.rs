//! Single-pass lexical structure for the built-in `.g` compiler.
//!
//! Production parsing currently consumes only the source-wide validation
//! results. The remaining structure is exercised differentially against the
//! text parser while declarations and expressions migrate to token input.

use std::ops::Range;

use super::super::Diagnostic;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ByteSpan {
    pub(super) start: usize,
    pub(super) end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LeadingTrivia {
    Joint,
    Space,
    LineBreak,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Delimiter {
    Parenthesis,
    Bracket,
    Brace,
}

impl Delimiter {
    fn from_open(ch: char) -> Option<Self> {
        match ch {
            '(' => Some(Self::Parenthesis),
            '[' => Some(Self::Bracket),
            '{' => Some(Self::Brace),
            _ => None,
        }
    }

    fn from_close(ch: char) -> Option<Self> {
        match ch {
            ')' => Some(Self::Parenthesis),
            ']' => Some(Self::Bracket),
            '}' => Some(Self::Brace),
            _ => None,
        }
    }

    fn close(self) -> char {
        match self {
            Self::Parenthesis => ')',
            Self::Bracket => ']',
            Self::Brace => '}',
        }
    }
}

pub(super) type GroupId = usize;
pub(super) type TextId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TokenKind<'source> {
    Name(&'source str),
    Number(&'source str),
    Text(TextId),
    Symbol(&'source str),
    Open(GroupId),
    Close(GroupId),
    LineStart { indentation: usize },
    Unknown(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpannedToken<'source> {
    pub(super) kind: TokenKind<'source>,
    pub(super) span: ByteSpan,
    pub(super) leading: LeadingTrivia,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LexedText {
    pub(super) value: String,
    pub(super) span: ByteSpan,
    pub(super) multiline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DelimiterGroup {
    pub(super) delimiter: Delimiter,
    pub(super) open_token: usize,
    pub(super) close_token: Option<usize>,
    pub(super) parent: Option<GroupId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeclarationSection {
    pub(super) tokens: Range<usize>,
    pub(super) span: ByteSpan,
    pub(super) line: usize,
}

#[derive(Debug)]
pub(super) struct LexedSource<'source> {
    pub(super) source: &'source str,
    pub(super) tokens: Vec<SpannedToken<'source>>,
    pub(super) texts: Vec<LexedText>,
    pub(super) groups: Vec<DelimiterGroup>,
    pub(super) declarations: Vec<DeclarationSection>,
    pub(super) line_starts: Vec<usize>,
    pub(super) validation_diagnostics: Vec<Diagnostic>,
    pub(super) structural_diagnostics: Vec<Diagnostic>,
    pub(super) has_invalid_whitespace: bool,
}

impl LexedSource<'_> {
    pub(super) fn invariants_hold(&self) -> bool {
        let source_len = self.source.len();
        self.line_starts.first() == Some(&0)
            && self
                .line_starts
                .windows(2)
                .all(|pair| pair[0] < pair[1] && pair[1] <= source_len)
            && self
                .tokens
                .iter()
                .all(|token| token.span.start <= token.span.end && token.span.end <= source_len)
            && self
                .texts
                .iter()
                .all(|text| text.span.start <= text.span.end && text.span.end <= source_len)
            && self.groups.iter().all(|group| {
                group.open_token < self.tokens.len()
                    && group
                        .close_token
                        .is_none_or(|close| close < self.tokens.len() && close > group.open_token)
            })
            && self.declarations.iter().all(|declaration| {
                declaration.tokens.start <= declaration.tokens.end
                    && declaration.tokens.end <= self.tokens.len()
                    && declaration.span.start <= declaration.span.end
                    && declaration.span.end <= source_len
            })
            && self
                .structural_diagnostics
                .iter()
                .all(|diagnostic| diagnostic.line > 0)
    }
}

pub(super) fn lex_source(source: &str) -> LexedSource<'_> {
    Lexer::new(source).run()
}

struct OpenDeclaration {
    token_start: usize,
    byte_start: usize,
    line: usize,
}

struct Lexer<'source> {
    source: &'source str,
    index: usize,
    line: usize,
    line_start: usize,
    indentation: usize,
    at_line_start: bool,
    emitted_line_start: bool,
    leading: LeadingTrivia,
    newline_kinds: u8,
    last_invalid_whitespace_line: Option<usize>,
    tokens: Vec<SpannedToken<'source>>,
    texts: Vec<LexedText>,
    groups: Vec<DelimiterGroup>,
    group_stack: Vec<GroupId>,
    declarations: Vec<DeclarationSection>,
    open_declaration: Option<OpenDeclaration>,
    line_starts: Vec<usize>,
    invalid_whitespace_diagnostics: Vec<Diagnostic>,
    structural_diagnostics: Vec<Diagnostic>,
}

impl<'source> Lexer<'source> {
    fn new(source: &'source str) -> Self {
        Self {
            source,
            index: 0,
            line: 1,
            line_start: 0,
            indentation: 0,
            at_line_start: true,
            emitted_line_start: false,
            leading: LeadingTrivia::Joint,
            newline_kinds: 0,
            last_invalid_whitespace_line: None,
            tokens: Vec::new(),
            texts: Vec::new(),
            groups: Vec::new(),
            group_stack: Vec::new(),
            declarations: Vec::new(),
            open_declaration: None,
            line_starts: vec![0],
            invalid_whitespace_diagnostics: Vec::new(),
            structural_diagnostics: Vec::new(),
        }
    }

    fn run(mut self) -> LexedSource<'source> {
        while self.index < self.source.len() {
            if self.consume_newline(true) {
                continue;
            }

            let ch = self.current_char();
            if ch == ' ' {
                if self.at_line_start {
                    self.indentation += 1;
                } else {
                    self.leading = LeadingTrivia::Space;
                }
                self.index += 1;
                continue;
            }
            if ch.is_whitespace() {
                self.record_invalid_whitespace(ch);
                self.leading = LeadingTrivia::Space;
                self.index += ch.len_utf8();
                continue;
            }
            if ch == '#' {
                self.consume_comment();
                continue;
            }

            self.ensure_line_start();
            if self.source[self.index..].starts_with("\"\"\"") {
                self.consume_multiline_text();
            } else if ch == '"' {
                self.consume_inline_text();
            } else if let Some(delimiter) = Delimiter::from_open(ch) {
                self.consume_open(delimiter);
            } else if let Some(delimiter) = Delimiter::from_close(ch) {
                self.consume_close(delimiter, ch);
            } else if is_number_start(self.source, self.index) {
                self.consume_number();
            } else if is_name_start(self.source, self.index) {
                self.consume_name();
            } else if is_symbol(ch) {
                self.consume_symbol();
            } else {
                let start = self.index;
                self.index += ch.len_utf8();
                self.emit(TokenKind::Unknown(ch), start, self.index);
            }
        }

        self.finish_declaration(self.tokens.len(), self.source.len());
        for group_id in self.group_stack.drain(..).rev() {
            let group = &self.groups[group_id];
            let line = line_at(&self.line_starts, self.tokens[group.open_token].span.start);
            self.structural_diagnostics.push(Diagnostic::error(
                line,
                format!("unclosed delimiter; expected `{}`", group.delimiter.close()),
            ));
        }

        let mut validation_diagnostics = Vec::new();
        if self.newline_kinds.count_ones() > 1 {
            validation_diagnostics
                .push(Diagnostic::warn(1, "source uses inconsistent line endings"));
        }
        let has_invalid_whitespace = !self.invalid_whitespace_diagnostics.is_empty();
        validation_diagnostics.extend(self.invalid_whitespace_diagnostics);

        LexedSource {
            source: self.source,
            tokens: self.tokens,
            texts: self.texts,
            groups: self.groups,
            declarations: self.declarations,
            line_starts: self.line_starts,
            validation_diagnostics,
            structural_diagnostics: self.structural_diagnostics,
            has_invalid_whitespace,
        }
    }

    fn current_char(&self) -> char {
        self.source[self.index..]
            .chars()
            .next()
            .expect("lexer index should remain within source")
    }

    fn ensure_line_start(&mut self) {
        if self.emitted_line_start {
            return;
        }
        let token_start = self.tokens.len();
        if self.group_stack.is_empty() && self.indentation == 0 {
            self.finish_declaration(token_start, self.line_start);
            self.open_declaration = Some(OpenDeclaration {
                token_start,
                byte_start: self.line_start,
                line: self.line,
            });
        }
        self.tokens.push(SpannedToken {
            kind: TokenKind::LineStart {
                indentation: self.indentation,
            },
            span: ByteSpan {
                start: self.index,
                end: self.index,
            },
            leading: self.leading,
        });
        self.leading = if self.indentation == 0 {
            LeadingTrivia::Joint
        } else {
            LeadingTrivia::Space
        };
        self.at_line_start = false;
        self.emitted_line_start = true;
    }

    fn emit(&mut self, kind: TokenKind<'source>, start: usize, end: usize) {
        self.tokens.push(SpannedToken {
            kind,
            span: ByteSpan { start, end },
            leading: self.leading,
        });
        self.leading = LeadingTrivia::Joint;
    }

    fn consume_newline(&mut self, layout: bool) -> bool {
        let bytes = self.source.as_bytes();
        let (kind, width) = match bytes.get(self.index) {
            Some(b'\r') if bytes.get(self.index + 1) == Some(&b'\n') => (0b010, 2),
            Some(b'\r') => (0b100, 1),
            Some(b'\n') => (0b001, 1),
            _ => return false,
        };
        self.newline_kinds |= kind;
        self.index += width;
        self.line += 1;
        self.line_start = self.index;
        self.line_starts.push(self.index);
        if layout {
            self.indentation = 0;
            self.at_line_start = true;
            self.emitted_line_start = false;
            self.leading = LeadingTrivia::LineBreak;
        }
        true
    }

    fn record_invalid_whitespace(&mut self, ch: char) {
        if self.last_invalid_whitespace_line == Some(self.line) {
            return;
        }
        self.last_invalid_whitespace_line = Some(self.line);
        self.invalid_whitespace_diagnostics.push(Diagnostic::error(
            self.line,
            format!(
                "unsupported whitespace U+{:04X}; .g source permits only SP, CR, and LF",
                ch as u32
            ),
        ));
    }

    fn consume_comment(&mut self) {
        while self.index < self.source.len() {
            if matches!(self.source.as_bytes()[self.index], b'\r' | b'\n') {
                break;
            }
            let ch = self.current_char();
            if ch.is_whitespace() && ch != ' ' {
                self.record_invalid_whitespace(ch);
            }
            self.index += ch.len_utf8();
        }
    }

    fn consume_inline_text(&mut self) {
        let start = self.index;
        self.index += 1;
        let mut value = String::new();
        let mut closed = false;
        while self.index < self.source.len() {
            if self.consume_newline(false) {
                value.push('\n');
                continue;
            }
            let ch = self.current_char();
            if ch == '"' {
                self.index += 1;
                closed = true;
                break;
            }
            if ch.is_whitespace() && ch != ' ' {
                self.record_invalid_whitespace(ch);
            }
            value.push(ch);
            self.index += ch.len_utf8();
        }
        if !closed {
            self.structural_diagnostics.push(Diagnostic::error(
                line_at(&self.line_starts, start),
                "unterminated text literal",
            ));
        }
        self.emit_text(value, start, self.index, false);
    }

    fn consume_multiline_text(&mut self) {
        let start = self.index;
        self.index += 3;
        self.consume_opening_multiline_tail();
        let mut lines = Vec::new();
        let mut closed = false;

        while self.index < self.source.len() {
            let line_start = self.index;
            while self.index < self.source.len()
                && !matches!(self.source.as_bytes()[self.index], b'\r' | b'\n')
            {
                let ch = self.current_char();
                if ch.is_whitespace() && ch != ' ' {
                    self.record_invalid_whitespace(ch);
                }
                self.index += ch.len_utf8();
            }
            let line = &self.source[line_start..self.index];
            let trimmed = line.trim_start_matches(' ');
            if trimmed.starts_with("\"\"\"") {
                let leading = line.len() - trimmed.len();
                self.index = line_start + leading + 3;
                closed = true;
                break;
            }
            if !trimmed.is_empty()
                && !trimmed.starts_with('#')
                && let Some(content) = trimmed.strip_prefix('"')
            {
                lines.push(content.strip_prefix(' ').unwrap_or(content).to_owned());
            }
            if !self.consume_newline(false) {
                break;
            }
        }

        if !closed {
            self.structural_diagnostics.push(Diagnostic::error(
                line_at(&self.line_starts, start),
                "unterminated multiline text literal",
            ));
        }
        self.emit_text(lines.join("\n"), start, self.index, true);
    }

    fn consume_opening_multiline_tail(&mut self) {
        while self.index < self.source.len() && self.source.as_bytes()[self.index] == b' ' {
            self.index += 1;
        }
        if self.index < self.source.len() && !self.consume_newline(false) {
            self.structural_diagnostics.push(Diagnostic::error(
                self.line,
                "multiline text opening delimiter must end its source line",
            ));
        }
    }

    fn emit_text(&mut self, value: String, start: usize, end: usize, multiline: bool) {
        let id = self.texts.len();
        self.texts.push(LexedText {
            value,
            span: ByteSpan { start, end },
            multiline,
        });
        self.emit(TokenKind::Text(id), start, end);
        self.at_line_start = false;
        self.emitted_line_start = true;
    }

    fn consume_open(&mut self, delimiter: Delimiter) {
        let start = self.index;
        self.index += 1;
        let group_id = self.groups.len();
        let open_token = self.tokens.len();
        self.groups.push(DelimiterGroup {
            delimiter,
            open_token,
            close_token: None,
            parent: self.group_stack.last().copied(),
        });
        self.emit(TokenKind::Open(group_id), start, self.index);
        self.group_stack.push(group_id);
    }

    fn consume_close(&mut self, delimiter: Delimiter, ch: char) {
        let start = self.index;
        self.index += 1;
        let Some(group_id) = self.group_stack.last().copied() else {
            self.structural_diagnostics.push(Diagnostic::error(
                self.line,
                format!("unmatched closing delimiter `{ch}`"),
            ));
            self.emit(
                TokenKind::Symbol(&self.source[start..self.index]),
                start,
                self.index,
            );
            return;
        };
        if self.groups[group_id].delimiter != delimiter {
            self.structural_diagnostics.push(Diagnostic::error(
                self.line,
                format!(
                    "mismatched closing delimiter `{ch}`; expected `{}`",
                    self.groups[group_id].delimiter.close()
                ),
            ));
            self.emit(
                TokenKind::Symbol(&self.source[start..self.index]),
                start,
                self.index,
            );
            return;
        }
        let close_token = self.tokens.len();
        self.emit(TokenKind::Close(group_id), start, self.index);
        self.groups[group_id].close_token = Some(close_token);
        self.group_stack.pop();
    }

    fn consume_number(&mut self) {
        let start = self.index;
        if self.source.as_bytes()[self.index] == b'_' {
            self.index += 1;
        }
        while self.index < self.source.len() {
            let ch = self.current_char();
            if ch.is_ascii_digit()
                || matches!(
                    ch,
                    '_' | '.'
                        | 'x'
                        | 'X'
                        | 'b'
                        | 'B'
                        | 'e'
                        | 'E'
                        | 'a'
                        | 'A'
                        | 'c'
                        | 'C'
                        | 'd'
                        | 'D'
                        | 'f'
                        | 'F'
                )
            {
                self.index += ch.len_utf8();
            } else {
                break;
            }
        }
        self.emit(
            TokenKind::Number(&self.source[start..self.index]),
            start,
            self.index,
        );
    }

    fn consume_name(&mut self) {
        let start = self.index;
        self.index += self.current_char().len_utf8();
        while self.index < self.source.len() {
            let ch = self.current_char();
            if ch.is_ascii_alphanumeric() || ch == '_' {
                self.index += ch.len_utf8();
            } else {
                break;
            }
        }
        self.emit(
            TokenKind::Name(&self.source[start..self.index]),
            start,
            self.index,
        );
    }

    fn consume_symbol(&mut self) {
        const SYMBOLS: &[&str] = &[
            "::=", ">>=", ">=>", "=>>", ":=", "<-", "->", "!>", "<!", ">=", "==", "<>", "=<", ">>",
            "<<", "|>", "<|", "++",
        ];
        let start = self.index;
        if let Some(symbol) = SYMBOLS
            .iter()
            .find(|symbol| self.source[self.index..].starts_with(**symbol))
        {
            self.index += symbol.len();
        } else {
            self.index += self.current_char().len_utf8();
        }
        self.emit(
            TokenKind::Symbol(&self.source[start..self.index]),
            start,
            self.index,
        );
    }

    fn finish_declaration(&mut self, token_end: usize, byte_end: usize) {
        let Some(open) = self.open_declaration.take() else {
            return;
        };
        self.declarations.push(DeclarationSection {
            tokens: open.token_start..token_end,
            span: ByteSpan {
                start: open.byte_start,
                end: byte_end,
            },
            line: open.line,
        });
    }
}

fn is_name_start(source: &str, index: usize) -> bool {
    let ch = source[index..]
        .chars()
        .next()
        .expect("name check should remain within source");
    ch.is_ascii_alphabetic()
        || ch == '_'
            && source[index + ch.len_utf8()..]
                .chars()
                .next()
                .is_none_or(|next| !next.is_ascii_digit())
}

fn is_number_start(source: &str, index: usize) -> bool {
    let ch = source[index..]
        .chars()
        .next()
        .expect("number check should remain within source");
    ch.is_ascii_digit()
        || ch == '_'
            && source[index + ch.len_utf8()..]
                .chars()
                .next()
                .is_some_and(|next| next.is_ascii_digit())
}

fn is_symbol(ch: char) -> bool {
    matches!(
        ch,
        '.' | ','
            | ';'
            | ':'
            | '\''
            | '\\'
            | '^'
            | '='
            | '<'
            | '>'
            | '!'
            | '|'
            | '+'
            | '-'
            | '*'
            | '/'
            | '@'
    )
}

fn line_at(line_starts: &[usize], byte: usize) -> usize {
    line_starts.partition_point(|start| *start <= byte)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::g_syntax::parser::source::parse_source;

    #[test]
    fn scans_validation_structure_and_declarations_together() {
        let source = "language g0\r\nvalue = do {\n  text = \"#;[]\"\n  .r (text)\n}\nnext = 1\n";
        let lexed = lex_source(source);

        assert_eq!(
            lexed.validation_diagnostics,
            vec![Diagnostic::warn(1, "source uses inconsistent line endings")]
        );
        assert_eq!(lexed.declarations.len(), 3);
        assert_eq!(
            lexed
                .declarations
                .iter()
                .map(|declaration| declaration.line)
                .collect::<Vec<_>>(),
            [1, 2, 6]
        );
        assert_eq!(lexed.groups.len(), 2);
        assert!(lexed.groups.iter().all(|group| group.close_token.is_some()));
        assert_eq!(lexed.texts[0].value, "#;[]");
        assert!(lexed.structural_diagnostics.is_empty());
    }

    #[test]
    fn decodes_multiline_text_once_and_excludes_source_only_lines() {
        let source = concat!(
            "language g0\n",
            "text =\n",
            "    \"\"\"\n",
            "      \" first  \n",
            "  # source-only comment\n",
            "\n",
            "  \" second # retained\n",
            "      \" \"\"\" retained\n",
            "    \"\"\"\n",
        );
        let lexed = lex_source(source);

        assert_eq!(lexed.texts.len(), 1);
        assert_eq!(
            lexed.texts[0].value,
            "first  \nsecond # retained\n\"\"\" retained"
        );
        assert!(lexed.texts[0].multiline);
        assert!(lexed.structural_diagnostics.is_empty());
    }

    #[test]
    fn reports_delimiter_errors_without_mistaking_text_for_structure() {
        let lexed = lex_source("language g0\nvalue = ([\"})]\"]}\n");

        assert_eq!(lexed.groups.len(), 2);
        assert_eq!(lexed.texts[0].value, "})]");
        assert_eq!(lexed.structural_diagnostics.len(), 2);
        assert!(
            lexed.structural_diagnostics[0]
                .message
                .contains("mismatched closing delimiter")
        );
        assert!(
            lexed.structural_diagnostics[1]
                .message
                .contains("unclosed delimiter")
        );
    }

    #[test]
    fn source_validation_matches_the_previous_contract() {
        let source = concat!(
            "language g0\r\n",
            "separator\t= 1\n",
            "text = \"tab\there\"\n",
            "# non-breaking space: \u{00A0}\n",
        );
        let lexed = lex_source(source);

        assert!(lexed.has_invalid_whitespace);
        assert_eq!(
            lexed
                .validation_diagnostics
                .iter()
                .map(|diagnostic| (diagnostic.line, diagnostic.severity))
                .collect::<Vec<_>>(),
            [
                (1, crate::diagnostic::Severity::Warning),
                (2, crate::diagnostic::Severity::Error),
                (3, crate::diagnostic::Severity::Error),
                (4, crate::diagnostic::Severity::Error),
            ]
        );
    }

    #[test]
    fn declaration_sections_match_valid_syntax_samples() {
        for path in sample_files(Path::new("samples/syntax")) {
            let bytes = fs::read(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let source = std::str::from_utf8(&bytes)
                .unwrap_or_else(|error| panic!("{} is not UTF-8: {error}", path.display()));
            let lexed = lex_source(source);
            let parsed = parse_source(&bytes);
            assert_eq!(
                lexed.declarations.len(),
                parsed.declarations.len(),
                "{}",
                path.display()
            );
            assert_eq!(
                lexed
                    .declarations
                    .iter()
                    .map(|declaration| declaration.line)
                    .collect::<Vec<_>>(),
                parsed
                    .declarations
                    .iter()
                    .map(|declaration| declaration.line)
                    .collect::<Vec<_>>(),
                "{}",
                path.display()
            );
        }
    }

    fn sample_files(folder: &Path) -> Vec<PathBuf> {
        let mut paths = fs::read_dir(folder)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", folder.display()))
            .flat_map(|entry| {
                let path = entry
                    .expect("sample directory entry should be readable")
                    .path();
                if path.is_dir() {
                    sample_files(&path)
                } else if path.extension().is_some_and(|extension| extension == "g") {
                    vec![path]
                } else {
                    Vec::new()
                }
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }
}
