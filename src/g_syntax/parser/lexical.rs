//! Single-pass lexical structure for the built-in `.g` compiler.
//!
//! Production parsing consumes its diagnostics, token groups, declaration
//! sections, and text values directly. Character-oriented parsers remain only
//! as temporary test oracles pending their deletion.

use std::ops::Range;

use super::super::Diagnostic;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ByteSpan {
    start: usize,
    end: usize,
}

impl ByteSpan {
    pub(super) fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub(super) fn start(self) -> usize {
        self.start
    }

    pub(super) fn end(self) -> usize {
        self.end
    }

    #[cfg(test)]
    pub(super) fn range(self) -> Range<usize> {
        self.start..self.end
    }
}

impl chumsky::span::Span for ByteSpan {
    type Context = ();
    type Offset = usize;

    fn new(_context: Self::Context, range: Range<Self::Offset>) -> Self {
        Self::new(range.start, range.end)
    }

    fn context(&self) -> Self::Context {}

    fn start(&self) -> Self::Offset {
        self.start
    }

    fn end(&self) -> Self::Offset {
        self.end
    }
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
    Open {
        group: GroupId,
        delimiter: Delimiter,
    },
    Close {
        group: GroupId,
        delimiter: Delimiter,
    },
    LineStart {
        indentation: usize,
    },
    Unknown(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpannedToken<'source> {
    kind: TokenKind<'source>,
    span: ByteSpan,
    leading: LeadingTrivia,
}

impl<'source> SpannedToken<'source> {
    pub(super) fn kind(&self) -> &TokenKind<'source> {
        &self.kind
    }

    pub(super) fn span(&self) -> ByteSpan {
        self.span
    }

    pub(super) fn span_ref(&self) -> &ByteSpan {
        &self.span
    }

    pub(super) fn leading(&self) -> LeadingTrivia {
        self.leading
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LexedText {
    value: String,
    span: ByteSpan,
    multiline: bool,
}

impl LexedText {
    pub(super) fn value(&self) -> &str {
        &self.value
    }

    pub(super) fn span(&self) -> ByteSpan {
        self.span
    }

    pub(super) fn is_multiline(&self) -> bool {
        self.multiline
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DelimiterGroup {
    delimiter: Delimiter,
    open_token: usize,
    close_token: Option<usize>,
    parent: Option<GroupId>,
}

impl DelimiterGroup {
    pub(super) fn delimiter(&self) -> Delimiter {
        self.delimiter
    }

    pub(super) fn open_token(&self) -> usize {
        self.open_token
    }

    pub(super) fn close_token(&self) -> Option<usize> {
        self.close_token
    }

    pub(super) fn parent(&self) -> Option<GroupId> {
        self.parent
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeclarationSection {
    tokens: Range<usize>,
    span: ByteSpan,
    line: usize,
}

impl DeclarationSection {
    pub(super) fn tokens(&self) -> Range<usize> {
        self.tokens.clone()
    }

    pub(super) fn span(&self) -> ByteSpan {
        self.span
    }

    pub(super) fn line(&self) -> usize {
        self.line
    }
}

#[derive(Debug)]
pub(super) struct LexedSource<'source> {
    source: &'source str,
    tokens: Vec<SpannedToken<'source>>,
    texts: Vec<LexedText>,
    groups: Vec<DelimiterGroup>,
    declarations: Vec<DeclarationSection>,
    line_starts: Vec<usize>,
    diagnostics: Vec<Diagnostic>,
    has_errors: bool,
}

impl<'source> LexedSource<'source> {
    pub(super) fn source(&self) -> &'source str {
        self.source
    }

    pub(super) fn tokens(&self) -> &[SpannedToken<'source>] {
        &self.tokens
    }

    pub(super) fn texts(&self) -> &[LexedText] {
        &self.texts
    }

    pub(super) fn groups(&self) -> &[DelimiterGroup] {
        &self.groups
    }

    pub(super) fn declarations(&self) -> &[DeclarationSection] {
        &self.declarations
    }

    pub(super) fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub(super) fn has_errors(&self) -> bool {
        self.has_errors
    }

    pub(super) fn token(&self, index: usize) -> Option<&SpannedToken<'source>> {
        self.tokens.get(index)
    }

    pub(super) fn text(&self, id: TextId) -> Option<&LexedText> {
        self.texts.get(id)
    }

    pub(super) fn group(&self, id: GroupId) -> Option<&DelimiterGroup> {
        self.groups.get(id)
    }

    pub(super) fn group_contents(&self, id: GroupId) -> Option<Range<usize>> {
        let group = self.group(id)?;
        Some(group.open_token + 1..group.close_token?)
    }

    pub(super) fn source_slice(&self, span: ByteSpan) -> Option<&'source str> {
        self.source.get(span.start..span.end)
    }

    pub(super) fn line_at_byte(&self, byte: usize) -> Option<usize> {
        (byte <= self.source.len()).then(|| line_at(&self.line_starts, byte))
    }

    #[allow(
        dead_code,
        reason = "source-line projection is part of the phase 2 token parser substrate"
    )]
    pub(super) fn line_span(&self, line: usize) -> Option<ByteSpan> {
        let start = *self.line_starts.get(line.checked_sub(1)?)?;
        let mut end = self
            .line_starts
            .get(line)
            .copied()
            .unwrap_or(self.source.len());
        if end > start && self.source.as_bytes()[end - 1] == b'\n' {
            end -= 1;
        }
        if end > start && self.source.as_bytes()[end - 1] == b'\r' {
            end -= 1;
        }
        Some(ByteSpan::new(start, end))
    }

