//! Do statements and guard clauses read from their pieces.
//!
//! The binding rule is `pattern::parse_binding`'s, applied to pieces. A
//! clause binds with `<-` or `=` when that token stands before any `\` and
//! any keyword but `as` and `when`; a keyword form ends the search, whether
//! a token in a keyword region or a frame's form piece. Otherwise the clause
//! is an operation, which may end at a `->` that a pattern follows. A side
//! holding a form that view-level dispatch parses is parsed from its view.

use crate::g_syntax::keywords::{canonical_keyword, g0_keyword, reserved_keyword_message};
use crate::g_syntax::{SyntaxExpr, SyntaxGuardClause, SyntaxPattern};

use super::super::do_expr::ParsedDoStatement;
use super::super::expression_context::{ExpressionContext, validate_expression_floor};
use super::super::input::{TokenRange, TokenView};
use super::super::lexical::{Delimiter, TokenKind};
use super::super::pattern::{Binding, BindingSite, Missing};
use super::super::structural::{
    before_key_mark, in_keyword_position, parse_expression_extent, trim_layout,
};
use super::{Fail, Items, Mode, Parse, Piece, Position, Scope, piece_token, token, trim_padding};

/// A guard's clauses, split at each `and` in keyword position.
pub(super) fn guard_clauses(
    scope: Scope<'_, '_, '_>,
    pieces: &[Piece],
    construct: &str,
) -> Parse<Vec<SyntaxGuardClause>> {
    let view = scope.view;
    let at = pieces.first().map(piece_token);
    let mut clauses = Vec::new();
    for clause in pieces.split(|piece| is_keyword(view, piece, "and")) {
        let clause = trim_padding(view, clause);
        let Some(first) = clause.first() else {
            return Err(
                Fail::error(format!("{construct} contains an empty clause around `and`")).or_at(at),
            );
        };
        if let [Piece::Token(index)] = clause
            && matches!(token(view, *index).kind(), TokenKind::Name("_"))
        {
            clauses.push(SyntaxGuardClause::Pass);
            continue;
        }
        let context = ExpressionContext::for_owner_at(view, piece_token(first));
        clauses.push(match binding(scope, clause, context, BindingSite::Guard)? {
            Binding::Effect { pattern, operation } => {
                SyntaxGuardClause::EffectBind { pattern, operation }
            }
            Binding::Value { pattern, value } => SyntaxGuardClause::ValueBind { pattern, value },
            Binding::Operation(effect) => SyntaxGuardClause::Effect(effect),
        });
    }
    Ok(clauses)
}

/// A do statement: a `declare`, or a binding.
pub(super) fn statement(
    scope: Scope<'_, '_, '_>,
    pieces: &[Piece],
    context: ExpressionContext,
) -> Parse<ParsedDoStatement> {
    let view = scope.view;
    if let Some(Piece::Token(head)) = pieces.first()
        && leads_with(view, pieces, "declare")
    {
        return declare(view, *head, &pieces[1..]);
    }
    Ok(
        match binding(scope, pieces, context, BindingSite::DoStatement)? {
            Binding::Effect { pattern, operation } => {
                ParsedDoStatement::Bind { pattern, operation }
            }
            Binding::Value { pattern, value } => ParsedDoStatement::ValueBind { pattern, value },
            Binding::Operation(expression) => ParsedDoStatement::Expr(expression),
        },
    )
}

/// `declare name, name`.
fn declare(view: TokenView<'_, '_>, head: usize, names: &[Piece]) -> Parse<ParsedDoStatement> {
    const NAMES: &str = "do `declare` statement requires one or more comma-separated local names";
    let fail = |at: usize, message: String| Err(Fail::error(message).or_at(Some(at)));
    let mut declared = Vec::new();
    let names = trim_padding(view, names);
    if names.is_empty() {
        return fail(head, NAMES.to_owned());
    }
    for name in names.split(|piece| is_symbol(view, piece, ",")) {
        let name = trim_padding(view, name);
        let [Piece::Token(index)] = name else {
            return fail(head, NAMES.to_owned());
        };
        let TokenKind::Name(spelling) = token(view, *index).kind() else {
            return fail(head, NAMES.to_owned());
        };
        if *spelling == "_" {
            return fail(
                *index,
                "do `declare` statement cannot use the inaccessible `_` name".to_owned(),
            );
        }
        if let Some(keyword) = canonical_keyword(spelling) {
            return fail(*index, reserved_keyword_message(keyword));
        }
        let local = spelling.starts_with(|character: char| character.is_ascii_alphabetic())
            || spelling.strip_prefix('_').is_some_and(|rest| {
                rest.starts_with(|character: char| character.is_ascii_alphabetic())
            });
        if !local {
            return fail(head, NAMES.to_owned());
        }
        declared.push((*spelling).to_owned());
    }
    Ok(ParsedDoStatement::Declare(declared))
}

