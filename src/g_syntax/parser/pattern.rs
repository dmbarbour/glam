//! Pattern grammar shared by pattern-bearing `.g` syntax.
//!
//! A pattern is parsed once by the term parser in cover mode and converted
//! by role (see `term`). Fixed quoted paths become exact list patterns;
//! computed components remain affine syntax-owned expressions until their
//! matching step.

use crate::g_syntax::{Diagnostic, SyntaxExpr, SyntaxGuardClause, SyntaxPattern};

use super::expression_context::ExpressionContext;
use super::input::{TokenRange, TokenView};
use super::lexical::{LeadingTrivia, TokenKind};
use super::structural::{is_layout_empty, parse_expression_in_context, trim_layout};
use super::term::{self, Fail, Position};

#[cfg(test)]
mod legacy;
#[cfg(test)]
mod tests;

type ParseResult<T> = Result<T, Vec<Diagnostic>>;

/// Parses a pattern in a nested position: a do or guard binding, or a part
/// of another pattern.
pub(in crate::g_syntax::parser) fn parse_pattern(
    view: TokenView<'_, '_>,
) -> ParseResult<SyntaxPattern> {
    parse_pattern_in(view, Position::Nested)
}

/// Parses a pattern that owns its complete structural range.
///
/// Match-arm `=>` gives the left side an unambiguous end, so a top-level view
/// arrow or predicate application does not need the parentheses required in
/// do-binding and nested-pattern positions.
pub(in crate::g_syntax::parser) fn parse_complete_pattern(
    view: TokenView<'_, '_>,
) -> ParseResult<SyntaxPattern> {
    parse_pattern_in(view, Position::Complete)
}

fn parse_pattern_in(view: TokenView<'_, '_>, position: Position) -> ParseResult<SyntaxPattern> {
    let view = trim_layout(view);
    let pattern = term::parse_pattern_term(view, ExpressionContext::for_owner(view), position)
        .map_err(|fail| match fail {
            Fail::Error { at, message } => vec![term::diagnostic(view, at, message)],
            Fail::Reported(diagnostics) => diagnostics,
            Fail::Unsupported(construct) => {
                error_at_view(view, format!("a pattern cannot contain {construct}"))
            }
        });
    #[cfg(test)]
    oracle::check(view, position, &pattern);
    pattern
}

pub(in crate::g_syntax::parser) fn parse_guard_clauses(
    guards: TokenView<'_, '_>,
    construct: &str,
) -> ParseResult<Vec<SyntaxGuardClause>> {
    let clauses = split_top_level_names(guards, "and");
    let mut parsed = Vec::with_capacity(clauses.len());
    for clause in clauses {
        let clause = trim_layout(clause);
        if is_layout_empty(clause) {
            return Err(error_at_view(
                guards,
                format!("{construct} contains an empty clause around `and`"),
            ));
        }
        parsed.push(parse_guard_clause(clause)?);
    }
    Ok(parsed)
}

fn parse_guard_clause(view: TokenView<'_, '_>) -> ParseResult<SyntaxGuardClause> {
    if is_wildcard_guard(view) {
        return Ok(SyntaxGuardClause::Pass);
    }

    if let Some(arrow) = top_level_symbols(view, "<-").into_iter().next() {
        let pattern = trim_layout(view_between(view, view.range().start(), arrow));
        let operation = trim_layout(view_between(view, arrow + 1, view.range().end()));
        if is_layout_empty(pattern) || is_layout_empty(operation) {
            return Err(error_at_view(
                view,
                "guard binding requires a pattern before `<-` and an operation after it",
            ));
        }
        return Ok(SyntaxGuardClause::EffectBind {
            pattern: parse_pattern(pattern)?,
            operation: parse_pattern_expression(operation)?,
        });
    }

    if let Some(equal) = top_level_symbols(view, "=").into_iter().next() {
        let pattern = trim_layout(view_between(view, view.range().start(), equal));
        let value = trim_layout(view_between(view, equal + 1, view.range().end()));
        if is_layout_empty(pattern) || is_layout_empty(value) {
            return Err(error_at_view(
                view,
                "guard value binding requires a pattern before `=` and a value after it",
            ));
        }
        return Ok(SyntaxGuardClause::ValueBind {
            pattern: parse_pattern(pattern)?,
            value: parse_pattern_expression(value)?,
        });
    }

    if let Ok(effect) = parse_pattern_expression(view) {
        return Ok(SyntaxGuardClause::Effect(effect));
    }

    for arrow in top_level_symbols(view, "->").into_iter().rev() {
        let operation = trim_layout(view_between(view, view.range().start(), arrow));
        let pattern = trim_layout(view_between(view, arrow + 1, view.range().end()));
        let Ok(operation) = parse_pattern_expression(operation) else {
            continue;
        };
        let Ok(pattern) = parse_pattern(pattern) else {
            continue;
        };
        return Ok(SyntaxGuardClause::EffectBind { pattern, operation });
    }

    Err(error_at_view(
        view,
        "expected an effect, effect-pattern binding, or value-pattern binding guard",
    ))
}

fn parse_pattern_expression(view: TokenView<'_, '_>) -> ParseResult<SyntaxExpr> {
    parse_expression_in_context(view, ExpressionContext::for_owner(view))
}

fn is_wildcard_guard(view: TokenView<'_, '_>) -> bool {
    let mut tokens = view
        .top_level()
        .filter(|indexed| !matches!(indexed.token().kind(), TokenKind::LineStart { .. }));
    matches!(
        (tokens.next(), tokens.next()),
        (Some(token), None) if matches!(token.token().kind(), TokenKind::Name("_"))
    )
}

