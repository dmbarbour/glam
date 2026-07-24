//! Token-range views and Chumsky input for the built-in `.g` parser.
//!
//! This is the sole adapter from one [`LexedSource`] to token parsers. Grammar
//! code receives an existing [`TokenView`]; it must not lex source fragments.

use std::fmt;
use std::ops::Range;

use chumsky::error::{Rich, RichPattern, RichReason};
use chumsky::input::{Input as _, MappedInput};
use chumsky::prelude::*;

use super::super::Diagnostic;
use super::lexical::{
    ByteSpan, DeclarationSection, Delimiter, DelimiterGroup, GroupId, LeadingTrivia, LexedSource,
    LexedText, SpannedToken, TextId, TokenKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TokenRange {
    start: usize,
    end: usize,
}

impl TokenRange {
    pub(super) fn new(start: usize, end: usize) -> Option<Self> {
        (start <= end).then_some(Self { start, end })
    }

    pub(super) fn start(self) -> usize {
        self.start
    }

    pub(super) fn end(self) -> usize {
        self.end
    }

    pub(super) fn len(self) -> usize {
        self.end - self.start
    }

    pub(super) fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub(super) fn contains(self, index: usize) -> bool {
        self.start <= index && index < self.end
    }

    pub(super) fn as_range(self) -> Range<usize> {
        self.start..self.end
    }
}

impl TryFrom<Range<usize>> for TokenRange {
    type Error = ();

    fn try_from(range: Range<usize>) -> Result<Self, Self::Error> {
        Self::new(range.start, range.end).ok_or(())
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct TokenView<'lex, 'source> {
    source: &'lex LexedSource<'source>,
    range: TokenRange,
}

impl<'lex, 'source> TokenView<'lex, 'source> {
    #[cfg(test)]
    pub(super) fn whole(source: &'lex LexedSource<'source>) -> Self {
        Self {
            source,
            range: TokenRange {
                start: 0,
                end: source.tokens().len(),
            },
        }
    }

    pub(super) fn new(source: &'lex LexedSource<'source>, range: TokenRange) -> Option<Self> {
        (range.end() <= source.tokens().len()).then_some(Self { source, range })
    }

    pub(super) fn declaration(
        source: &'lex LexedSource<'source>,
        declaration: &DeclarationSection,
    ) -> Self {
        let range = TokenRange::try_from(declaration.tokens())
            .expect("lexical declaration ranges are ordered");
        Self::new(source, range).expect("lexical declaration range must remain within its source")
    }

    pub(super) fn declarations(
        source: &'lex LexedSource<'source>,
    ) -> impl ExactSizeIterator<Item = (usize, Self)> + 'lex {
        source
            .declarations()
            .iter()
            .map(move |declaration| (declaration.line(), Self::declaration(source, declaration)))
    }

    pub(super) fn range(self) -> TokenRange {
        self.range
    }

    pub(super) fn len(self) -> usize {
        self.range.len()
    }

    pub(super) fn is_empty(self) -> bool {
        self.range.is_empty()
    }

    pub(super) fn source(self) -> &'lex LexedSource<'source> {
        self.source
    }

    pub(super) fn tokens(self) -> &'lex [SpannedToken<'source>] {
        &self.source.tokens()[self.range.as_range()]
    }

    #[cfg(test)]
    pub(super) fn get(self, relative_index: usize) -> Option<&'lex SpannedToken<'source>> {
        (relative_index < self.len())
            .then(|| &self.source.tokens()[self.range.start + relative_index])
    }

    pub(super) fn token_at(self, absolute_index: usize) -> Option<&'lex SpannedToken<'source>> {
        self.range
            .contains(absolute_index)
            .then(|| &self.source.tokens()[absolute_index])
    }

    pub(super) fn absolute_index(self, relative_index: usize) -> Option<usize> {
        (relative_index < self.len()).then_some(self.range.start + relative_index)
    }

    pub(super) fn subview(self, range: TokenRange) -> Option<Self> {
        (self.range.start <= range.start && range.end <= self.range.end).then_some(Self {
            source: self.source,
            range,
        })
    }

    pub(super) fn slice(self, relative: Range<usize>) -> Option<Self> {
        if relative.start > relative.end || relative.end > self.len() {
            return None;
        }
        self.subview(TokenRange {
            start: self.range.start + relative.start,
            end: self.range.start + relative.end,
        })
    }

    #[cfg(test)]
    pub(super) fn before(self, relative_index: usize) -> Option<Self> {
        self.slice(0..relative_index)
    }

    #[cfg(test)]
    pub(super) fn after(self, relative_index: usize) -> Option<Self> {
        self.slice(relative_index.checked_add(1)?..self.len())
    }

    pub(super) fn byte_span(self) -> ByteSpan {
        let start = self
            .tokens()
            .first()
            .map_or_else(|| self.end_byte(), |token| token.span().start());
        let end = self
            .tokens()
            .last()
            .map_or(start, |token| token.span().end());
        ByteSpan::new(start, end)
    }

    pub(super) fn source_text(self) -> Option<&'source str> {
        self.source.source_slice(self.byte_span())
    }

    pub(super) fn line_at_byte(self, byte: usize) -> Option<usize> {
        self.source.line_at_byte(byte)
    }

    pub(super) fn line_at_span(self, span: ByteSpan) -> Option<usize> {
        self.line_at_byte(span.start())
    }

    pub(super) fn column_at_span(self, span: ByteSpan) -> Option<usize> {
        let line = self.line_at_span(span)?;
        let line = self.line_span(line)?;
        Some(span.start().saturating_sub(line.start()))
    }

    #[cfg(test)]
    pub(super) fn source_line(self, line: usize) -> Option<&'source str> {
        self.source.source_line(line)
    }

    pub(super) fn line_indentation_at(self, absolute_index: usize) -> Option<usize> {
        (absolute_index < self.source.tokens().len())
            .then(|| {
                self.source.tokens()[..=absolute_index]
                    .iter()
                    .rev()
                    .find_map(|token| match token.kind() {
                        TokenKind::LineStart { indentation } => Some(*indentation),
                        _ => None,
                    })
            })
            .flatten()
    }

    pub(super) fn line_span(self, line: usize) -> Option<ByteSpan> {
        self.source.line_span(line)
    }

    pub(super) fn text(self, id: TextId) -> Option<&'lex LexedText> {
        self.source.text(id)
    }

    pub(super) fn group(self, id: GroupId) -> Option<&'lex DelimiterGroup> {
        self.source.group(id)
    }

    pub(super) fn group_contents(self, id: GroupId) -> Option<Self> {
        let range = TokenRange::try_from(self.source.group_contents(id)?).ok()?;
        self.subview(range)
    }

    #[cfg(test)]
    pub(super) fn group_at(
        self,
        relative_index: usize,
        delimiter: Delimiter,
    ) -> Option<(GroupId, Self)> {
        let TokenKind::Open {
            group,
            delimiter: actual,
        } = self.get(relative_index)?.kind()
        else {
            return None;
        };
        if *actual != delimiter {
            return None;
        }
        Some((*group, self.group_contents(*group)?))
    }

    pub(super) fn first_significant(self) -> Option<(usize, &'lex SpannedToken<'source>)> {
        self.tokens()
            .iter()
            .enumerate()
            .find(|(_, token)| !matches!(token.kind(), TokenKind::LineStart { .. }))
            .map(|(relative, token)| (self.range.start + relative, token))
    }

    pub(super) fn last_significant(self) -> Option<(usize, &'lex SpannedToken<'source>)> {
        self.tokens()
            .iter()
            .enumerate()
            .rev()
            .find(|(_, token)| !matches!(token.kind(), TokenKind::LineStart { .. }))
            .map(|(relative, token)| (self.range.start + relative, token))
    }

    pub(super) fn top_level(self) -> TopLevelTokens<'lex, 'source> {
        TopLevelTokens {
            view: self,
            next: self.range.start,
        }
    }

    pub(super) fn display(
        self,
        token: Option<&'lex SpannedToken<'source>>,
    ) -> TokenDisplay<'lex, 'source> {
        TokenDisplay {
            source: self.source,
            token,
        }
    }

    pub(super) fn chumsky_input(self) -> TokenInput<'lex, 'source> {
        self.tokens().map(
            ByteSpan::new(self.end_byte(), self.end_byte()),
            token_and_span as TokenMapper<'lex, 'source>,
        )
    }

    fn end_byte(self) -> usize {
        self.source
            .token(self.range.end)
            .map_or_else(|| self.source.source().len(), |token| token.span().start())
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct IndexedToken<'lex, 'source> {
    index: usize,
    token: &'lex SpannedToken<'source>,
}

impl<'lex, 'source> IndexedToken<'lex, 'source> {
    pub(super) fn index(self) -> usize {
        self.index
    }

    pub(super) fn token(self) -> &'lex SpannedToken<'source> {
        self.token
    }
}

pub(super) struct TopLevelTokens<'lex, 'source> {
    view: TokenView<'lex, 'source>,
    next: usize,
}

impl<'lex, 'source> Iterator for TopLevelTokens<'lex, 'source> {
    type Item = IndexedToken<'lex, 'source>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.view.range.end {
            return None;
        }
        let index = self.next;
        let token = self.view.source.token(index)?;
        self.next = match token.kind() {
            TokenKind::Open { group, .. } => self
                .view
                .source
                .group(*group)
                .and_then(DelimiterGroup::close_token)
                .filter(|close| *close < self.view.range.end)
                .map_or(index + 1, |close| close + 1),
            _ => index + 1,
        };
        Some(IndexedToken { index, token })
    }
}