/// A clause or statement, split at its binder.
fn binding(
    scope: Scope<'_, '_, '_>,
    pieces: &[Piece],
    context: ExpressionContext,
    site: BindingSite,
) -> Parse<Binding> {
    let view = scope.view;
    let at = pieces.first().map(piece_token);
    let missing = |part: Missing| Err(Fail::error(site.missing(part)).or_at(at));
    if let Some(binder) = pattern_binder(view, pieces) {
        let backward = is_symbol(view, &pieces[binder], "<-");
        let (missing_pattern, missing_right) = if backward {
            (Missing::BackwardPattern, Missing::BackwardOperation)
        } else {
            (Missing::ValuePattern, Missing::Value)
        };
        let left = trim_padding(view, &pieces[..binder]);
        let right = trim_padding(view, &pieces[binder + 1..]);
        if left.is_empty() {
            return missing(missing_pattern);
        }
        let pattern = pattern(scope, left)?;
        if right.is_empty() {
            return missing(missing_right);
        }
        let (right, _) = expression(scope, right, context, false)?;
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
    if pieces
        .first()
        .is_some_and(|piece| is_symbol(view, piece, "->"))
    {
        return missing(Missing::ForwardOperation);
    }
    let (operation, stop) = expression(scope, pieces, context, true)?;
    let Some(arrow) = stop else {
        return Ok(Binding::Operation(operation));
    };
    let rest = trim_padding(view, &pieces[arrow + 1..]);
    if rest.is_empty() {
        return missing(Missing::ForwardPattern);
    }
    Ok(Binding::Effect {
        pattern: pattern(scope, rest)?,
        operation,
    })
}

/// The `<-` or `=` that makes a clause a binding, by position.
fn pattern_binder(view: TokenView<'_, '_>, pieces: &[Piece]) -> Option<usize> {
    for (position, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Form { .. } => return None,
            Piece::Group { .. } => {}
            Piece::Token(index) => match token(view, *index).kind() {
                TokenKind::Symbol("<-" | "=") => return Some(position),
                TokenKind::Symbol("\\") => return None,
                TokenKind::Name(name)
                    if !matches!(*name, "as" | "when")
                        && g0_keyword(name).is_some()
                        && (in_keyword_position(view, *index)
                            || position == 0 && !before_key_mark(view, *index)) =>
                {
                    return None;
                }
                _ => {}
            },
        }
    }
    None
}

/// A pattern in a nested position.
fn pattern(scope: Scope<'_, '_, '_>, pieces: &[Piece]) -> Parse<SyntaxPattern> {
    let view = scope.view;
    let first = piece_token(&pieces[0]);
    let scope = Scope {
        view: pieces_view(view, pieces),
        context: ExpressionContext::for_owner_at(view, first).ending_at_arrow(),
        mode: Mode::Cover,
        ..scope
    };
    Items::new(scope, pieces)
        .pattern_item()?
        .into_pattern(Position::Nested)
        .map_err(|message| Fail::error(message).or_at(Some(first)))
}

