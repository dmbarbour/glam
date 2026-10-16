//! Pattern grammar shared by pattern-bearing `.g` syntax.
//!
//! A pattern is parsed once by the term parser in cover mode and converted
//! by role (see `term`). Fixed quoted paths become exact list patterns;
//! computed components remain affine syntax-owned expressions until their
//! matching step.

use crate::g_syntax::keywords::g0_keyword;
use crate::g_syntax::{Diagnostic, SyntaxExpr, SyntaxGuardClause, SyntaxPattern};

use super::expression_context::ExpressionContext;
use super::input::{TokenRange, TokenView};
use super::lexical::TokenKind;
use super::structural::{
    in_keyword_position, is_layout_empty, parse_expression_extent, parse_expression_in_context,
    trim_layout,
};
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
    // A keyword form's tail in a view's expression ends at the view's arrow.
    let context = ExpressionContext::for_owner(view).ending_at_arrow();
    let pattern = term::parse_pattern_term(view, context, position).map_err(|fail| match fail {
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
    Ok(
        match parse_binding(view, ExpressionContext::for_owner(view), BindingSite::Guard)? {
            Binding::Effect { pattern, operation } => {
                SyntaxGuardClause::EffectBind { pattern, operation }
            }
            Binding::Value { pattern, value } => SyntaxGuardClause::ValueBind { pattern, value },
            Binding::Operation(effect) => SyntaxGuardClause::Effect(effect),
        },
    )
}

/// A do statement or guard clause, split at its binder.
pub(in crate::g_syntax::parser) enum Binding {
    /// `Pattern <- Operation` or `Operation -> Pattern`.
    Effect {
        pattern: SyntaxPattern,
        operation: SyntaxExpr,
    },
    /// `Pattern = Value`.
    Value {
        pattern: SyntaxPattern,
        value: SyntaxExpr,
    },
    /// An operation alone.
    Operation(SyntaxExpr),
}

/// Where a binding stands, which words its errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::g_syntax::parser) enum BindingSite {
    DoStatement,
    Guard,
}

/// The part a binding lacks.
#[derive(Clone, Copy)]
enum Missing {
    BackwardPattern,
    BackwardOperation,
    ValuePattern,
    Value,
    ForwardOperation,
    ForwardPattern,
}

impl BindingSite {
    fn missing(self, part: Missing) -> &'static str {
        match (self, part) {
            (Self::DoStatement, Missing::BackwardPattern) => {
                "a do backward binding requires a pattern before `<-`"
            }
            (Self::DoStatement, Missing::BackwardOperation) => {
                "a do backward binding requires an operation after `<-`"
            }
            (Self::DoStatement, Missing::ValuePattern) => {
                "a do value binding requires a pattern before `=`"
            }
            (Self::DoStatement, Missing::Value) => "a do value binding requires a value after `=`",
            (Self::DoStatement, Missing::ForwardOperation) => {
                "a do forward binding requires an operation before `->`"
            }
            (Self::DoStatement, Missing::ForwardPattern) => {
                "a do forward binding requires a pattern after `->`"
            }
            (Self::Guard, Missing::BackwardPattern | Missing::BackwardOperation) => {
                "guard binding requires a pattern before `<-` and an operation after it"
            }
            (Self::Guard, Missing::ValuePattern | Missing::Value) => {
                "guard value binding requires a pattern before `=` and a value after it"
            }
            (Self::Guard, Missing::ForwardOperation | Missing::ForwardPattern) => {
                "guard binding requires an operation before `->` and a pattern after it"
            }
        }
    }
}

/// Parses a do statement or guard clause once, with no split tried and
/// abandoned.
///
/// It binds with `<-` or `=` when that token stands at its top level before
/// any `\` and any keyword but `as` and `when`. A pattern's top level holds
/// no lambda and no keyword form, so a keyword form owns every `<-` or `=`
/// after its head: `if (a, b) = e then ...` is an operation, and
/// `x = if ...` a binding. The left side is then a pattern and the right an
/// expression. Otherwise the clause is an operation, which may end at a
/// top-level `->` that a pattern follows: an open tail before it, such as
/// an `else` branch, ends there too.
pub(in crate::g_syntax::parser) fn parse_binding(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
    site: BindingSite,
) -> ParseResult<Binding> {
    let view = trim_layout(view);
    if let Some(binder) = pattern_binder(view) {
        let backward = view
            .token_at(binder)
            .is_some_and(|token| matches!(token.kind(), TokenKind::Symbol("<-")));
        let (missing_pattern, missing_right) = if backward {
            (Missing::BackwardPattern, Missing::BackwardOperation)
        } else {
            (Missing::ValuePattern, Missing::Value)
        };
        let pattern = trim_layout(view_between(view, view.range().start(), binder));
        let right = trim_layout(view_between(view, binder + 1, view.range().end()));
        if is_layout_empty(pattern) {
            return Err(error_at_view(view, site.missing(missing_pattern)));
        }
        let pattern = parse_pattern(pattern)?;
        if is_layout_empty(right) {
            return Err(error_at_view(view, site.missing(missing_right)));
        }
        let right = parse_expression_in_context(right, context)?;
        return Ok(if backward {
            Binding::Effect {
                pattern,
                operation: right,
            }
        } else {
            Binding::Value {
                pattern,
                value: right,
            }
        });
    }
    if view
        .first_significant()
        .is_some_and(|(_, token)| matches!(token.kind(), TokenKind::Symbol("->")))
    {
        return Err(error_at_view(view, site.missing(Missing::ForwardOperation)));
    }
    let operation = parse_expression_extent(view, context.ending_at_arrow())?;
    let rest = trim_layout(view_between(view, operation.end(), view.range().end()));
    let operation = operation
        .into_expression()
        .map_err(|message| error_at_view(view, message))?;
    let Some((arrow, _)) = rest.first_significant() else {
        return Ok(Binding::Operation(operation));
    };
    let pattern = trim_layout(view_between(view, arrow + 1, view.range().end()));
    if is_layout_empty(pattern) {
        return Err(error_at_view(view, site.missing(Missing::ForwardPattern)));
    }
    Ok(Binding::Effect {
        pattern: parse_pattern(pattern)?,
        operation,
    })
}

/// The `<-` or `=` that makes a clause a binding: the first at its top
/// level, before any `\` and any keyword but `as` and `when`.
pub(in crate::g_syntax::parser) fn pattern_binder(view: TokenView<'_, '_>) -> Option<usize> {
    for indexed in view.top_level() {
        match indexed.token().kind() {
            TokenKind::Symbol("<-" | "=") => return Some(indexed.index()),
            TokenKind::Symbol("\\") => return None,
            TokenKind::Name(name)
                if !matches!(*name, "as" | "when")
                    && g0_keyword(name).is_some()
                    && in_keyword_position(view, indexed.index()) =>
            {
                return None;
            }
            _ => {}
        }
    }
    None
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

fn split_top_level_names<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    separator: &str,
) -> Vec<TokenView<'lex, 'source>> {
    let mut start = view.range().start();
    let mut parts = Vec::new();
    for indexed in view.top_level() {
        if matches!(indexed.token().kind(), TokenKind::Name(name) if *name == separator)
            && in_keyword_position(view, indexed.index())
        {
            parts.push(view_between(view, start, indexed.index()));
            start = indexed.index() + 1;
        }
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