pub(super) struct TokenDisplay<'lex, 'source> {
    source: &'lex LexedSource<'source>,
    token: Option<&'lex SpannedToken<'source>>,
}

impl fmt::Display for TokenDisplay<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(token) = self.token else {
            return formatter.write_str("end of input");
        };
        if matches!(token.kind(), TokenKind::LineStart { .. }) {
            return formatter.write_str("start of line");
        }
        match self.source.source_slice(token.span()) {
            Some(source) => write!(formatter, "`{source}`"),
            None => formatter.write_str("token"),
        }
    }
}

type TokenMapper<'lex, 'source> =
    fn(&'lex SpannedToken<'source>) -> (&'lex SpannedToken<'source>, &'lex ByteSpan);

pub(super) type TokenInput<'lex, 'source> = MappedInput<
    'lex,
    SpannedToken<'source>,
    ByteSpan,
    &'lex [SpannedToken<'source>],
    TokenMapper<'lex, 'source>,
>;

pub(super) type TokenError<'lex, 'source> = Rich<'lex, SpannedToken<'source>, ByteSpan>;
pub(super) type TokenExtra<'lex, 'source> = extra::Err<TokenError<'lex, 'source>>;

fn token_and_span<'lex, 'source>(
    token: &'lex SpannedToken<'source>,
) -> (&'lex SpannedToken<'source>, &'lex ByteSpan) {
    (token, token.span_ref())
}

