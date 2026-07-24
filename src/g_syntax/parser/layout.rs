use crate::g_syntax::Diagnostic;

use super::input::{TokenRange, TokenView};
use super::lexical::{Delimiter, LeadingTrivia, LexedSource, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutBase {
    FirstLine,
    /// The first member follows its introducer on the same source line.
    ///
    /// Its token column establishes the anchor even though the physical line
    /// begins before it. Later lines use ordinary indentation comparisons.
    Hanging(usize),
    #[cfg(test)]
    Indentation(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LayoutLine {
    line: usize,
    indentation: usize,
    start: usize,
    tokens: TokenRange,
}

impl LayoutLine {
    pub(super) fn line(self) -> usize {
        self.line
    }

    pub(super) fn indentation(self) -> usize {
        self.indentation
    }

    pub(super) fn start(self) -> usize {
        self.start
    }

    pub(super) fn tokens(self) -> TokenRange {
        self.tokens
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LayoutStatement {
    line: usize,
    tokens: TokenRange,
}

impl LayoutStatement {
    pub(super) fn line(self) -> usize {
        self.line
    }

    pub(super) fn tokens(self) -> TokenRange {
        self.tokens
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LayoutBlock {
    anchor: usize,
    statements: Vec<LayoutStatement>,
    end: usize,
    boundary: Option<LayoutLine>,
}

impl LayoutBlock {
    pub(super) fn anchor(&self) -> usize {
        self.anchor
    }

    pub(super) fn statements(&self) -> &[LayoutStatement] {
        &self.statements
    }

    pub(super) fn into_statements(self) -> Vec<LayoutStatement> {
        self.statements
    }

    pub(super) fn end(&self) -> usize {
        self.end
    }

    pub(super) fn boundary(&self) -> Option<LayoutLine> {
        self.boundary
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LayoutError {
    line: usize,
    message: String,
}

impl LayoutError {
    pub(super) fn line(&self) -> usize {
        self.line
    }

    pub(super) fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LayoutView<'lex, 'source> {
    tokens: TokenView<'lex, 'source>,
}

impl<'lex, 'source> LayoutView<'lex, 'source> {
    pub(super) fn new(tokens: TokenView<'lex, 'source>) -> Self {
        Self { tokens }
    }

    /// Selects next-line or hanging policy from the first significant member.
    ///
    /// A member beginning after earlier source on its physical line has a
    /// token column deeper than that line's indentation and therefore owns a
    /// hanging anchor. Source-only blank and comment lines have no tokens and
    /// cannot affect this decision.
    pub(super) fn inferred_base(self) -> LayoutBase {
        let Some((index, token)) = self.tokens.first_significant() else {
            return LayoutBase::FirstLine;
        };
        let indentation = self.tokens.line_indentation_at(index).unwrap_or(0);
        let column = self
            .tokens
            .column_at_span(token.span())
            .unwrap_or(indentation);
        if column > indentation {
            LayoutBase::Hanging(column)
        } else {
            LayoutBase::FirstLine
        }
    }

    #[cfg(test)]
    pub(super) fn tokens(self) -> TokenView<'lex, 'source> {
        self.tokens
    }

    /// Returns nonempty source lines at this view's delimiter depth.
    ///
    /// Line starts nested in a balanced group are skipped with that group, and
    /// multiline text is already one lexical token. Neither can accidentally
    /// establish layout for the surrounding construct.
    pub(super) fn lines(self) -> Vec<LayoutLine> {
        if self.tokens.is_empty() {
            return Vec::new();
        }

        let line_starts = self
            .tokens
            .top_level()
            .filter_map(|indexed| match indexed.token().kind() {
                TokenKind::LineStart { indentation } => {
                    Some((indexed.index(), *indentation, indexed.token().span()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut lines = Vec::with_capacity(line_starts.len().saturating_add(1));

        if line_starts
            .first()
            .is_none_or(|(index, _, _)| *index > self.tokens.range().start())
        {
            let start = self.tokens.range().start();
            let end = line_starts
                .first()
                .map_or(self.tokens.range().end(), |(index, _, _)| *index);
            if start < end
                && let Some(token) = self.tokens.token_at(start)
            {
                let line = self.tokens.line_at_span(token.span()).unwrap_or(1);
                let indentation = self.tokens.line_indentation_at(start).unwrap_or(0);
                lines.push(LayoutLine {
                    line,
                    indentation,
                    start,
                    tokens: TokenRange::new(start, end)
                        .expect("ordered token boundaries form a range"),
                });
            }
        }

        for (position, (line_start, indentation, span)) in line_starts.iter().enumerate() {
            let start = line_start + 1;
            let end = line_starts
                .get(position + 1)
                .map_or(self.tokens.range().end(), |(index, _, _)| *index);
            if start < end {
                lines.push(LayoutLine {
                    line: self.tokens.line_at_span(*span).unwrap_or(1),
                    indentation: *indentation,
                    start: *line_start,
                    tokens: TokenRange::new(start, end)
                        .expect("ordered token boundaries form a range"),
                });
            }
        }

        lines
    }

    /// Selects one layout body and leaves its first dedented line unconsumed.
    ///
    /// A line at the base begins a statement and a deeper line continues it.
    /// A line below the base ends the body, except that a closer-only line
    /// stays with the preceding statement because delimiter ownership is
    /// lexical. The enclosing grammar decides whether it can consume the
    /// returned boundary.
    pub(super) fn block(self, base: LayoutBase) -> Result<LayoutBlock, LayoutError> {
        let lines = self.lines();
        let Some(first) = lines.first().copied() else {
            return Ok(LayoutBlock {
                anchor: 0,
                statements: Vec::new(),
                end: self.tokens.range().end(),
                boundary: None,
            });
        };
        let (base, hanging) = match base {
            LayoutBase::FirstLine => (first.indentation, false),
            LayoutBase::Hanging(indentation) => (indentation, true),
            #[cfg(test)]
            LayoutBase::Indentation(indentation) => (indentation, false),
        };
        let mut statements: Vec<LayoutStatement> = Vec::new();

        for (position, line) in lines.into_iter().enumerate() {
            let closer_only = self.line_is_closer_only(line);
            let hanging_first = hanging && position == 0;
            if !hanging_first && line.indentation < base && !closer_only {
                return Ok(LayoutBlock {
                    anchor: base,
                    statements,
                    end: line.start,
                    boundary: Some(line),
                });
            }

            if (hanging_first || line.indentation == base) && !closer_only {
                statements.push(LayoutStatement {
                    line: line.line,
                    tokens: line.tokens,
                });
            } else if let Some(statement) = statements.last_mut() {
                statement.tokens = TokenRange::new(statement.tokens.start(), line.tokens.end())
                    .expect("continuation extends an ordered statement range");
            } else {
                return Err(LayoutError {
                    line: line.line,
                    message: "layout block begins with a continuation line".to_owned(),
                });
            }
        }

        Ok(LayoutBlock {
            anchor: base,
            statements,
            end: self.tokens.range().end(),
            boundary: None,
        })
    }

    /// Groups a view that must contain exactly one complete layout body.
    ///
    /// This compatibility wrapper keeps complete-range callers strict while
    /// structural expression parsers migrate to `block`.
    pub(super) fn statements(self, base: LayoutBase) -> Result<Vec<LayoutStatement>, LayoutError> {
        let block = self.block(base)?;
        if let Some(boundary) = block.boundary() {
            return Err(LayoutError {
                line: boundary.line,
                message: format!(
                    "layout line is indented {} spaces; expected at least {}",
                    boundary.indentation,
                    block.anchor()
                ),
            });
        }
        Ok(block.into_statements())
    }

    fn line_is_closer_only(self, line: LayoutLine) -> bool {
        let Some(view) = self.tokens.subview(line.tokens) else {
            return false;
        };
        let mut tokens = view
            .top_level()
            .filter(|token| !matches!(token.token().kind(), TokenKind::LineStart { .. }));
        let Some(first) = tokens.next() else {
            return false;
        };
        matches!(first.token().kind(), TokenKind::Close { .. })
            && tokens.all(|token| matches!(token.token().kind(), TokenKind::Close { .. }))
    }
}

/// Validates presentation inside every lexer-owned delimiter group once.
///
/// Commas or semicolons remain the sole member separators. Layout only checks
/// the column of group-level contributions made after the opening line: the
/// first member, a leading separator, or a member following a trailing
/// separator. Ordinary expression-continuation lines neither establish nor
/// move that anchor.
pub(super) fn validate_delimited_layouts(source: &LexedSource<'_>) -> Vec<Diagnostic> {
    let whole = TokenView::whole(source);
    let mut diagnostics = Vec::new();

    for (group_id, group) in source.groups().iter().enumerate() {
        if group.close_token().is_none() {
            continue;
        }
        let Some(contents) = whole.group_contents(group_id) else {
            continue;
        };
        let separator = group_separator(source, group.delimiter(), group.open_token());
        let contributions = delimited_contributions(contents, separator);

        let Some((_, first_token)) = contributions.first().copied() else {
            continue;
        };
        let first_line = whole.line_at_span(first_token.span()).unwrap_or(1);
        let anchor = whole.column_at_span(first_token.span()).unwrap_or(0);
        for (_, token) in contributions.into_iter().skip(1) {
            let indentation = whole.column_at_span(token.span()).unwrap_or(0);
            if indentation != anchor {
                diagnostics.push(Diagnostic::error(
                    whole.line_at_span(token.span()).unwrap_or(first_line),
                    format!(
                        "delimited group contribution is indented {indentation} spaces; expected content indentation {anchor}"
                    ),
                ));
            }
        }
    }

    diagnostics
}

fn group_separator(
    source: &LexedSource<'_>,
    delimiter: Delimiter,
    open_index: usize,
) -> &'static str {
    if delimiter != Delimiter::Brace || !brace_has_compound_introducer(source, open_index) {
        return ",";
    }
    ";"
}

fn brace_has_compound_introducer(source: &LexedSource<'_>, open_index: usize) -> bool {
    let Some((prior_index, prior)) = source.tokens()[..open_index]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, token)| !matches!(token.kind(), TokenKind::LineStart { .. }))
    else {
        return false;
    };
    let TokenKind::Name(name) = prior.kind() else {
        return false;
    };
    if !matches!(*name, "do" | "let" | "where" | "with") {
        return false;
    }
    if prior.leading() != LeadingTrivia::Joint {
        return true;
    }
    !prior_index.checked_sub(1).is_some_and(|previous| {
        matches!(
            source.tokens()[previous].kind(),
            TokenKind::Symbol("." | "'" | ":")
        )
    })
}

fn delimited_contributions<'lex, 'source>(
    contents: TokenView<'lex, 'source>,
    separator: &str,
) -> Vec<(usize, &'lex super::lexical::SpannedToken<'source>)> {
    let significant = contents
        .top_level()
        .filter(|indexed| !matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
        .map(|indexed| (indexed.index(), indexed.token()))
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();

    if let Some(first) = significant.first().copied() {
        candidates.push(first);
    }
    for (position, current) in significant.iter().copied().enumerate() {
        if !matches!(current.1.kind(), TokenKind::Symbol(symbol) if *symbol == separator) {
            continue;
        }
        candidates.push(current);
        if let Some(next) = significant.get(position + 1).copied() {
            candidates.push(next);
        }
    }

    candidates.sort_unstable_by_key(|(index, _)| *index);
    candidates.dedup_by_key(|(index, _)| *index);
    candidates.retain(|(index, token)| token_begins_line(contents, *index, token));
    candidates
}

fn token_begins_line(
    contents: TokenView<'_, '_>,
    index: usize,
    token: &super::lexical::SpannedToken<'_>,
) -> bool {
    token.leading() == LeadingTrivia::LineBreak
        || index.checked_sub(1).is_some_and(|previous| {
            contents
                .token_at(previous)
                .is_some_and(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
        })
}
