//! Delimiter-depth-aware physical lines and inferred layout blocks.
//!
//! `LayoutView::block` chooses next-line or hanging layout from its first
//! member and leaves the first dedented line unconsumed. Construct-specific
//! parsers decide whether that boundary is a valid member error or an
//! enclosing expression resumption.

use crate::g_syntax::Diagnostic;

use super::input::{TokenRange, TokenView};
use super::lexical::{Delimiter, LeadingTrivia, LexedSource, TokenKind};
use super::structural::{in_keyword_position, token_is_name, trim_layout, view_between};
use super::term::may_head_keyword_form;

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

    #[cfg(test)]
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
    hanging: bool,
    statements: Vec<LayoutStatement>,
    end: usize,
    boundary: Option<LayoutLine>,
}

impl LayoutBlock {
    pub(super) fn anchor(&self) -> usize {
        self.anchor
    }

    #[cfg(test)]
    pub(super) fn hanging(&self) -> bool {
        self.hanging
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

#[derive(Debug, Clone, Copy)]
pub(super) struct LayoutView<'lex, 'source> {
    tokens: TokenView<'lex, 'source>,
}

impl<'lex, 'source> LayoutView<'lex, 'source> {
    pub(super) fn new(tokens: TokenView<'lex, 'source>) -> Self {
        Self { tokens }
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
    pub(super) fn block(self) -> LayoutBlock {
        let lines = self.lines();
        let Some(first) = lines.first().copied() else {
            return LayoutBlock {
                anchor: 0,
                hanging: false,
                statements: Vec::new(),
                end: self.tokens.range().end(),
                boundary: None,
            };
        };
        let (base, hanging) = self.inferred_anchor(first);
        self.block_with_anchor(lines, base, hanging)
            .unwrap_or_else(|_| unreachable!("inferred layout starts at its first member"))
    }

    fn inferred_anchor(self, first: LayoutLine) -> (usize, bool) {
        let Some((_, token)) = self
            .tokens
            .subview(first.tokens)
            .and_then(TokenView::first_significant)
        else {
            return (first.indentation, false);
        };
        let column = self
            .tokens
            .column_at_span(token.span())
            .unwrap_or(first.indentation);
        if column > first.indentation {
            (column, true)
        } else {
            (first.indentation, false)
        }
    }

    fn block_with_anchor(
        self,
        lines: Vec<LayoutLine>,
        base: usize,
        hanging: bool,
    ) -> Result<LayoutBlock, ()> {
        let mut statements: Vec<LayoutStatement> = Vec::new();

        for (position, line) in lines.into_iter().enumerate() {
            let closer_only = self.line_is_closer_only(line);
            let hanging_first = hanging && position == 0;
            if !hanging_first && line.indentation < base && !closer_only {
                return Ok(LayoutBlock {
                    anchor: base,
                    hanging,
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
                return Err(());
            }
        }

        Ok(LayoutBlock {
            anchor: base,
            hanging,
            statements,
            end: self.tokens.range().end(),
            boundary: None,
        })
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

/// The `as` tokens that end a match's subject, in order: in each group's
/// contents, the first `as` in keyword position after a match head belongs
/// to the innermost head still reading its subject, as in the parser. A
/// braced block after one holds arms, separated by `;`, where a `{` after
/// any other `as` begins an as-pattern's dictionary, separated by `,`. The
/// lexer records them once, as it records each token's enclosing group.
pub(super) fn match_arms_keywords(source: &LexedSource<'_>) -> Vec<usize> {
    let view = TokenView::whole(source);
    // Per open group, the match heads still reading their subjects.
    let mut pending = vec![0usize];
    let mut found = Vec::new();
    for (index, token) in source.tokens().iter().enumerate() {
        match token.kind() {
            TokenKind::Open { .. } => pending.push(0),
            TokenKind::Close { .. } if pending.len() > 1 => {
                pending.pop();
            }
            // A declaration ends any subject left without its `as`.
            TokenKind::LineStart { indentation: 0 } if pending.len() == 1 => pending[0] = 0,
            TokenKind::Name("match" | "try_match")
                if may_head_keyword_form(view, index) && !heads_match_when(view, index) =>
            {
                *pending.last_mut().expect("the source level stays") += 1;
            }
            TokenKind::Name("as") if in_keyword_position(view, index) => {
                let heads = pending.last_mut().expect("the source level stays");
                if *heads > 0 {
                    *heads -= 1;
                    found.push(index);
                }
            }
            _ => {}
        }
    }
    found
}

pub(super) fn group_separator(
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
    if *name == "as" {
        return source
            .match_arms_keywords()
            .binary_search(&prior_index)
            .is_ok();
    }
    if !matches!(*name, "do" | "let" | "when" | "where" | "with") {
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

/// Whether the match head at `index` begins a `match when`, which takes no
/// `as`.
fn heads_match_when(view: TokenView<'_, '_>, index: usize) -> bool {
    let star = view.token_at(index + 1).is_some_and(|token| {
        matches!(token.kind(), TokenKind::Symbol("*")) && token.leading() == LeadingTrivia::Joint
    });
    let after = trim_layout(view_between(
        view,
        (index + 1 + usize::from(star)).min(view.range().end()),
        view.range().end(),
    ));
    after.first_significant().is_some_and(|(when, token)| {
        token_is_name(token, "when") && in_keyword_position(after, when)
    })
}