pub(super) fn name<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, &'source str, TokenExtra<'lex, 'source>> {
    chumsky::primitive::select_ref(|token: &'lex SpannedToken<'source>, _| {
        if let TokenKind::Name(name) = token.kind() {
            Some(*name)
        } else {
            None
        }
    })
    .labelled("name")
}

pub(super) fn keyword<'lex, 'source: 'lex>(
    expected: &'static str,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, (), TokenExtra<'lex, 'source>> {
    chumsky::primitive::select_ref(move |token: &'lex SpannedToken<'source>, _| {
        matches!(token.kind(), TokenKind::Name(name) if *name == expected).then_some(())
    })
    .labelled(expected)
}

pub(super) fn number<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, &'source str, TokenExtra<'lex, 'source>> {
    chumsky::primitive::select_ref(|token: &'lex SpannedToken<'source>, _| {
        if let TokenKind::Number(number) = token.kind() {
            Some(*number)
        } else {
            None
        }
    })
    .labelled("number")
}

pub(super) fn text_id<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, TextId, TokenExtra<'lex, 'source>> {
    chumsky::primitive::select_ref(|token: &'lex SpannedToken<'source>, _| {
        if let TokenKind::Text(id) = token.kind() {
            Some(*id)
        } else {
            None
        }
    })
    .labelled("text")
}