fn top_level_symbols(view: TokenView<'_, '_>, expected: &str) -> Vec<usize> {
    view.top_level()
        .filter(|indexed| {
            matches!(indexed.token().kind(), TokenKind::Symbol(symbol) if *symbol == expected)
        })
        .map(|indexed| indexed.index())
        .collect()
}

fn top_level_names(view: TokenView<'_, '_>, expected: &str) -> Vec<usize> {
    view.top_level()
        .filter(|indexed| {
            (indexed.index() == view.range().start()
                || indexed.token().leading() != LeadingTrivia::Joint
                || indexed.index().checked_sub(1).is_some_and(|previous| {
                    view.token_at(previous)
                        .is_some_and(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
                }))
                && matches!(indexed.token().kind(), TokenKind::Name(name) if *name == expected)
        })
        .map(|indexed| indexed.index())
        .collect()
}

fn split_top_level_names<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    separator: &str,
) -> Vec<TokenView<'lex, 'source>> {
    let mut start = view.range().start();
    let mut parts = Vec::new();
    for index in top_level_names(view, separator) {
        parts.push(view_between(view, start, index));
        start = index + 1;
    }
    parts.push(view_between(view, start, view.range().end()));
    parts
}

fn error_at_view(view: TokenView<'_, '_>, message: impl Into<String>) -> Vec<Diagnostic> {
    let line = view
        .first_significant()
        .and_then(|(_, token)| view.line_at_span(token.span()))
        .unwrap_or(1);
    vec![Diagnostic::error(line, message)]
}

fn view_between<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    start: usize,
    end: usize,
) -> TokenView<'lex, 'source> {
    view.subview(TokenRange::new(start, end).expect("ordered token indices form a range"))
        .expect("pattern range remains within its source view")
}

/// The pattern oracle: while a test enables it, every pattern cover mode
/// parses is parsed again by the legacy split-and-retry parser, and any
/// disagreement is recorded. Setting `GLAM_PATTERN_ORACLE` prints every
/// disagreement any test meets instead, since some are intended:
///
/// ```sh
/// GLAM_PATTERN_ORACLE=1 cargo test --lib 2>&1 | grep -A3 'pattern oracle'
/// ```
#[cfg(test)]
pub(in crate::g_syntax::parser) mod oracle {
    use std::cell::{Cell, RefCell};

    use super::{ParseResult, Position, SyntaxPattern, TokenView, legacy};

    #[derive(Debug, Default)]
    pub(in crate::g_syntax::parser) struct Report {
        pub(in crate::g_syntax::parser) compared: usize,
        /// Compared patterns that both parsers accept.
        pub(in crate::g_syntax::parser) accepted: usize,
        pub(in crate::g_syntax::parser) disagreements: Vec<String>,
    }

    thread_local! {
        static ACTIVE: RefCell<Option<Report>> = const { RefCell::new(None) };
        static SUSPENDED: Cell<bool> = const { Cell::new(false) };
    }

    fn sweeping() -> bool {
        static SWEEPING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *SWEEPING.get_or_init(|| std::env::var_os("GLAM_PATTERN_ORACLE").is_some())
    }

    /// Runs `work` with the oracle enabled on this thread and returns its
    /// report.
    pub(in crate::g_syntax::parser) fn observe(work: impl FnOnce()) -> Report {
        ACTIVE.with(|active| *active.borrow_mut() = Some(Report::default()));
        work();
        ACTIVE.with(|active| {
            active
                .borrow_mut()
                .take()
                .expect("the oracle stays enabled")
        })
    }

    /// Runs `work` with the oracle off on this thread, as it is while the
    /// legacy parser runs. A test that counts cover mode's work suspends it.
    pub(in crate::g_syntax::parser) fn suspended<R>(work: impl FnOnce() -> R) -> R {
        let was = SUSPENDED.with(|suspended| suspended.replace(true));
        let result = work();
        SUSPENDED.with(|suspended| suspended.set(was));
        result
    }

    /// The legacy parser's reading of `view`, without comparing the patterns
    /// its expressions parse in turn.
    fn reference(view: TokenView<'_, '_>, position: Position) -> ParseResult<SyntaxPattern> {
        suspended(|| match position {
            Position::Complete => legacy::parse_complete_pattern(view),
            Position::Nested => legacy::parse_pattern(view),
        })
    }

    pub(super) fn check(
        view: TokenView<'_, '_>,
        position: Position,
        new: &ParseResult<SyntaxPattern>,
    ) {
        let active = ACTIVE.with(|active| active.borrow().is_some());
        if SUSPENDED.with(Cell::get) || !active && !sweeping() {
            return;
        }
        let old = reference(view, position);
        let agree = match (new, &old) {
            (Ok(new), Ok(old)) => new == old,
            (Err(_), Err(_)) => true,
            _ => false,
        };
        let describe = || {
            format!(
                "source: {:?} ({position:?})\n  legacy: {old:?}\n  cover: {new:?}",
                view.source_text().unwrap_or("<no text>")
            )
        };
        if !active {
            if !agree {
                eprintln!("pattern oracle disagreement\n  {}", describe());
            }
            return;
        }
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let report = active.as_mut().expect("checked as enabled");
            report.compared += 1;
            report.accepted += usize::from(matches!((new, &old), (Ok(_), Ok(_))));
            if !agree {
                report.disagreements.push(describe());
            }
        });
    }
}