/// An expression side, returning the position of the `->` it stopped at
/// when `ends_at_arrow`.
fn expression(
    scope: Scope<'_, '_, '_>,
    pieces: &[Piece],
    context: ExpressionContext,
    ends_at_arrow: bool,
) -> Parse<(SyntaxExpr, Option<usize>)> {
    let view = scope.view;
    let side = pieces_view(view, pieces);
    let context = if ends_at_arrow {
        context.ending_at_arrow()
    } else {
        context.complete()
    };
    if needs_view_dispatch(view, pieces) {
        let parsed = parse_expression_extent(side, context).map_err(Fail::Reported)?;
        let end = parsed.end();
        let expression = parsed
            .into_expression()
            .map_err(|message| Fail::error(message).or_at(Some(side.range().start())))?;
        let stop = (end < side.range().end())
            .then(|| pieces.iter().position(|piece| piece_token(piece) >= end))
            .flatten();
        return Ok((expression, stop));
    }
    let trimmed = trim_layout(side);
    let context = if spans_lines(trimmed) {
        let (context, diagnostics) = validate_expression_floor(trimmed, context);
        if !diagnostics.is_empty() {
            return Err(Fail::Reported(diagnostics));
        }
        context
    } else {
        context.validated()
    };
    let scope = Scope {
        view: side,
        context,
        mode: Mode::Expression,
        ..scope
    };
    let mut items = Items::new(scope, pieces);
    items.stop_at_arrow = ends_at_arrow;
    let (chain, stop) = items.expression()?;
    let expression = chain
        .resolve()
        .map_err(|message| Fail::error(message).or_at(Some(side.range().start())))?;
    Ok((expression, stop))
}

/// Whether pieces hold a form that view-level dispatch parses: `let`,
/// `object`, `where` or `with` at their level, a leading `match` or
/// `try_match`, or one parenthesized group holding such a form.
pub(super) fn needs_view_dispatch(view: TokenView<'_, '_>, pieces: &[Piece]) -> bool {
    let pieces = trim_padding(view, pieces);
    if let [
        Piece::Group {
            open, view_level, ..
        },
    ] = pieces
    {
        return *view_level
            && matches!(
                token(view, *open).kind(),
                TokenKind::Open {
                    delimiter: Delimiter::Parenthesis,
                    ..
                }
            );
    }
    let leading = leads_with(view, pieces, "match") || leads_with(view, pieces, "try_match");
    leading
        || pieces.iter().any(|piece| {
            ["let", "object", "where", "with"]
                .into_iter()
                .any(|name| is_keyword(view, piece, name))
        })
}

/// The view from the first piece to the end of the last.
pub(super) fn pieces_view<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    pieces: &[Piece],
) -> TokenView<'lex, 'source> {
    let start = pieces.first().map_or(view.range().start(), piece_token);
    let end = pieces.last().map_or(start, |piece| match piece {
        Piece::Token(index) => index + 1,
        Piece::Group { close, .. } => close + 1,
        Piece::Form { end, .. } => *end,
    });
    view.subview(TokenRange::new(start, end).expect("pieces are ordered"))
        .expect("pieces lie in their view")
}

/// Whether a view's significant tokens span more than one line.
pub(super) fn spans_lines(view: TokenView<'_, '_>) -> bool {
    let line = |index: usize| {
        view.token_at(index)
            .and_then(|token| view.line_at_span(token.span()))
    };
    match (view.first_significant(), view.last_significant()) {
        (Some((first, _)), Some((last, _))) => line(first) != line(last),
        _ => false,
    }
}

/// Whether pieces begin with the keyword `name`: the first token of a
/// statement or clause is in keyword position, as a view's first is.
fn leads_with(view: TokenView<'_, '_>, pieces: &[Piece], name: &str) -> bool {
    matches!(pieces.first(), Some(Piece::Token(index))
        if matches!(token(view, *index).kind(), TokenKind::Name(found) if *found == name)
            && !before_key_mark(view, *index))
}

fn is_keyword(view: TokenView<'_, '_>, piece: &Piece, name: &str) -> bool {
    matches!(piece, Piece::Token(index)
        if matches!(token(view, *index).kind(), TokenKind::Name(found) if *found == name)
            && in_keyword_position(view, *index))
}

fn is_symbol(view: TokenView<'_, '_>, piece: &Piece, symbol: &str) -> bool {
    matches!(piece, Piece::Token(index)
        if matches!(token(view, *index).kind(), TokenKind::Symbol(found) if *found == symbol))
}