pub(super) fn symbol<'lex, 'source: 'lex>(
    expected: &'static str,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, (), TokenExtra<'lex, 'source>> {
    chumsky::primitive::select_ref(move |token: &'lex SpannedToken<'source>, _| {
        matches!(token.kind(), TokenKind::Symbol(symbol) if *symbol == expected).then_some(())
    })
    .labelled(expected)
}

pub(super) fn open<'lex, 'source: 'lex>(
    expected: Delimiter,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, GroupId, TokenExtra<'lex, 'source>> {
    chumsky::primitive::select_ref(move |token: &'lex SpannedToken<'source>, _| {
        if let TokenKind::Open { group, delimiter } = token.kind()
            && *delimiter == expected
        {
            Some(*group)
        } else {
            None
        }
    })
    .labelled(delimiter_label(expected))
}

pub(super) fn close<'lex, 'source: 'lex>(
    expected: Delimiter,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, GroupId, TokenExtra<'lex, 'source>> {
    chumsky::primitive::select_ref(move |token: &'lex SpannedToken<'source>, _| {
        if let TokenKind::Close { group, delimiter } = token.kind()
            && *delimiter == expected
        {
            Some(*group)
        } else {
            None
        }
    })
    .labelled(delimiter_label(expected))
}

pub(super) fn line_start<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, usize, TokenExtra<'lex, 'source>> {
    chumsky::primitive::select_ref(|token: &'lex SpannedToken<'source>, _| {
        if let TokenKind::LineStart { indentation } = token.kind() {
            Some(*indentation)
        } else {
            None
        }
    })
    .labelled("start of line")
}

pub(super) fn joint<'lex, 'source: 'lex, O, P>(
    parser: P,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, O, TokenExtra<'lex, 'source>>
where
    P: Parser<'lex, TokenInput<'lex, 'source>, O, TokenExtra<'lex, 'source>>,
{
    leading(LeadingTrivia::Joint, "adjacent token").ignore_then(parser)
}

pub(super) fn space_before<'lex, 'source: 'lex, O, P>(
    parser: P,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, O, TokenExtra<'lex, 'source>>
where
    P: Parser<'lex, TokenInput<'lex, 'source>, O, TokenExtra<'lex, 'source>>,
{
    leading(LeadingTrivia::Space, "same-line space").ignore_then(parser)
}

#[cfg(test)]
pub(super) fn line_break_before<'lex, 'source: 'lex, O, P>(
    parser: P,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, O, TokenExtra<'lex, 'source>>
where
    P: Parser<'lex, TokenInput<'lex, 'source>, O, TokenExtra<'lex, 'source>>,
{
    leading(LeadingTrivia::LineBreak, "line break").ignore_then(parser)
}

#[cfg(test)]
pub(super) fn space_or_layout_before<'lex, 'source: 'lex, O, P>(
    parser: P,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, O, TokenExtra<'lex, 'source>>
where
    P: Parser<'lex, TokenInput<'lex, 'source>, O, TokenExtra<'lex, 'source>>,
{
    choice((
        leading(LeadingTrivia::Space, "space or line break"),
        leading(LeadingTrivia::LineBreak, "space or line break"),
    ))
    .ignore_then(parser)
}

fn leading<'lex, 'source: 'lex>(
    expected: LeadingTrivia,
    label: &'static str,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, (), TokenExtra<'lex, 'source>> {
    chumsky::primitive::select_ref(move |token: &'lex SpannedToken<'source>, _| {
        (token.leading() == expected).then_some(())
    })
    .labelled(label)
    .rewind()
}

fn delimiter_label(delimiter: Delimiter) -> &'static str {
    match delimiter {
        Delimiter::Parenthesis => "parenthesis",
        Delimiter::Bracket => "bracket",
        Delimiter::Brace => "brace",
    }
}