    #[allow(
        dead_code,
        reason = "source-line projection is part of the phase 2 token parser substrate"
    )]
    pub(super) fn source_line(&self, line: usize) -> Option<&'source str> {
        self.source_slice(self.line_span(line)?)
    }

    pub(super) fn invariants_hold(&self) -> bool {
        let source_len = self.source().len();
        self.line_starts.first() == Some(&0)
            && self
                .line_starts
                .windows(2)
                .all(|pair| pair[0] < pair[1] && pair[1] <= source_len)
            && self.tokens().iter().all(|token| {
                let span = token.span();
                let token_reference_is_valid = match token.kind() {
                    TokenKind::Text(id) => self.text(*id).is_some(),
                    TokenKind::Open { group, delimiter }
                    | TokenKind::Close { group, delimiter } => self
                        .group(*group)
                        .is_some_and(|candidate| candidate.delimiter() == *delimiter),
                    _ => true,
                };
                matches!(
                    token.leading(),
                    LeadingTrivia::Joint | LeadingTrivia::Space | LeadingTrivia::LineBreak
                ) && span.start() <= span.end()
                    && span.end() <= source_len
                    && self.source_slice(span).is_some()
                    && self.line_at_byte(span.start()).is_some()
                    && token_reference_is_valid
            })
            && self.texts().iter().enumerate().all(|(id, text)| {
                let span = text.span();
                self.text(id) == Some(text)
                    && span.start() <= span.end()
                    && span.end() <= source_len
                    && !text.value().contains('\r')
                    && self.source_slice(span).is_some_and(|source| {
                        if text.is_multiline() {
                            source.starts_with("\"\"\"")
                        } else {
                            source.starts_with('"')
                        }
                    })
            })
            && self.groups().iter().enumerate().all(|(group_id, group)| {
                self.token(group.open_token()).is_some_and(|token| {
                    matches!(
                        token.kind(),
                        TokenKind::Open { group: token_group, delimiter }
                            if *token_group == group_id && *delimiter == group.delimiter()
                    )
                }) && group.close_token().is_none_or(|close| {
                    close > group.open_token()
                        && self.token(close).is_some_and(|token| {
                            matches!(
                                token.kind(),
                                TokenKind::Close { group: token_group, delimiter }
                                    if *token_group == group_id
                                        && *delimiter == group.delimiter()
                            )
                        })
                }) && group
                    .parent()
                    .is_none_or(|parent| parent < group_id && self.group(parent).is_some())
                    && group.close_token().is_none_or(|close| {
                        self.group_contents(group_id) == Some(group.open_token() + 1..close)
                    })
            })
            && self.declarations().iter().all(|declaration| {
                let tokens = declaration.tokens();
                let span = declaration.span();
                tokens.start < tokens.end
                    && tokens.end <= self.tokens.len()
                    && span.start() < span.end()
                    && span.end() <= source_len
                    && declaration.line() == self.line_at_byte(span.start()).unwrap_or(0)
                    && self.source_slice(span).is_some()
            })
            && self.declarations().windows(2).all(|declarations| {
                declarations[0].tokens().end <= declarations[1].tokens().start
                    && declarations[0].span().end() <= declarations[1].span().start()
            })
            && self
                .diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.line > 0)
            && self.has_errors()
                == self
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.severity == crate::diagnostic::Severity::Error)
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

        self.finish_declaration(self.tokens.len());
        for group_id in self.group_stack.drain(..).rev() {
            let group = &self.groups[group_id];
            let line = line_at(&self.line_starts, self.tokens[group.open_token].span.start);
            self.structural_diagnostics.push(Diagnostic::error(
                line,
                format!("unclosed delimiter; expected `{}`", group.delimiter.close()),
            ));
        }

        let mut diagnostics = Vec::new();
        if self.newline_kinds.count_ones() > 1 {
            diagnostics.push(Diagnostic::warn(1, "source uses inconsistent line endings"));
        }
        let has_errors = !self.invalid_whitespace_diagnostics.is_empty()
            || !self.structural_diagnostics.is_empty();
        diagnostics.extend(self.invalid_whitespace_diagnostics);
        diagnostics.extend(self.structural_diagnostics);
        diagnostics.sort_by_key(|diagnostic| diagnostic.line);

        LexedSource {
            source: self.source,
            tokens: self.tokens,
            texts: self.texts,
            groups: self.groups,
            declarations: self.declarations,
            line_starts: self.line_starts,
            diagnostics,
            has_errors,
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
            self.finish_declaration(token_start);
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
                if content.is_empty() {
                    lines.push(String::new());
                } else if let Some(content) = content.strip_prefix(' ') {
                    lines.push(content.to_owned());
                } else {
                    self.structural_diagnostics.push(Diagnostic::error(
                        line_at(&self.line_starts, line_start),
                        "multiline text content requires one separator space after `\"`",
                    ));
                }
            } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                self.structural_diagnostics.push(Diagnostic::error(
                    line_at(&self.line_starts, line_start),
                    "multiline text content lines must begin with `\"`",
                ));
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
        self.emit(
            TokenKind::Open {
                group: group_id,
                delimiter,
            },
            start,
            self.index,
        );
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
        self.emit(
            TokenKind::Close {
                group: group_id,
                delimiter,
            },
            start,
            self.index,
        );
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

    fn finish_declaration(&mut self, token_end: usize) {
        let Some(open) = self.open_declaration.take() else {
            return;
        };
        let byte_end = self.tokens[token_end - 1].span.end;
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
mod tests;