pub(super) struct ParseSession {
    diagnostics: Vec<Diagnostic>,
}

impl ParseSession {
    pub(super) fn new(_source: &LexedSource<'_>) -> Self {
        Self {
            diagnostics: Vec::new(),
        }
    }

    pub(super) fn record_token_errors<'lex, 'source>(
        &mut self,
        view: TokenView<'lex, 'source>,
        errors: impl IntoIterator<Item = TokenError<'lex, 'source>>,
    ) {
        self.diagnostics.extend(
            errors
                .into_iter()
                .map(|error| token_error_diagnostic(view, &error)),
        );
    }

    pub(super) fn record_token_errors_at_line<'lex, 'source>(
        &mut self,
        view: TokenView<'lex, 'source>,
        line: usize,
        errors: impl IntoIterator<Item = TokenError<'lex, 'source>>,
    ) {
        self.diagnostics.extend(errors.into_iter().map(|error| {
            let mut diagnostic = token_error_diagnostic(view, &error);
            diagnostic.line = line;
            diagnostic
        }));
    }

    pub(super) fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    pub(super) fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }
}

fn token_error_diagnostic(view: TokenView<'_, '_>, error: &TokenError<'_, '_>) -> Diagnostic {
    let line = match error.reason() {
        RichReason::Custom(_) => view.line_at_span(*error.span()).unwrap_or(1),
        RichReason::ExpectedFound { .. } if error.found().is_none() => view
            .last_significant()
            .and_then(|(_, token)| view.line_at_span(token.span()))
            .or_else(|| view.line_at_span(*error.span()))
            .unwrap_or(1),
        RichReason::ExpectedFound { .. } => view.line_at_span(*error.span()).unwrap_or(1),
    };
    let message = match error.reason() {
        RichReason::Custom(message) => message.clone(),
        RichReason::ExpectedFound { .. } => {
            let expected = error
                .expected()
                .map(|pattern| display_pattern(view, pattern))
                .collect::<Vec<_>>();
            let expected = match expected.as_slice() {
                [] => "something else".to_owned(),
                [only] => only.clone(),
                many => format!(
                    "{} or {}",
                    many[..many.len() - 1].join(", "),
                    many.last().expect("nonempty expectation list")
                ),
            };
            format!("expected {expected}, found {}", view.display(error.found()))
        }
    };
    Diagnostic::error(line, message)
}

fn display_pattern(view: TokenView<'_, '_>, pattern: &RichPattern<'_, SpannedToken<'_>>) -> String {
    match pattern {
        RichPattern::Token(token) => view.display(Some(token)).to_string(),
        RichPattern::Label(label) => label.to_string(),
        RichPattern::Identifier(identifier) => format!("`{identifier}`"),
        RichPattern::Any => "token".to_owned(),
        RichPattern::SomethingElse => "something else".to_owned(),
        RichPattern::EndOfInput => "end of input".to_owned(),
        _ => "token".to_owned(),
    }
}

#[cfg(test)]
pub(super) fn parse_expression_fragment<O>(
    source: &[u8],
    parse: impl for<'lex, 'source> FnOnce(TokenView<'lex, 'source>) -> Result<O, Vec<Diagnostic>>,
) -> Result<O, Vec<Diagnostic>> {
    let text = std::str::from_utf8(source).map_err(|error| {
        vec![Diagnostic::error(
            1,
            format!("source is not valid UTF-8: {error}"),
        )]
    })?;
    let lexical = super::lexical::lex_source(text);
    if lexical.has_errors() {
        return Err(lexical.diagnostics().to_vec());
    }
    let whole = TokenView::whole(&lexical);
    let start = whole
        .first_significant()
        .map_or(whole.range().end(), |(index, _)| index);
    let expression = whole
        .subview(TokenRange {
            start,
            end: whole.range().end(),
        })
        .expect("significant expression range must be within the whole token view");
    parse(expression)
}

#[cfg(test)]
mod tests;
