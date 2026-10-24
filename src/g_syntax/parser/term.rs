//! Prefix-shared term parser for expressions and patterns.
//!
//! It reads every expression and pattern. The round-trip test (`printer`)
//! checks that generated trees print and parse back to themselves.
//!
//! **Each group is parsed once.** One pass over the view's tokens keeps an
//! explicit stack of open delimiter groups. When a group closes, its
//! contents are interpreted once into a role-neutral cover form, and the
//! parent converts that form by role:
//! - a bracket group becomes a list or a key path;
//! - a parenthesized group becomes a grouping, tuple or section, a path
//!   index, or an escape target.
//!
//! No alternative is tried and abandoned. Source nesting therefore costs heap
//! frames, not Rust recursion.
//!
//! **Within a group the contents are flat:** each nested group is one piece.
//! The interpreter is a loop. Lambdas, whose bodies extend to the end of
//! their item, are a stack of pending levels.
//!
//! **Line breaks** follow the grammar's layout rules:
//! - padding inside groups and around separators, lambda parameters and
//!   arrows is skipped;
//! - a line-led atom continues an application;
//! - a line-led infix operator resumes the chain at its indentation, which is
//!   checked against the caller's `ExpressionContext`.
//!
//! **Errors are deferred to use.** A group whose contents are invalid as an
//! expression keeps its error in its cover, and the error surfaces only if a
//! role consumes the cover. A group the pass only keeps, such as one in a
//! `with` body, is never consumed here, so it raises nothing.
//!
//! **Keyword frames**: every keyword form (conditionals, `do` blocks,
//! matches, `let`, `using`, `where`, `with` and objects) is a frame of the
//! token pass (`pass`), interpreted when it closes into a form piece, an atom
//! of the item it stands in. A postfix `if`, on the same line or a
//! continuation line, ends the innermost open lambda's body:
//! `\x -> b if c else d` is `\x -> (b if c else d)`.
//!
//! **Tail position** (Syntax.md): a tail form that a token able to continue
//! it would end on its line, that an argument or path follows, or that
//! stands as an argument, fails its view.
//!
//! **Regions:** a pattern's `when` opens a region to its group member's
//! end, whose groups are deferred, not interpreted: the guard-clause parser
//! reads the guards from their tokens, so a group is interpreted by one
//! parser only.
//!
//! **Cover mode** parses a pattern position. Groups are interpreted into
//! role-neutral cover terms (`cover::Term`) instead of expressions, and the
//! item grammar adds what patterns need: `_`, quoted paths kept as keys,
//! `path?:` and `:_name` dict members, and the pattern operators. Loosest
//! first, these are `when`, one view arrow (`->` or `<-`) and `as`, above
//! the expression grammar, whose precedence is otherwise unchanged. A `when`
//! opens a keyword region, and the guard-clause parser reads its clauses. A
//! term converts by role once the role is known; ordinary expression parses
//! never build terms that are not expressions.

use std::cell::RefCell;

use crate::g_syntax::keywords::{canonical_keyword, g0_keyword, reserved_keyword_message};
use crate::g_syntax::{
    DictMember, PathSuffix, SyntaxExpr, SyntaxKeyExpr, SyntaxOperator, SyntaxPattern,
};

use super::expression::{
    InfixChain, access_if_path, quoted_path, syntax_operator, validate_expr_name,
};
use super::expression_context::ExpressionContext;
use super::input::{TokenRange, TokenView};
use super::lexical::{Delimiter, LeadingTrivia, SpannedToken, TokenKind};
use super::pattern::parse_guard_clauses;
use super::structural::{before_key_mark, in_keyword_position};

pub(super) use self::cover::Position;
use self::cover::Term;
use self::pass::{Form, FormShape, top_pieces};

mod binding;
mod cover;
mod pass;

/// Why the term parser produced no syntax tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Fail {
    /// The view is not a valid expression. `at` is the token where parsing
    /// stopped, when known.
    Error { at: Option<usize>, message: String },
    /// A parse the term parser ran for part of the view, such as a `with`
    /// body's members or a `where`'s bindings, failed with these
    /// diagnostics.
    Reported(Vec<crate::g_syntax::Diagnostic>),
}

impl Fail {
    fn error(message: impl Into<String>) -> Self {
        Self::Error {
            at: None,
            message: message.into(),
        }
    }

    /// Locates an error that has no location yet.
    fn or_at(self, location: Option<usize>) -> Self {
        match self {
            Self::Error { at: None, message } => Self::Error {
                at: location,
                message,
            },
            fail => fail,
        }
    }
}

type Parse<T> = Result<T, Fail>;

fn error<T>(message: impl Into<String>) -> Parse<T> {
    Err(Fail::error(message))
}

/// A term parser error as a diagnostic on the line of its token, or of the
/// view's first token when it has no location.
pub(super) fn diagnostic(
    view: TokenView<'_, '_>,
    at: Option<usize>,
    message: String,
) -> crate::g_syntax::Diagnostic {
    let line = at
        .and_then(|index| view.token_at(index))
        .or_else(|| view.first_significant().map(|(_, token)| token))
        .and_then(|token| view.line_at_span(token.span()))
        .unwrap_or(1);
    crate::g_syntax::Diagnostic::error(line, message)
}

/// Parses `view` as one ordinary expression, returning its unresolved infix
/// chain and the token where it ends, as `parse_expression_chain_view` does.
/// `context` checks the indentation of line-led infix operators, and may end
/// the expression at a top-level `->`.
pub(super) fn parse_term_chain(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Parse<(InfixChain, usize)> {
    let covers = Covers::default();
    let scope = Scope {
        view,
        context,
        covers: &covers,
        mode: Mode::Expression,
    };
    let (pieces, end) = top_pieces(scope)?;
    // Trailing layout ends the expression; leading layout does not start one.
    let pieces = trim_trailing_padding(view, &pieces);
    let mut items = Items::new(scope, pieces);
    items.stop_at_arrow = context.arrow_ends_tail();
    let (chain, stop) = items.expression()?;
    Ok((
        chain,
        stop.map_or(end, |position| piece_token(&pieces[position])),
    ))
}

/// A `let`'s or `where`'s bindings, in source order.
pub(super) type Bindings = Vec<(String, SyntaxExpr)>;

/// Reads the `where` at `where_index` in `view`, which ends the body
/// before it: its bindings, and where they end.
pub(super) fn parse_where_group(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
    where_index: usize,
) -> Result<(Bindings, usize), Vec<crate::g_syntax::Diagnostic>> {
    let covers = Covers::default();
    let scope = Scope {
        view,
        context,
        covers: &covers,
        mode: Mode::Expression,
    };
    pass::where_group(scope, where_index).map_err(|fail| match fail {
        Fail::Error { at, message } => vec![diagnostic(view, at, message)],
        Fail::Reported(diagnostics) => diagnostics,
    })
}

/// Parses `view` as one pattern in `position`, in cover mode.
pub(super) fn parse_pattern_term(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
    position: Position,
) -> Parse<SyntaxPattern> {
    let covers = Covers::default();
    let scope = Scope {
        view,
        context,
        covers: &covers,
        mode: Mode::Cover,
    };
    let (pieces, _) = top_pieces(scope)?;
    let pieces = trim_padding(view, &pieces);
    if pieces.is_empty() {
        return error("expected a pattern");
    }
    Items::new(scope, pieces)
        .pattern_item()?
        .into_pattern(position)
        .map_err(Fail::error)
}

/// Rejects, before any group is interpreted, a token the lexer could not
/// read.
fn prescan(view: TokenView<'_, '_>) -> Parse<()> {
    let range = view.range();
    for (offset, token) in view.tokens().iter().enumerate() {
        let message = match token.kind() {
            TokenKind::InvalidNumber(text) => format!("invalid number literal `{text}`"),
            TokenKind::Unknown(character) => format!("unexpected character `{character}`"),
            _ => continue,
        };
        return Err(Fail::error(message).or_at(Some(range.start() + offset)));
    }
    Ok(())
}

fn token<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    index: usize,
) -> &'lex SpannedToken<'source> {
    view.token_at(index)
        .expect("term indices stay inside the view")
}

/// What every interpretation step shares.
#[derive(Clone, Copy)]
struct Scope<'s, 'lex, 'source> {
    view: TokenView<'lex, 'source>,
    context: ExpressionContext,
    covers: &'s Covers,
    mode: Mode,
}

/// Which grammar a parse reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// An ordinary expression: every term is an expression.
    Expression,
    /// A position that may hold a pattern: terms are role-neutral.
    Cover,
}

/// Whether the token at `index` opens a keyword region: the rest of its
/// group member, up to the group's separator or close. Only a pattern's
/// `when` does, whose guards the guard-clause parser reads from their
/// tokens, so groups closing inside it are deferred rather than
/// interpreted, and every group is interpreted by one parser.
fn opens_keyword_region(view: TokenView<'_, '_>, index: usize, mode: Mode) -> bool {
    regions_enabled() && mode == Mode::Cover && is_pattern_when(view, index)
}

/// The error for a tag prefix with no payload, which only a section,
/// `(path:)`, may stand without.
fn missing_payload(optional: bool) -> &'static str {
    if optional {
        "an optional dictionary entry pattern requires a payload after `?:`"
    } else {
        "a tag requires a payload after `:`; write `(path:)` for its constructor"
    }
}

/// The error for joint tags that end with no payload, as in `a:b:`, which
/// may mean a tag of a constructor, `a:(b:)`, or one path's constructor,
/// `(a.b:)`. The suggestions spell the paths when every key is a name.
fn chain_missing_payload(outer: &[(Vec<SyntaxKeyExpr>, bool)], inner: &[SyntaxKeyExpr]) -> String {
    let spelled = |path: &[SyntaxKeyExpr]| {
        path.iter()
            .map(|key| match key {
                SyntaxKeyExpr::Atom(name) => Some(name.as_str()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .map(|names| names.join("."))
    };
    let paths = outer
        .iter()
        .map(|(path, optional)| (!optional).then(|| spelled(path)).flatten())
        .collect::<Option<Vec<_>>>();
    let (tagged, joined) = match (paths, spelled(inner)) {
        (Some(outer), Some(inner)) => (
            format!("{}:({inner}:)", outer.join(":")),
            format!("({}.{inner}:)", outer.join(".")),
        ),
        _ => ("outer:(inner:)".to_owned(), "(outer.inner:)".to_owned()),
    };
    format!(
        "a tag requires a payload after `:`; write `{tagged}` to tag a constructor, or `{joined}` for one path's constructor"
    )
}

/// The error for a tail form at `head` that `follower` follows, so that it
/// stands outside tail position.
fn misplaced_tail(head: usize, label: &str, follower: &str) -> Fail {
    Fail::error(format!(
        "parenthesize this {label}: {follower} follows it, so it is not in tail position"
    ))
    .or_at(Some(head))
}

/// The error for a tail form at `at` that stands as an application's
/// argument, which is an atom.
fn tail_argument(at: Option<usize>, label: &str) -> Fail {
    Fail::error(format!(
        "parenthesize this {label}, or apply it with `<|`: an application's arguments are atoms"
    ))
    .or_at(at)
}

/// The error for a prefix conditional at `head` that follows an operand,
/// where an `if` reads as postfix and a `try` would be an argument.
fn conditional_argument(head: usize, keyword: &str) -> Fail {
    Fail::error(format!(
        "parenthesize this `{keyword}` expression: a prefix conditional cannot follow an operand"
    ))
    .or_at(Some(head))
}

/// Whether the token at `index` ends an operand: a literal, a special
/// reference such as `self`, or a name that is no keyword where it stands.
fn ends_operand(view: TokenView<'_, '_>, index: usize) -> bool {
    match token(view, index).kind() {
        TokenKind::Name(name) => {
            matches!(*name, "self" | "module" | "module_origin")
                || g0_keyword(name).is_none()
                || !in_keyword_position(view, index)
        }
        TokenKind::Number(_) | TokenKind::Text(_) | TokenKind::Embedded(_) => true,
        _ => false,
    }
}

/// A form's name in errors.
fn form_label(keyword: &str) -> String {
    match keyword {
        "do" => "`do` block".to_string(),
        keyword => format!("`{keyword}` expression"),
    }
}

/// Whether keyword regions are on, as they are outside tests that check
/// regions never change a parse.
fn regions_enabled() -> bool {
    #[cfg(test)]
    if REGIONS_OFF.with(std::cell::Cell::get) {
        return false;
    }
    true
}

/// Whether the token at `index` is a `when` that guards a pattern.
fn is_pattern_when(view: TokenView<'_, '_>, index: usize) -> bool {
    matches!(token(view, index).kind(), TokenKind::Name("when")) && in_keyword_position(view, index)
}

/// Whether the token at `index` heads a keyword form that opens its frame
/// by its keyword alone (`if`, `try`, `match`, `try_match`, `using`, `do`),
/// judged from it and its neighbours. A keyword spelled as a key fails:
/// after a joint `.` (a path segment or effect name), after a joint `'` (a
/// quoted atom or path), after a constructor's `:`, or before a key mark (a
/// tag's key or a path's root).
pub(super) fn may_head_keyword_form(view: TokenView<'_, '_>, index: usize) -> bool {
    let head = token(view, index);
    let TokenKind::Name(name) = head.kind() else {
        return false;
    };
    matches!(*name, "if" | "try" | "match" | "try_match" | "using" | "do")
        && heads_form(view, index)
}

/// Whether the keyword at `index` heads its form where it stands: it is no
/// tag's key or path's root, and follows no key mark.
fn heads_form(view: TokenView<'_, '_>, index: usize) -> bool {
    let joint = |index: usize| {
        view.token_at(index)
            .is_some_and(|token| token.leading() == LeadingTrivia::Joint)
    };
    let symbol = |index: usize, expected: &str| {
        view.token_at(index).is_some_and(
            |token| matches!(token.kind(), TokenKind::Symbol(symbol) if *symbol == expected),
        )
    };
    let after_key_mark = joint(index)
        && index.checked_sub(1).is_some_and(|mark| {
            symbol(mark, ".")
                || symbol(mark, "'")
                || symbol(mark, ":") && is_constructor_colon(view, mark)
        });
    !after_key_mark && !before_key_mark(view, index)
}

/// Whether the `:` at `index` starts a constructor (`:path`) rather than
/// ending a tag's key (`a:`, `[k]:`, `(p):`): it follows a space, a line
/// break, an opening delimiter, a symbol, or nothing.
fn is_constructor_colon(view: TokenView<'_, '_>, index: usize) -> bool {
    let colon = token(view, index);
    colon.leading() != LeadingTrivia::Joint
        || index
            .checked_sub(1)
            .and_then(|before| view.token_at(before))
            .is_none_or(|before| {
                matches!(
                    before.kind(),
                    TokenKind::Open { .. } | TokenKind::Symbol(_) | TokenKind::LineStart { .. }
                )
            })
}

/// One element of a group's flat contents: a token, a nested group already
/// interpreted into its cover form, or a conditional already interpreted
/// into its form; covers and forms are held in the arena.
enum Piece {
    Token(usize),
    Group {
        open: usize,
        close: usize,
        cover: usize,
    },
    /// A conditional from its head to `end`, exclusive.
    Form {
        head: usize,
        end: usize,
        form: usize,
    },
}

/// Every group's cover and every conditional's form. A cover is inspected
/// freely but taken exactly once, by the role that consumes it, so no
/// subtree is copied; so is a form.
#[derive(Default)]
struct Covers {
    slots: RefCell<Vec<Slot>>,
    forms: RefCell<Vec<Option<Form>>>,
}

enum Slot {
    Ready(Cover),
    /// A group in a keyword region or a `with` body, which another parse
    /// reads from its tokens. A role that consumes it anyway interprets it
    /// then.
    Deferred(Deferred),
    Taken,
}

/// What interpreting a closed group needs of its frame.
struct GroupBody {
    delimiter: Delimiter,
    pieces: Vec<Piece>,
    mode: Mode,
    context: ExpressionContext,
}

struct Deferred {
    body: GroupBody,
    open: usize,
    close: usize,
    /// Whether the group lies in a keyword region of its own parent, rather
    /// than only inside a deferred parent; interpreting the parent then
    /// leaves it deferred.
    in_parent_region: bool,
}

impl Covers {
    fn form(&self, form: Form) -> usize {
        let mut forms = self.forms.borrow_mut();
        forms.push(Some(form));
        forms.len() - 1
    }

    /// A form's shape and keyword, without taking it.
    fn form_shape(&self, id: usize) -> (FormShape, &'static str) {
        let forms = self.forms.borrow();
        let form = forms[id]
            .as_ref()
            .expect("a form is inspected before it is consumed");
        (form.shape, form.keyword)
    }

    /// Whether a form's last part ended at its own `}` or a dedent.
    fn form_ends_with_block(&self, id: usize) -> bool {
        self.forms.borrow()[id]
            .as_ref()
            .is_some_and(|form| form.ends_with_block)
    }

    fn take_form(&self, id: usize) -> Form {
        self.forms.borrow_mut()[id]
            .take()
            .expect("a form is consumed exactly once")
    }

    fn ready(&self, cover: Cover) -> usize {
        let mut slots = self.slots.borrow_mut();
        slots.push(Slot::Ready(cover));
        slots.len() - 1
    }

    fn defer(&self, deferred: Deferred) -> usize {
        let mut slots = self.slots.borrow_mut();
        slots.push(Slot::Deferred(deferred));
        slots.len() - 1
    }

    fn take(&self, scope: Scope<'_, '_, '_>, id: usize) -> Cover {
        self.interpret_deferred(scope, id);
        match std::mem::replace(&mut self.slots.borrow_mut()[id], Slot::Taken) {
            Slot::Ready(cover) => cover,
            Slot::Deferred(_) | Slot::Taken => unreachable!("a cover is consumed exactly once"),
        }
    }

    fn shape(&self, scope: Scope<'_, '_, '_>, id: usize) -> Shape {
        self.interpret_deferred(scope, id);
        match &self.slots.borrow()[id] {
            Slot::Ready(Cover::Bracket(items)) => Shape::Bracket {
                empty: items.is_empty(),
            },
            Slot::Ready(Cover::Paren(ParenCover::Grouped(_))) => Shape::Grouped,
            Slot::Ready(Cover::Paren(ParenCover::Unit)) => Shape::Unit,
            Slot::Ready(Cover::Paren(_) | Cover::Brace(_) | Cover::Invalid(_)) => Shape::Other,
            Slot::Deferred(_) | Slot::Taken => {
                unreachable!("a cover is inspected before it is consumed")
            }
        }
    }

    /// Interprets a deferred group as the pass would have: its deferred
    /// descendants first, except those in a keyword region of their own
    /// parent. Post-order on an explicit stack, so nesting costs no Rust
    /// recursion.
    fn interpret_deferred(&self, scope: Scope<'_, '_, '_>, id: usize) {
        if !matches!(self.slots.borrow()[id], Slot::Deferred(_)) {
            return;
        }
        let mut stack = vec![(id, false)];
        while let Some((slot, children_done)) = stack.pop() {
            if !children_done {
                let slots = self.slots.borrow();
                let Slot::Deferred(deferred) = &slots[slot] else {
                    continue;
                };
                stack.push((slot, true));
                stack.extend(deferred.body.pieces.iter().filter_map(|piece| {
                    match piece {
                        Piece::Group { cover, .. } => matches!(
                            &slots[*cover],
                            Slot::Deferred(child) if !child.in_parent_region
                        )
                        .then_some((*cover, false)),
                        Piece::Token(_) | Piece::Form { .. } => None,
                    }
                }));
                continue;
            }
            let Slot::Deferred(deferred) =
                std::mem::replace(&mut self.slots.borrow_mut()[slot], Slot::Taken)
            else {
                unreachable!("a deferred group is interpreted once");
            };
            // A deferred group reads in its own frame's mode and context.
            let scope = Scope {
                mode: deferred.body.mode,
                context: deferred.body.context,
                ..scope
            };
            let cover = interpret_group(scope, &deferred.body, deferred.close)
                .unwrap_or_else(|fail| Cover::Invalid(fail.or_at(Some(deferred.open))));
            self.slots.borrow_mut()[slot] = Slot::Ready(cover);
        }
    }
}

/// What a cover can become without consuming it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Bracket {
        empty: bool,
    },
    Grouped,
    /// `()`, which cannot be a path splice.
    Unit,
    Other,
}

/// A group's contents, interpreted once and converted by the parent's role.
/// In expression mode every term in it is an expression.
enum Cover {
    Paren(ParenCover),
    /// A list, or a key path whose items convert by `SyntaxKeyExpr::from_item`.
    Bracket(Vec<Term>),
    Brace(Vec<Term>),
    /// Contents that are not a valid group in the parse's mode; the failure
    /// surfaces only if a role consumes the group.
    Invalid(Fail),
}

enum ParenCover {
    Unit,
    Grouped(Term),
    Tuple(Vec<Term>),
    Section(SyntaxExpr),
}

/// The token that starts a piece: the token itself, a group's open, or a
/// form's head.
fn piece_token(piece: &Piece) -> usize {
    match piece {
        Piece::Token(index) => *index,
        Piece::Group { open, .. } => *open,
        Piece::Form { head, .. } => *head,
    }
}

fn piece_leading(view: TokenView<'_, '_>, piece: &Piece) -> LeadingTrivia {
    token(view, piece_token(piece)).leading()
}

fn piece_kind<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    piece: &Piece,
) -> Option<&'lex TokenKind<'source>> {
    match piece {
        Piece::Token(index) => Some(token(view, *index).kind()),
        Piece::Group { .. } | Piece::Form { .. } => None,
    }
}

fn is_symbol(view: TokenView<'_, '_>, piece: &Piece, symbol: &str) -> bool {
    matches!(piece_kind(view, piece), Some(TokenKind::Symbol(found)) if *found == symbol)
}

fn is_line_start(view: TokenView<'_, '_>, piece: &Piece) -> bool {
    matches!(piece_kind(view, piece), Some(TokenKind::LineStart { .. }))
}

fn piece_operator(view: TokenView<'_, '_>, piece: &Piece) -> Option<SyntaxOperator> {
    match piece {
        Piece::Token(index) => syntax_operator(token(view, *index)),
        Piece::Group { .. } | Piece::Form { .. } => None,
    }
}

/// Splits a group's pieces at its top-level commas and trims line-start
/// padding from each segment. Every piece is already top-level: nested groups
/// are single pieces.
fn segments<'a>(view: TokenView<'_, '_>, pieces: &'a [Piece]) -> Vec<&'a [Piece]> {
    pieces
        .split(|piece| is_symbol(view, piece, ","))
        .map(|segment| trim_padding(view, segment))
        .collect()
}

fn trim_padding<'a>(view: TokenView<'_, '_>, mut pieces: &'a [Piece]) -> &'a [Piece] {
    while let Some((first, rest)) = pieces.split_first()
        && is_line_start(view, first)
    {
        pieces = rest;
    }
    trim_trailing_padding(view, pieces)
}

fn trim_trailing_padding<'a>(view: TokenView<'_, '_>, mut pieces: &'a [Piece]) -> &'a [Piece] {
    while let Some((last, rest)) = pieces.split_last()
        && is_line_start(view, last)
    {
        pieces = rest;
    }
    pieces
}

/// The non-empty items of a separated sequence, accepting exactly what
/// Chumsky's `separated_by` accepts: at most one leading separator when
/// `allow_leading`, and one trailing separator, after which no item follows,
/// when `allow_trailing`.
fn separated_items<'a>(
    segments: &[&'a [Piece]],
    allow_leading: bool,
    allow_trailing: bool,
) -> Parse<Vec<&'a [Piece]>> {
    let mut items = Vec::new();
    let mut position = 0;
    let mut consumed_separator = false;
    if allow_leading && segments.len() > 1 && segments[0].is_empty() {
        position = 1;
        consumed_separator = true;
    }
    while let Some(segment) = segments.get(position) {
        if segment.is_empty() {
            let is_last = position + 1 == segments.len();
            if is_last && (items.is_empty() && !consumed_separator || allow_trailing) {
                break;
            }
            return error("expected an item between separators");
        }
        items.push(*segment);
        position += 1;
        // Another segment means another separator was consumed.
        consumed_separator = true;
    }
    Ok(items)
}

#[cfg(test)]
thread_local! {
    /// Groups this thread's term parsers have interpreted, for tests that
    /// check no group is interpreted twice.
    static INTERPRETED_GROUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static REGIONS_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `work` with no keyword regions on this thread, for tests that check
/// regions never change a parse.
#[cfg(test)]
fn without_regions<R>(work: impl FnOnce() -> R) -> R {
    let was = REGIONS_OFF.with(|off| off.replace(true));
    let result = work();
    REGIONS_OFF.with(|off| off.set(was));
    result
}

fn interpret_group(scope: Scope<'_, '_, '_>, frame: &GroupBody, close: usize) -> Parse<Cover> {
    #[cfg(test)]
    INTERPRETED_GROUPS.with(|count| count.set(count.get() + 1));
    let view = scope.view;
    let delimiter = &frame.delimiter;
    match *delimiter {
        Delimiter::Parenthesis => interpret_paren(scope, frame, close).map(Cover::Paren),
        Delimiter::Bracket => {
            let segments = segments(view, &frame.pieces);
            let items = separated_items(&segments, true, true)?;
            resolved_items(scope, items).map(Cover::Bracket)
        }
        Delimiter::Brace => {
            let segments = segments(view, &frame.pieces);
            let items = separated_items(&segments, true, true)?;
            items
                .into_iter()
                .map(|item| dict_item(scope, item))
                .collect::<Parse<Vec<_>>>()
                .map(Cover::Brace)
        }
    }
}

fn interpret_paren(scope: Scope<'_, '_, '_>, frame: &GroupBody, close: usize) -> Parse<ParenCover> {
    let view = scope.view;
    // A parenthesized expression is a body, which a `where` may end; a
    // tuple's item is none.
    let where_head = frame.pieces.iter().find_map(|piece| match piece {
        Piece::Form { head, form, .. } if scope.covers.form_shape(*form).0 == FormShape::Where => {
            Some(*head)
        }
        _ => None,
    });
    if let Some(head) = where_head
        && frame.pieces.iter().any(|piece| is_symbol(view, piece, ","))
    {
        return Err(Fail::error(
            "`where` cannot stand in a tuple item: it scopes a whole definition, statement or arm; parenthesize `(… where …)` or use `let … in` for part of an expression",
        )
        .or_at(Some(head)));
    }
    let (contents, suffix) = binding::split_where(scope, &frame.pieces)?;
    if let Some(binding::WhereSuffix { head, form }) = suffix {
        if contents.is_empty() {
            return Err(Fail::error("where expression requires a body").or_at(Some(head)));
        }
        let expression = resolved(scope, contents)?;
        return binding::with_where(scope, form, expression)
            .map(|expression| ParenCover::Grouped(Term::Expr(expression)));
    }
    let contents = trim_padding(view, contents);
    if contents.is_empty() {
        return if frame.pieces.is_empty() && token(view, close).leading() == LeadingTrivia::Joint {
            Ok(ParenCover::Unit)
        } else {
            error("expected an expression inside parentheses")
        };
    }
    let segments = segments(view, &frame.pieces);
    if is_symbol(view, &contents[0], ",") {
        // A leading tuple: `(, a, b)`. Its items follow the explicit comma.
        let items = separated_items(&segments[1..], false, true)?;
        return resolved_items(scope, items).map(ParenCover::Tuple);
    }
    // A joint `:` after a name always makes a tag, so `(and:a)` is a grouped
    // tag rather than a section of `and`.
    if let Some(operator) = piece_operator(view, &contents[0])
        && !Items::new(scope, contents).named_tag_at(0)
    {
        if segments.len() > 1 {
            return error("an operator section takes one operand");
        }
        let operand = trim_padding(view, &contents[1..]);
        if operand.is_empty() {
            return Ok(ParenCover::Section(SyntaxExpr::OperatorSection {
                operator,
                left: None,
                right: None,
            }));
        }
        let right = resolved(scope, operand)?;
        return Ok(ParenCover::Section(SyntaxExpr::OperatorSection {
            operator,
            left: None,
            right: Some(Box::new(right)),
        }));
    }
    if segments.len() == 1 {
        if let Some(path) = Items::new(scope, contents).tag_section() {
            return path.map(|path| ParenCover::Section(SyntaxExpr::TaggedConstructor(path)));
        }
        let (last, before) = contents
            .split_last()
            .expect("non-empty contents have a last piece");
        if let Some(operator) = piece_operator(view, last)
            && !absorbs_operator_name(view, before, last)
        {
            let left = resolved(scope, trim_padding(view, before))?;
            return Ok(ParenCover::Section(SyntaxExpr::OperatorSection {
                operator,
                left: Some(Box::new(left)),
                right: None,
            }));
        }
        return resolved_term(scope, contents).map(ParenCover::Grouped);
    }
    // A trailing tuple: `(a, b)`, `(a,)`.
    if segments[0].is_empty() {
        return error("expected an expression before `,`");
    }
    let mut items = vec![segments[0]];
    items.extend(separated_items(&segments[1..], false, true)?);
    resolved_items(scope, items).map(ParenCover::Tuple)
}

/// Whether a trailing operator name belongs to the expression before it:
/// `and` or `or` joint after `.`, `'` or `:`, as in `x.and`, `'and`, `:and`.
fn absorbs_operator_name(view: TokenView<'_, '_>, before: &[Piece], last: &Piece) -> bool {
    matches!(piece_kind(view, last), Some(TokenKind::Name(_)))
        && piece_leading(view, last) == LeadingTrivia::Joint
        && before.last().is_some_and(|piece| {
            [".", "'", ":"]
                .into_iter()
                .any(|symbol| is_symbol(view, piece, symbol))
        })
}

fn resolved_items(scope: Scope<'_, '_, '_>, items: Vec<&[Piece]>) -> Parse<Vec<Term>> {
    items
        .into_iter()
        .map(|item| resolved_term(scope, item))
        .collect()
}

/// One group member: an expression, or in cover mode a pattern item.
fn resolved_term(scope: Scope<'_, '_, '_>, pieces: &[Piece]) -> Parse<Term> {
    match scope.mode {
        Mode::Expression => Items::new(scope, pieces)
            .item()?
            .resolve()
            .map(Term::Expr)
            .map_err(Fail::error),
        Mode::Cover => Items::new(scope, pieces).pattern_item(),
    }
}

/// One group member that only an expression may fill.
fn resolved(scope: Scope<'_, '_, '_>, pieces: &[Piece]) -> Parse<SyntaxExpr> {
    resolved_term(scope, pieces)?
        .into_expr()
        .map_err(|message| Fail::error(message).or_at(pieces.first().map(piece_token)))
}

/// A dict literal's item: an ordinary expression, or in cover mode a pattern
/// item; the literal is the union of its items.
///
/// Transitional (`tags-prefix-migration`): an item that begins with a joint
/// tag yet is no tag, such as `{a:f x}`, read as `a:(f x)` before tags
/// became prefixes, and is reported until the corpus has moved.
fn dict_item(scope: Scope<'_, '_, '_>, item: &[Piece]) -> Parse<Term> {
    let joint = Items::new(scope, item)
        .tag_prefix_at(0)
        .is_some_and(|prefix| prefix.payload == TagPayload::Joint);
    // A parenthesized item is opaque to the literal: a dict member even
    // where it holds a tag (`dict-path-fusion`). An expression's groups
    // resolve away, so the literal keeps this one.
    let grouped = matches!(item, [Piece::Group { cover, .. }]
        if matches!(scope.covers.shape(scope, *cover), Shape::Grouped));
    let term = resolved_term(scope, item)?;
    let term = if grouped && !matches!(term, Term::Group(_)) {
        Term::Group(Box::new(term))
    } else {
        term
    };
    let tag = matches!(
        term,
        Term::Tag(..) | Term::OptionalTag(..) | Term::Expr(SyntaxExpr::PathDict(..))
    ) || matches!(&term, Term::Expr(SyntaxExpr::DictLiteral(members))
        if matches!(members.as_slice(), [DictMember::Optional(..)]));
    if joint && !tag {
        return Err(Fail::error(JOINT_ITEM).or_at(item.first().map(piece_token)));
    }
    Ok(term)
}

fn is_glam_name(name: &str) -> bool {
    name.starts_with(|character: char| character.is_ascii_alphabetic())
}

/// The flat interpreter over one item's pieces.
struct Items<'p, 'lex, 'source> {
    scope: Scope<'p, 'lex, 'source>,
    pieces: &'p [Piece],
    position: usize,
    /// Whether the item ends at a `->` after an operand: an expression whose
    /// tails end at `->`, after a keyword form that ended there.
    stop_at_arrow: bool,
}

/// One level of an item: the item's own, or an open lambda or spaced tag,
/// whose body runs to the end of the item.
struct Level {
    chain: Chain,
    kind: LevelKind,
}

enum LevelKind {
    Item,
    Lambda(Vec<String>),
    /// A spaced tag prefix, `path: payload`, or optional, `path?: payload`.
    Tag(Vec<SyntaxKeyExpr>, bool),
}

/// A tag in an expression: `path:payload`, a path dict, or optional,
/// `path?:payload`, a literal of its one optional member
/// (`dict-path-fusion`).
fn tag_expr(path: Vec<SyntaxKeyExpr>, optional: bool, payload: SyntaxExpr) -> SyntaxExpr {
    if optional {
        SyntaxExpr::DictLiteral(vec![DictMember::Optional(path, payload)])
    } else {
        SyntaxExpr::PathDict(path, Box::new(payload))
    }
}

/// What follows a tag prefix's `:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagPayload {
    /// A payload joint to the `:`, which binds tighter than juxtaposition.
    Joint,
    /// A space or line break, then the rest of the item.
    Spaced,
    /// Nothing in the item: the tag's constructor.
    Bare,
}

/// A tag prefix, `path:`, or in a pattern the optional `path?:`.
#[derive(Debug, Clone, Copy)]
struct TagPrefix {
    optional: bool,
    payload: TagPayload,
}

/// A joint brace item that changed meaning with `tags-prefix-operator`.
const JOINT_ITEM: &str = "a joint tag `path:Atom` now tags only its atom, in braces as elsewhere: write `path: …` with a space to tag the rest of the item, or parenthesize `(path:Atom) …`";

#[derive(Default)]
struct Chain {
    first: Option<Term>,
    rest: Vec<(SyntaxOperator, Term)>,
    pending: Option<SyntaxOperator>,
    /// The indentation and token of each line-led operator, in order.
    anchors: Vec<(usize, usize)>,
}

impl Chain {
    fn push_operand(&mut self, operand: Term) {
        match self.pending.take() {
            Some(operator) => self.rest.push((operator, operand)),
            None => {
                debug_assert!(self.first.is_none(), "operands alternate with operators");
                self.first = Some(operand);
            }
        }
    }

    fn operands(&self) -> Parse<()> {
        if self.pending.is_some() {
            return error("an infix operator requires a right operand");
        }
        if self.first.is_none() {
            return error("expected an expression");
        }
        Ok(())
    }

    /// The chain as an expression chain, its operands converted.
    fn finish(self, context: ExpressionContext) -> Parse<InfixChain> {
        self.operands()?;
        let first = self
            .first
            .expect("checked")
            .into_expr()
            .map_err(Fail::error)?;
        let rest = self
            .rest
            .into_iter()
            .map(|(operator, operand)| operand.into_expr().map(|operand| (operator, operand)))
            .collect::<Result<_, _>>()
            .map_err(Fail::error)?;
        self.anchors.into_iter().try_fold(
            InfixChain::from_parts(first, rest),
            |chain, (indentation, operator)| {
                chain
                    .with_resumption_anchor(indentation, context)
                    .map_err(|message| Fail::Error {
                        at: Some(operator),
                        message,
                    })
            },
        )
    }

    /// The chain as a cover term, resolved only once its role is known.
    fn finish_term(self, context: ExpressionContext) -> Parse<Term> {
        self.operands()?;
        self.anchors
            .into_iter()
            .try_fold(None, |anchor, (indentation, operator)| {
                InfixChain::check_resumption_anchor(anchor, indentation, context)
                    .map(Some)
                    .map_err(|message| Fail::Error {
                        at: Some(operator),
                        message,
                    })
            })?;
        let first = self.first.expect("checked");
        Ok(if self.rest.is_empty() {
            first
        } else {
            Term::Chain(Box::new(first), self.rest)
        })
    }
}

/// An operator of the pattern grammar, above the expression grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatternOperator {
    As,
    /// `View -> Pattern`.
    Forward,
    /// `Pattern <- View`.
    Backward,
    When,
}

impl<'p, 'lex, 'source> Items<'p, 'lex, 'source> {
    fn new(scope: Scope<'p, 'lex, 'source>, pieces: &'p [Piece]) -> Self {
        Self {
            scope,
            pieces,
            position: 0,
            stop_at_arrow: false,
        }
    }

    /// A cursor over the same pieces at `position`, for measuring by shape.
    fn at(&self, position: usize) -> Self {
        Self::new(self.scope, self.pieces).moved_to(position)
    }

    fn moved_to(mut self, position: usize) -> Self {
        self.position = position;
        self
    }

    fn shape_at(&self, offset: usize) -> Option<Shape> {
        match self.peek_at(offset) {
            Some(Piece::Group { cover, .. }) => Some(self.scope.covers.shape(self.scope, *cover)),
            _ => None,
        }
    }

    /// Takes the cover at the cursor and advances past it, surfacing a
    /// deferred error.
    fn take_group(&mut self) -> Parse<Cover> {
        let Some(Piece::Group { cover, .. }) = self.peek() else {
            unreachable!("take_group is called on a group");
        };
        self.position += 1;
        match self.scope.covers.take(self.scope, *cover) {
            Cover::Invalid(fail) => Err(fail),
            cover => Ok(cover),
        }
    }

    /// Takes a grouped cover as an expression: a path index or an escape
    /// target.
    fn take_grouped(&mut self) -> Parse<SyntaxExpr> {
        let at = self.here();
        match self.take_group() {
            Ok(Cover::Paren(ParenCover::Grouped(term))) => term
                .into_expr()
                .map_err(|message| Fail::error(message).or_at(at)),
            _ => unreachable!("the shape was checked as grouped"),
        }
    }

    fn take_keys(&mut self) -> Parse<Vec<SyntaxKeyExpr>> {
        let at = self.here();
        match self.take_group() {
            Ok(Cover::Bracket(items)) => items
                .into_iter()
                .map(|item| item.into_expr().map(SyntaxKeyExpr::from_item))
                .collect::<Result<_, _>>()
                .map_err(|message| Fail::error(message).or_at(at)),
            _ => unreachable!("the shape was checked as a bracket"),
        }
    }

    /// Where a data path starting at the cursor would end, measured by
    /// shape without consuming anything.
    fn data_path_end(&self) -> Option<usize> {
        let mut position = self.position;
        match self.peek() {
            Some(Piece::Group { cover, .. }) => match self.scope.covers.shape(self.scope, *cover) {
                Shape::Bracket { empty: false } | Shape::Grouped => return Some(position + 1),
                _ => return None,
            },
            Some(piece @ Piece::Token(_)) => match piece_kind(self.scope.view, piece) {
                Some(TokenKind::Name(name)) if is_glam_name(name) => position += 1,
                _ => return None,
            },
            Some(Piece::Form { .. }) | None => return None,
        }
        let mut cursor = self.at(position);
        while let Some(width) = cursor.path_suffix_width() {
            cursor.position += width;
        }
        position = cursor.position;
        Some(position)
    }

    /// The width of a joint path suffix at the cursor, by shape.
    fn path_suffix_width(&self) -> Option<usize> {
        if !(self.symbol_at(0, ".") && self.joint_at(0) && self.joint_at(1)) {
            return None;
        }
        let accepted = match self.peek_at(1) {
            Some(Piece::Group { cover, .. }) => matches!(
                self.scope.covers.shape(self.scope, *cover),
                Shape::Bracket { .. } | Shape::Grouped
            ),
            Some(piece @ Piece::Token(_)) => matches!(
                piece_kind(self.scope.view, piece),
                Some(TokenKind::Name(name)) if is_glam_name(name)
            ),
            Some(Piece::Form { .. }) | None => false,
        };
        accepted.then_some(2)
    }

    fn peek(&self) -> Option<&'p Piece> {
        self.pieces.get(self.position)
    }

    fn peek_at(&self, offset: usize) -> Option<&'p Piece> {
        self.pieces.get(self.position + offset)
    }

    fn kind(&self) -> Option<&'lex TokenKind<'source>> {
        self.peek()
            .and_then(|piece| piece_kind(self.scope.view, piece))
    }

    fn leading_at(&self, offset: usize) -> Option<LeadingTrivia> {
        self.peek_at(offset)
            .map(|piece| piece_leading(self.scope.view, piece))
    }

    fn joint_at(&self, offset: usize) -> bool {
        self.leading_at(offset) == Some(LeadingTrivia::Joint)
    }

    fn symbol_at(&self, offset: usize, symbol: &str) -> bool {
        self.peek_at(offset)
            .is_some_and(|piece| is_symbol(self.scope.view, piece, symbol))
    }

    /// How many line starts follow `offset`, and the indentation of the
    /// last one.
    fn line_starts_at(&self, offset: usize) -> (usize, Option<usize>) {
        let mut count = 0;
        let mut indentation = None;
        while let Some(TokenKind::LineStart { indentation: found }) = self
            .peek_at(offset + count)
            .and_then(|piece| piece_kind(self.scope.view, piece))
        {
            indentation = Some(*found);
            count += 1;
        }
        (count, indentation)
    }

    fn skip_line_starts(&mut self) {
        self.position += self.line_starts_at(0).0;
    }

    /// The token an error at the cursor is reported at: the current piece,
    /// or the item's last piece at its end.
    fn here(&self) -> Option<usize> {
        self.peek().or_else(|| self.pieces.last()).map(piece_token)
    }

    fn error_here<T>(&self, message: impl Into<String>) -> Parse<T> {
        Err(Fail::error(message).or_at(self.here()))
    }

    /// Reports what the cursor expected and what it found instead.
    fn expected<T>(&self, expected: &str) -> Parse<T> {
        let found = match self.peek() {
            Some(piece) => {
                let view = self.scope.view;
                view.display(view.token_at(piece_token(piece))).to_string()
            }
            None => "the end of the expression".to_owned(),
        };
        self.error_here(format!("expected {expected}, found {found}"))
    }

    /// The pattern operator at `offset`, in cover mode.
    fn pattern_operator_at(&self, offset: usize) -> Option<PatternOperator> {
        if self.scope.mode != Mode::Cover {
            return None;
        }
        let Some(Piece::Token(index)) = self.peek_at(offset) else {
            return None;
        };
        let view = self.scope.view;
        match token(view, *index).kind() {
            TokenKind::Symbol("->") => Some(PatternOperator::Forward),
            TokenKind::Symbol("<-") => Some(PatternOperator::Backward),
            TokenKind::Name("as") if in_keyword_position(view, *index) => Some(PatternOperator::As),
            TokenKind::Name("when") if is_pattern_when(view, *index) => Some(PatternOperator::When),
            _ => None,
        }
    }

    /// Parses the whole item as an expression: an infix chain of
    /// applications, where a lambda takes the rest of the item as its body.
    fn item(self) -> Parse<InfixChain> {
        self.expression().map(|(chain, _)| chain)
    }

    /// Parses the item as an expression, returning the position of the `->`
    /// it stopped at, if any.
    fn expression(mut self) -> Parse<(InfixChain, Option<usize>)> {
        let chain = self.chain()?;
        let stop = self.peek().is_some().then_some(self.position);
        debug_assert!(
            stop.is_none() || self.stop_at_arrow || self.where_at(0),
            "an expression chain ends its item"
        );
        Ok((self.infix_chain(chain)?, stop))
    }

    /// Whether the item ends at the piece at `offset`: a pattern operator in
    /// cover mode, a `->` ending an open tail, or a `where`, which ends the
    /// definition, statement or arm the item stands in.
    fn stops_at(&self, offset: usize) -> bool {
        self.pattern_operator_at(offset).is_some()
            || self.stop_at_arrow && self.symbol_at(offset, "->")
            || self.where_at(offset)
    }

    /// Whether the piece at `offset` is an update's body.
    fn with_at(&self, offset: usize) -> bool {
        matches!(self.peek_at(offset), Some(Piece::Form { form, .. })
            if self.scope.covers.form_shape(*form).0 == FormShape::With)
    }

    /// An update's body (`FormShape::With`), whose base is the chain before
    /// it at its level. The update is the operand that what follows
    /// continues.
    fn with_update(&mut self, levels: &mut [Level]) -> Parse<()> {
        let Some(Piece::Form { form, .. }) = self.peek() else {
            unreachable!("with_update is called on an update's form");
        };
        let form = self.scope.covers.take_form(*form);
        self.position += 1;
        let level = levels.last_mut().expect("an item always has a level");
        let chain = std::mem::take(&mut level.chain);
        let anchors = chain.anchors.clone();
        let base = chain
            .finish(self.scope.context)
            .and_then(|chain| chain.resolve().map_err(Fail::error))
            .map_err(|fail| fail.or_at(Some(form.head)))?;
        let SyntaxExpr::With { alias, body, .. } = form.result? else {
            unreachable!("an update's form holds an update");
        };
        level.chain = Chain {
            anchors,
            ..Chain::default()
        };
        level.chain.push_operand(Term::Expr(SyntaxExpr::With {
            base: Box::new(base),
            alias,
            body,
        }));
        Ok(())
    }

    /// Whether the piece at `offset` is a `where` that the body around the
    /// expression takes; a pattern has none.
    fn where_at(&self, offset: usize) -> bool {
        self.scope.mode == Mode::Expression
            && match self.peek_at(offset) {
                Some(Piece::Token(index)) => {
                    matches!(
                        token(self.scope.view, *index).kind(),
                        TokenKind::Name("where")
                    ) && in_keyword_position(self.scope.view, *index)
                }
                Some(Piece::Form { form, .. }) => {
                    self.scope.covers.form_shape(*form).0 == FormShape::Where
                }
                _ => false,
            }
    }

    /// A pattern position's item, in cover mode: an `as` chain, at most one
    /// view arrow, and a `when` guard, each looser than the last.
    fn pattern_item(mut self) -> Parse<Term> {
        const FORWARD: &str = "view pattern requires a view before `->` and a pattern after it";
        const BACKWARD: &str = "view pattern requires a pattern before `<-` and a view after it";
        let at = self.here();
        match self.pattern_operator_at(0) {
            Some(PatternOperator::Forward) => return self.error_here(FORWARD),
            Some(PatternOperator::Backward) => return self.error_here(BACKWARD),
            _ => {}
        }
        let mut term = self.as_chain()?;
        match self.pattern_operator_at(0) {
            Some(PatternOperator::Forward) => {
                self.position += 1;
                self.skip_line_starts();
                if self.peek().is_none() {
                    return self.error_here(FORWARD);
                }
                let view = term
                    .into_expr()
                    .map_err(|message| Fail::error(message).or_at(at))?;
                let pattern = self.as_chain()?;
                term = Term::View {
                    view: Box::new(view),
                    pattern: Box::new(pattern),
                };
            }
            Some(PatternOperator::Backward) => {
                self.position += 1;
                self.skip_line_starts();
                if self.peek().is_none() {
                    return self.error_here(BACKWARD);
                }
                let at = self.here();
                let view = self
                    .operand_term()?
                    .into_expr()
                    .map_err(|message| Fail::error(message).or_at(at))?;
                term = Term::View {
                    view: Box::new(view),
                    pattern: Box::new(term),
                };
            }
            _ => {}
        }
        match self.pattern_operator_at(0) {
            None => Ok(term),
            Some(PatternOperator::When) => Ok(Term::Guarded {
                pattern: Box::new(term),
                guards: self.guards()?,
            }),
            Some(PatternOperator::Forward | PatternOperator::Backward) => self
                .error_here("a pattern takes one view arrow; parenthesize a view inside another"),
            Some(PatternOperator::As) => {
                self.error_here("`as` cannot follow a view's expression; parenthesize the view")
            }
        }
    }

    /// Operand chains joined by `as`, which is left-associative.
    fn as_chain(&mut self) -> Parse<Term> {
        let mut term = self.operand_term()?;
        while self.pattern_operator_at(0) == Some(PatternOperator::As) {
            self.position += 1;
            self.skip_line_starts();
            let right = self.operand_term()?;
            term = Term::As(Box::new(term), Box::new(right));
        }
        Ok(term)
    }

    /// One operand chain, up to the item's end or a pattern operator.
    fn operand_term(&mut self) -> Parse<Term> {
        if self.peek().is_none() || self.pattern_operator_at(0).is_some() {
            return self.expected("a pattern");
        }
        let chain = self.chain()?;
        chain
            .finish_term(self.scope.context)
            .map_err(|fail| fail.or_at(self.here()))
    }

    /// A pattern's `when` guard: the clauses after it, to the item's end,
    /// parsed from their tokens, since `when` opened a keyword region.
    fn guards(&mut self) -> Parse<Vec<crate::g_syntax::SyntaxGuardClause>> {
        let when = self.here().expect("a `when` is a piece");
        let end = match self.pieces.last().expect("the item holds its `when`") {
            Piece::Token(index) => index + 1,
            Piece::Group { close, .. } => close + 1,
            Piece::Form { end, .. } => *end,
        };
        self.position = self.pieces.len();
        if end == when + 1 {
            return Err(
                Fail::error("local pattern guard requires a guard after `when`").or_at(Some(when)),
            );
        }
        let guards = self
            .scope
            .view
            .subview(TokenRange::new(when + 1, end).expect("a guard follows its `when`"))
            .expect("a pattern's guard lies in its view");
        parse_guard_clauses(guards, "local pattern guard").map_err(Fail::Reported)
    }

    /// Parses operands and infix operators up to the item's end, or in
    /// cover mode up to a pattern operator. A lambda takes the rest as its
    /// body. Returns the item level's chain, its lambdas closed, and how it
    /// ends.
    fn chain(&mut self) -> Parse<Chain> {
        let mut levels = vec![Level {
            chain: Chain::default(),
            kind: LevelKind::Item,
        }];
        'operand: loop {
            // Expect an operand: a lambda, a spaced tag or constructor, or
            // an application.
            if self.symbol_at(0, "\\") {
                let params = self.lambda_head()?;
                levels.push(Level {
                    chain: Chain::default(),
                    kind: LevelKind::Lambda(params),
                });
                continue 'operand;
            }
            let head = match self.tag_prefix_at(0) {
                Some(prefix) if prefix.payload != TagPayload::Joint => {
                    match self.open_tag(prefix, &mut levels)? {
                        Some(term) => term,
                        None => continue 'operand,
                    }
                }
                _ => self.application_head()?,
            };
            if let Some(fail) = self.tail_argument() {
                return Err(fail);
            }
            levels
                .last_mut()
                .expect("an item always has a level")
                .chain
                .push_operand(head);
            // After an operand: the end of the item, an update's body, a
            // postfix `if`, a pattern operator in cover mode, or an infix
            // operator, which resumes the chain at its indentation when
            // line-led.
            let (mut breaks, mut indentation) = self.line_starts_at(0);
            while self.with_at(breaks) {
                self.position += breaks;
                self.with_update(&mut levels)?;
                (breaks, indentation) = self.line_starts_at(0);
            }
            let Some(piece) = self.peek_at(breaks) else {
                break 'operand;
            };
            if self.postfix_if_at(breaks) {
                self.position += breaks;
                self.postfix_if(&mut levels)?;
                break 'operand;
            }
            if self.stops_at(breaks) {
                self.position += breaks;
                break 'operand;
            }
            let Some(operator) = piece_operator(self.scope.view, piece) else {
                self.position += breaks;
                if self.symbol_at(0, ",") {
                    return self
                        .error_here("unexpected `,` in an expression; parenthesize a tuple");
                }
                if self.symbol_at(0, ".")
                    && self.joint_at(1)
                    && self.shape_at(1) == Some(Shape::Unit)
                {
                    self.position += 1;
                    return self.error_here("a path splice requires an expression");
                }
                // A path or tag separated from its continuation by a space.
                if self.symbol_at(0, ".") && !self.joint_at(1) {
                    self.position += 1;
                    return self.expected("a name, bracket or parenthesis adjacent to `.`");
                }
                if self.symbol_at(0, ":") {
                    return self
                        .error_here("a tag's `:` must be joint to its key, as in `key: value`");
                }
                return self.expected("an infix operator or the end of the expression");
            };
            let operator_token = piece_token(piece);
            self.position += breaks + 1;
            self.skip_line_starts();
            let chain = &mut levels.last_mut().expect("levels remain").chain;
            chain.pending = Some(operator);
            chain
                .anchors
                .extend(indentation.map(|indentation| (indentation, operator_token)));
            if self.peek().is_none() {
                return self.error_here("an infix operator requires a right operand");
            }
        }
        self.close_levels(levels)
    }

    /// The chain as an expression chain, located at the cursor.
    fn infix_chain(&self, chain: Chain) -> Parse<InfixChain> {
        chain
            .finish(self.scope.context)
            .map_err(|fail| fail.or_at(self.here()))
    }

    /// Closes lambdas and spaced tags innermost first. Each one ends with
    /// the item, so the item's chain then ends in an open one.
    fn close_levels(&self, mut levels: Vec<Level>) -> Parse<Chain> {
        loop {
            let level = levels.pop().expect("levels remain while closing");
            let body = |chain| {
                self.infix_chain(chain)?
                    .resolve()
                    .map_err(|message| Fail::error(message).or_at(self.here()))
            };
            let operand = match level.kind {
                LevelKind::Item => {
                    debug_assert!(levels.is_empty(), "only the first level is the item's");
                    return Ok(level.chain);
                }
                LevelKind::Lambda(params) => {
                    SyntaxExpr::Lambda(params, Box::new(body(level.chain)?))
                }
                LevelKind::Tag(path, optional) => tag_expr(path, optional, body(level.chain)?),
            };
            levels
                .last_mut()
                .expect("a lambda's opening level remains")
                .chain
                .push_operand(Term::Expr(operand));
        }
    }

    /// Whether the piece at `offset` is a conditional that reads as postfix:
    /// one with no `then`, after an operand.
    fn postfix_if_at(&self, offset: usize) -> bool {
        match self.peek_at(offset) {
            Some(Piece::Form { form, .. }) => matches!(
                self.scope.covers.form_shape(*form),
                (FormShape::Postfix | FormShape::Unfinished, "if")
            ),
            _ => false,
        }
    }

    /// A postfix `if` after the innermost level's chain, which becomes that
    /// level's result. An open lambda binds a maximal trailing expression, so
    /// the conditional is the innermost lambda's body, or the whole item when
    /// no lambda is open. Nothing but a stop (see `stops_at`) may follow it
    /// in the item.
    fn postfix_if(&mut self, levels: &mut [Level]) -> Parse<()> {
        let Some(Piece::Form { form, .. }) = self.peek() else {
            unreachable!("a postfix `if` is a form piece");
        };
        let form = self.scope.covers.take_form(*form);
        self.position += 1;
        let if_index = form.head;
        let result = match form.shape {
            FormShape::Unfinished => Err(Fail::error(
                "postfix `if` requires `else` and a fallback result",
            )
            .or_at(Some(form.head))),
            _ => form.result,
        };
        let (breaks, _) = self.line_starts_at(0);
        if self.peek_at(breaks).is_some() && !self.stops_at(breaks) {
            self.position += breaks;
            return self.expected("the end of the expression after a postfix `if`");
        }
        self.position += breaks;
        let level = levels.last_mut().expect("an item always has a level");
        let left = std::mem::take(&mut level.chain)
            .finish(self.scope.context)
            .and_then(|chain| chain.resolve().map_err(Fail::error))
            .map_err(|fail| fail.or_at(Some(if_index)))?;
        let SyntaxExpr::If(mut postfix) = result? else {
            unreachable!("a postfix conditional parses to an if expression");
        };
        postfix.then_result = Box::new(left);
        level
            .chain
            .push_operand(Term::Expr(SyntaxExpr::If(postfix)));
        Ok(())
    }

    /// `abstract_global_path` and a static name path, after a space or line
    /// breaks.
    fn abstract_global_path(&mut self) -> Parse<SyntaxExpr> {
        self.position += 1;
        let (breaks, _) = self.line_starts_at(0);
        if breaks == 0 && self.leading_at(0) != Some(LeadingTrivia::Space) {
            return self.expected("a global path after `abstract_global_path`");
        }
        self.position += breaks;
        let root = match self.kind() {
            Some(TokenKind::Name(root)) if is_glam_name(root) => *root,
            _ => return self.expected("a global path after `abstract_global_path`"),
        };
        self.position += 1;
        let mut path = Vec::new();
        while self.symbol_at(0, ".")
            && self.joint_at(0)
            && self.joint_at(1)
            && let Some(Piece::Token(index)) = self.peek_at(1)
            && let TokenKind::Name(name) = token(self.scope.view, *index).kind()
            && is_glam_name(name)
        {
            path.push((*name).to_owned());
            self.position += 2;
        }
        if root == "module" {
            if path.is_empty() {
                return self.error_here(
                    "`abstract_global_path module` requires a relative path after `module.`",
                );
            }
            return Ok(SyntaxExpr::AbstractGlobalPath {
                explicit_module: true,
                path,
            });
        }
        if let Some(keyword) = g0_keyword(root) {
            return self.error_here(reserved_keyword_message(keyword));
        }
        path.insert(0, root.to_owned());
        Ok(SyntaxExpr::AbstractGlobalPath {
            explicit_module: false,
            path,
        })
    }

    /// The path of a tag section, when the items are only a tag prefix with
    /// nothing after it: `foo:` in `(foo:)`.
    fn tag_section(&mut self) -> Option<Parse<Vec<SyntaxKeyExpr>>> {
        let prefix = self.tag_prefix_at(0)?;
        let end = self.data_path_end()? + 1;
        (prefix.payload == TagPayload::Bare && !prefix.optional && end == self.pieces.len())
            .then(|| self.data_path())
    }

    /// The error for a lambda or a spaced tag after an application, after a
    /// space or line breaks at the cursor, which would stand as its
    /// argument.
    fn tail_argument(&self) -> Option<Fail> {
        let (breaks, _) = self.line_starts_at(0);
        if breaks == 0 && self.leading_at(0) != Some(LeadingTrivia::Space) {
            return None;
        }
        let label = if self.symbol_at(breaks, "\\") {
            "lambda"
        } else if self
            .tag_prefix_at(breaks)
            .is_some_and(|prefix| prefix.payload == TagPayload::Spaced)
        {
            "spaced tag"
        } else {
            return None;
        };
        Some(tail_argument(self.at(self.position + breaks).here(), label))
    }

    /// The tag prefix at `offset`, if one is there: a data path, in a
    /// pattern an optional `?`, and a `:` joint to it.
    fn tag_prefix_at(&self, offset: usize) -> Option<TagPrefix> {
        let start = self.position + offset;
        let end = self.at(start).data_path_end()? - self.position;
        let optional = self.symbol_at(end, "?") && self.joint_at(end);
        let colon = end + usize::from(optional);
        if !(self.symbol_at(colon, ":") && self.joint_at(colon)) {
            return None;
        }
        let (breaks, _) = self.line_starts_at(colon + 1);
        let payload =
            if self.peek_at(colon + 1 + breaks).is_none() || self.stops_at(colon + 1 + breaks) {
                TagPayload::Bare
            } else if breaks == 0 && self.joint_at(colon + 1) {
                TagPayload::Joint
            } else {
                TagPayload::Spaced
            };
        Some(TagPrefix { optional, payload })
    }

    /// Takes a spaced tag prefix at the cursor; a bare one, with no
    /// payload, is an error. In an expression it opens a level, its payload
    /// the rest of the item; in a pattern its payload is the rest of the
    /// item as a complete position, and the tag is returned.
    fn open_tag(&mut self, prefix: TagPrefix, levels: &mut Vec<Level>) -> Parse<Option<Term>> {
        let at = self.here();
        let path = self.data_path()?;
        self.position += 1 + usize::from(prefix.optional);
        if prefix.payload == TagPayload::Bare {
            return Err(Fail::error(missing_payload(prefix.optional)).or_at(at));
        }
        self.skip_line_starts();
        if self
            .peek()
            .is_some_and(|piece| piece_operator(self.scope.view, piece).is_some())
            && !self.named_tag_at(0)
        {
            return self.error_here(
                "a spaced tag takes the rest of its item as its payload, which an operator cannot begin; write `(path:)` for the constructor",
            );
        }
        match self.scope.mode {
            Mode::Expression => {
                levels.push(Level {
                    chain: Chain::default(),
                    kind: LevelKind::Tag(path, prefix.optional),
                });
                Ok(None)
            }
            Mode::Cover => {
                let payload =
                    Items::new(self.scope, &self.pieces[self.position..]).pattern_item()?;
                self.position = self.pieces.len();
                let payload = Box::new(Term::Complete(Box::new(payload)));
                Ok(Some(if prefix.optional {
                    Term::OptionalTag(path, payload)
                } else {
                    Term::Tag(path, payload)
                }))
            }
        }
    }

    /// `\ name+ ->`, returning the parameters. Line breaks may pad each part.
    fn lambda_head(&mut self) -> Parse<Vec<String>> {
        self.position += 1;
        self.skip_line_starts();
        let mut params = Vec::new();
        while let Some(TokenKind::Name(name)) = self.kind() {
            let is_local = *name == "_"
                || is_glam_name(name)
                || name.strip_prefix('_').is_some_and(is_glam_name);
            if !is_local {
                return self.expected("a lambda parameter");
            }
            if let Some(keyword) = canonical_keyword(name) {
                return self.error_here(reserved_keyword_message(keyword));
            }
            params.push((*name).to_owned());
            self.position += 1;
            self.skip_line_starts();
        }
        if params.is_empty() {
            return self.expected("a lambda parameter");
        }
        if !self.symbol_at(0, "->") {
            return self.expected("`->` after the lambda parameters");
        }
        self.position += 1;
        self.skip_line_starts();
        if self.peek().is_none() {
            return self.error_here("a lambda requires a body");
        }
        Ok(params)
    }

    /// An atom followed by argument atoms, each after a space or after line
    /// breaks.
    fn application_head(&mut self) -> Parse<Term> {
        let form = self.prefix_form_at(0);
        let mut function = self.atom()?;
        loop {
            let (breaks, _) = self.line_starts_at(0);
            let spaced = breaks > 0 || self.leading_at(0) == Some(LeadingTrivia::Space);
            // A postfix `if` ends the application, and a spaced tag would
            // be its argument, which `chain` reports.
            if !(spaced && self.starts_atom_at(breaks))
                || self.postfix_if_at(breaks)
                || self
                    .tag_prefix_at(breaks)
                    .is_some_and(|prefix| prefix.payload == TagPayload::Spaced)
            {
                break;
            }
            // A tail form stands only where nothing but an operator or a
            // keyword follows it.
            if let Some((head, keyword)) = form {
                return Err(misplaced_tail(head, &form_label(keyword), "an argument"));
            }
            // An argument is an atom, never a tail form.
            if let Some((head, keyword)) = self.prefix_form_at(breaks) {
                return Err(tail_argument(Some(head), &form_label(keyword)));
            }
            let at = self.here();
            self.position += breaks;
            if self.symbol_at(0, ".") {
                return self.error_here(
                    "dot-leading application arguments must be parenthesized; write `f (.bar)` or use `<|`",
                );
            }
            let argument = self.atom()?;
            // A function is an expression in both roles.
            let function_expr = function
                .into_expr()
                .map_err(|message| Fail::error(message).or_at(at))?;
            function = match self.scope.mode {
                Mode::Expression => Term::Expr(SyntaxExpr::Apply(
                    Box::new(function_expr),
                    Box::new(argument.into_expr().map_err(Fail::error)?),
                )),
                Mode::Cover => Term::Apply(Box::new(function_expr), Box::new(argument)),
            };
        }
        Ok(function)
    }

    /// The head and keyword of the prefix form at `offset`, if one is there.
    fn prefix_form_at(&self, offset: usize) -> Option<(usize, &'static str)> {
        match self.peek_at(offset) {
            Some(Piece::Form { head, form, .. }) => match self.scope.covers.form_shape(*form) {
                (FormShape::Prefix, keyword) => Some((*head, keyword)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether an argument atom starts at `offset`. The operator names `and`
    /// and `or` start one only as a tag's key, as in `and:x`, and in cover
    /// mode a pattern operator starts none.
    fn starts_atom_at(&self, offset: usize) -> bool {
        match self.peek_at(offset) {
            Some(Piece::Group { .. }) => true,
            Some(Piece::Form { form, .. }) => {
                self.scope.covers.form_shape(*form).0 == FormShape::Prefix
            }
            Some(piece @ Piece::Token(_)) => match piece_kind(self.scope.view, piece) {
                Some(TokenKind::Name(name)) => {
                    (!matches!(*name, "and" | "or") || self.named_tag_at(offset))
                        && self.pattern_operator_at(offset).is_none()
                        && !self.where_at(offset)
                }
                Some(TokenKind::Number(_) | TokenKind::Text(_) | TokenKind::Embedded(_)) => true,
                // A `:` starts a pun only joint to its name.
                Some(TokenKind::Symbol(":")) => {
                    self.joint_at(offset + 1)
                        && matches!(
                            self.peek_at(offset + 1)
                                .and_then(|piece| piece_kind(self.scope.view, piece)),
                            Some(TokenKind::Name(_))
                        )
                }
                Some(TokenKind::Symbol(symbol)) => matches!(*symbol, "'" | "." | "^"),
                _ => false,
            },
            None => false,
        }
    }

    /// Whether a named tag prefix, `name.path:`, starts at `offset`.
    fn named_tag_at(&self, offset: usize) -> bool {
        matches!(
            self.peek_at(offset)
                .and_then(|piece| piece_kind(self.scope.view, piece)),
            Some(TokenKind::Name(_))
        ) && self.tag_prefix_at(offset).is_some()
    }

    /// One atom, with any joint tag prefixes (`a:`, `[k]:`, `(p):`, and in
    /// a pattern `a?:`) applied to its payload. The prefixes are a loop, not
    /// recursion.
    fn atom(&mut self) -> Parse<Term> {
        let mut tags: Vec<(Vec<SyntaxKeyExpr>, bool)> = Vec::new();
        let base = loop {
            if let Some(prefix) = self.tag_prefix_at(0) {
                let at = self.here();
                match prefix.payload {
                    TagPayload::Joint => {
                        let path = self.data_path()?;
                        self.position += 1 + usize::from(prefix.optional);
                        tags.push((path, prefix.optional));
                        continue;
                    }
                    TagPayload::Bare if prefix.optional || tags.is_empty() => {
                        return Err(Fail::error(missing_payload(prefix.optional)).or_at(at));
                    }
                    TagPayload::Bare => {
                        let path = self.data_path()?;
                        return Err(Fail::error(chain_missing_payload(&tags, &path)).or_at(at));
                    }
                    TagPayload::Spaced => {
                        return self.error_here(
                            "a spaced tag takes the rest of its item; parenthesize `(path: …)` here",
                        );
                    }
                }
            }
            if let Some(message) = self.misplaced_optional_marker() {
                return self.error_here(message);
            }
            if !tags.is_empty() && self.symbol_at(0, "\\") {
                return self.error_here(
                    "a joint tag's payload is an atom; write `path: \\x -> …` with a space for a lambda",
                );
            }
            let key_mark = self.symbol_at(1, ":") && self.joint_at(1);
            match self.peek() {
                None => return self.expected("an expression"),
                Some(Piece::Form { .. }) => break Term::Expr(self.conditional_atom()?),
                Some(Piece::Group { .. }) => match self.shape_at(0) {
                    Some(Shape::Bracket { empty: true }) if key_mark => {
                        return self.error_here("dictionary paths cannot be empty");
                    }
                    Some(Shape::Unit) if key_mark => {
                        return self.error_here("a path splice requires an expression");
                    }
                    _ => break self.literal_group()?,
                },
                Some(Piece::Token(_)) => match self.kind() {
                    Some(TokenKind::Symbol(":")) => break self.pun()?,
                    Some(TokenKind::Name(name)) if is_glam_name(name) => {
                        // `abstract_global_path` heads its form unless it is
                        // a tag's key; the pass made every other form a piece.
                        if *name == "abstract_global_path" {
                            break Term::Expr(self.abstract_global_path()?);
                        }
                        let path = self.named_path(name)?;
                        break self.name_with_path(path)?;
                    }
                    _ => break self.base_atom()?,
                },
            }
        };
        tags.into_iter()
            .rev()
            .try_fold(base, |payload, (path, optional)| match self.scope.mode {
                Mode::Expression => Ok(Term::Expr(tag_expr(
                    path,
                    optional,
                    payload.into_expr().map_err(Fail::error)?,
                ))),
                Mode::Cover if optional => Ok(Term::OptionalTag(path, Box::new(payload))),
                Mode::Cover => Ok(Term::Tag(path, Box::new(payload))),
            })
    }

    /// A prefix conditional, with path suffixes. A conditional that reads
    /// as postfix stands where no operand precedes it.
    fn conditional_atom(&mut self) -> Parse<SyntaxExpr> {
        let Some(Piece::Form { form, .. }) = self.peek() else {
            unreachable!("conditional_atom is called on a form");
        };
        let form = self.scope.covers.take_form(*form);
        self.position += 1;
        let expression = match form.shape {
            FormShape::Prefix => form.result?,
            FormShape::Postfix => {
                return Err(Fail::error(format!(
                    "{} expression requires `then` before `else`",
                    form.keyword
                ))
                .or_at(Some(form.head)));
            }
            FormShape::Unfinished => {
                return Err(Fail::error(format!(
                    "{} expression requires `then` and `else`",
                    form.keyword
                ))
                .or_at(Some(form.head)));
            }
            FormShape::With => {
                return Err(Fail::error("with expression requires a base expression")
                    .or_at(Some(form.head)));
            }
            // A body reads its `where` before its items do.
            FormShape::Where => {
                return Err(Fail::error(
                    "`where` scopes a whole definition, statement or arm; parenthesize `(… where …)` or use `let … in` for part of an expression",
                )
                .or_at(Some(form.head)));
            }
        };
        let suffixes = self.path_suffixes()?;
        if !suffixes.is_empty() {
            return Err(misplaced_tail(
                form.head,
                &form_label(form.keyword),
                "a path",
            ));
        }
        Ok(access_if_path(expression, suffixes))
    }

    /// What is wrong with an optional entry's marker at the cursor, in a
    /// pattern, when it makes no tag prefix.
    fn misplaced_optional_marker(&self) -> Option<&'static str> {
        if self.scope.mode != Mode::Cover {
            return None;
        }
        if self.symbol_at(0, "?") {
            return Some("optional dictionary entry pattern requires a path before `?:`");
        }
        let end = self.data_path_end()? - self.position;
        if !self.symbol_at(end, "?") {
            return None;
        }
        Some(if !self.joint_at(end) {
            "optional dictionary entry marker `?` must be joint with its path"
        } else {
            "optional dictionary entry marker must be written as joint `?:`"
        })
    }

    /// `:name`, a pun: `name:name`, which in a pattern captures `name`
    /// under its key. A pun takes one bare name.
    fn pun(&mut self) -> Parse<Term> {
        self.position += 1;
        let name = match self.kind() {
            Some(TokenKind::Name(name)) if self.joint_at(0) => *name,
            _ => return self.expected("a name adjacent to `:`"),
        };
        self.position += 1;
        if self.path_suffix_width().is_some() {
            return self
                .error_here("a pun `:name` takes one bare name; write `a.b:a.b` for a path");
        }
        Ok(match self.scope.mode {
            Mode::Expression => {
                let name = validate_expr_name(name).map_err(Fail::error)?;
                Term::Expr(SyntaxExpr::PathDict(
                    vec![SyntaxKeyExpr::Atom(name.clone())],
                    Box::new(SyntaxExpr::Name(name)),
                ))
            }
            Mode::Cover => Term::Pun(name.to_owned()),
        })
    }

    /// A key name with its path suffixes, as keys.
    fn named_path(&mut self, head: &str) -> Parse<Vec<SyntaxKeyExpr>> {
        self.position += 1;
        let mut path = vec![SyntaxKeyExpr::Atom(head.to_owned())];
        for suffix in self.path_suffixes()? {
            match suffix {
                PathSuffix::Single(key) => path.push(key),
                PathSuffix::Expand(keys) => path.extend(keys),
            }
        }
        Ok(path)
    }

    /// A named path that is not a tag: a name, then an access. In cover
    /// mode a bare name is checked by role.
    fn name_with_path(&self, mut path: Vec<SyntaxKeyExpr>) -> Parse<Term> {
        let SyntaxKeyExpr::Atom(name) = path.remove(0) else {
            unreachable!("named paths begin with an atom key");
        };
        if self.scope.mode == Mode::Cover && path.is_empty() {
            return Ok(Term::Name(name));
        }
        let base = SyntaxExpr::Name(validate_expr_name(&name).map_err(Fail::error)?);
        Ok(Term::Expr(if path.is_empty() {
            base
        } else {
            SyntaxExpr::Access(Box::new(base), path)
        }))
    }

    /// Consumes the data path at the cursor, whose extent `data_path_end`
    /// has accepted: a named path, a bracket key path, or a parenthesized
    /// path index.
    fn data_path(&mut self) -> Parse<Vec<SyntaxKeyExpr>> {
        match self.shape_at(0) {
            Some(Shape::Bracket { empty: false }) => self.take_keys(),
            Some(Shape::Grouped) => Ok(vec![SyntaxKeyExpr::PathIndex(Box::new(
                self.take_grouped()?,
            ))]),
            Some(_) => self.expected("a dictionary path"),
            None => match self.kind() {
                Some(TokenKind::Name(name)) if is_glam_name(name) => self.named_path(name),
                _ => self.expected("a dictionary path"),
            },
        }
    }

    /// Joint `.name`, `.[keys]` and `.(index)` suffixes.
    fn path_suffixes(&mut self) -> Parse<Vec<PathSuffix>> {
        let mut suffixes = Vec::new();
        while self.path_suffix_width().is_some() {
            self.position += 1;
            let suffix = match self.shape_at(0) {
                Some(Shape::Bracket { .. }) => PathSuffix::Expand(self.take_keys()?),
                Some(Shape::Grouped) => {
                    PathSuffix::Single(SyntaxKeyExpr::PathIndex(Box::new(self.take_grouped()?)))
                }
                Some(Shape::Unit | Shape::Other) => {
                    unreachable!("path_suffix_width accepts no other group")
                }
                None => {
                    let Some(TokenKind::Name(name)) = self.kind() else {
                        unreachable!("path_suffix_width accepts a name");
                    };
                    self.position += 1;
                    PathSuffix::Single(SyntaxKeyExpr::Atom((*name).to_owned()))
                }
            };
            suffixes.push(suffix);
        }
        Ok(suffixes)
    }

    /// A group used as a literal: list, dict, unit, grouping, tuple or
    /// section, with path suffixes, which make it an expression.
    fn literal_group(&mut self) -> Parse<Term> {
        let at = self.here();
        let base = match self.take_group()? {
            Cover::Bracket(items) => Term::List(items),
            Cover::Brace(members) => Term::Dict(members),
            Cover::Paren(ParenCover::Unit) => Term::Expr(SyntaxExpr::Unit),
            Cover::Paren(ParenCover::Grouped(term)) => Term::Group(Box::new(term)),
            Cover::Paren(ParenCover::Section(expr)) => Term::Expr(expr),
            Cover::Paren(ParenCover::Tuple(items)) => Term::Tuple(items),
            Cover::Invalid(_) => unreachable!("take_group surfaces invalid covers"),
        };
        let suffixes = self.path_suffixes()?;
        if self.scope.mode == Mode::Cover && suffixes.is_empty() {
            return Ok(base);
        }
        let base = base
            .into_expr()
            .map_err(|message| Fail::error(message).or_at(at))?;
        Ok(Term::Expr(access_if_path(base, suffixes)))
    }

    fn base_atom(&mut self) -> Parse<Term> {
        let Some(kind) = self.kind() else {
            return self.expected("an expression");
        };
        let base = match kind {
            TokenKind::Number(id) => {
                self.position += 1;
                SyntaxExpr::Number(
                    self.scope
                        .view
                        .number(*id)
                        .expect("number tokens refer to lexer-owned values")
                        .clone(),
                )
            }
            TokenKind::Text(id) => {
                self.position += 1;
                SyntaxExpr::Text(
                    self.scope
                        .view
                        .text(*id)
                        .expect("text tokens refer to lexer-owned values")
                        .value()
                        .to_owned(),
                )
            }
            TokenKind::Embedded(id) => {
                self.position += 1;
                SyntaxExpr::Embedded(
                    self.scope
                        .view
                        .embedded_value(*id)
                        .expect("embedded tokens refer to lexer-owned values")
                        .clone(),
                )
            }
            TokenKind::Symbol("'") => return self.quoted(),
            TokenKind::Symbol(".") => return self.effect().map(Term::Expr),
            TokenKind::Symbol("^") => return self.escape().map(Term::Expr),
            TokenKind::Name(name) => return self.rooted_name(name),
            _ => return self.expected("an expression"),
        };
        let suffixes = self.path_suffixes()?;
        Ok(Term::Expr(access_if_path(base, suffixes)))
    }

    /// `'name` or `'.path`, with path suffixes. In cover mode a quoted path
    /// keeps its keys.
    fn quoted(&mut self) -> Parse<Term> {
        self.position += 1;
        if !self.joint_at(0) {
            return self.expected("a quoted name or path after `'`");
        }
        let base = if self.symbol_at(0, ".") {
            let suffixes = self.path_suffixes()?;
            if suffixes.is_empty() {
                return self.expected("a quoted path after `'`");
            }
            // The suffixes take every joint path segment, so none follow.
            return Ok(match self.scope.mode {
                Mode::Expression => Term::Expr(quoted_path(suffixes)),
                Mode::Cover => Term::Quoted(crate::g_syntax::flatten_path_suffixes(suffixes)),
            });
        } else {
            match self.kind() {
                Some(TokenKind::Name(name)) if is_glam_name(name) => {
                    self.position += 1;
                    SyntaxExpr::Atom((*name).to_owned())
                }
                _ => return self.expected("a quoted name or path after `'`"),
            }
        };
        let suffixes = self.path_suffixes()?;
        Ok(Term::Expr(access_if_path(base, suffixes)))
    }

    /// `.name.name`, an effect.
    fn effect(&mut self) -> Parse<SyntaxExpr> {
        let mut path = Vec::new();
        while self.symbol_at(0, ".")
            && (path.is_empty() || self.joint_at(0))
            && self.joint_at(1)
            && let Some(Piece::Token(index)) = self.peek_at(1)
            && let TokenKind::Name(name) = token(self.scope.view, *index).kind()
            && is_glam_name(name)
        {
            path.push((*name).to_owned());
            self.position += 2;
        }
        if path.is_empty() {
            return self.expected("an effect name after `.`");
        }
        Ok(SyntaxExpr::Effect(path))
    }

    /// `^target`, `^^target`, with path suffixes.
    fn escape(&mut self) -> Parse<SyntaxExpr> {
        self.position += 1;
        let mut carets = 1;
        while self.symbol_at(0, "^") && self.joint_at(0) {
            carets += 1;
            self.position += 1;
        }
        if !self.joint_at(0) {
            return self.expected("an escaped name or group");
        }
        let target = match self.peek() {
            Some(Piece::Group { .. }) if self.shape_at(0) == Some(Shape::Grouped) => {
                self.take_grouped()?
            }
            Some(Piece::Token(_)) => match self.kind() {
                Some(TokenKind::Name(name)) => {
                    let name = validate_expr_name(name)
                        .map_err(|message| Fail::error(message).or_at(self.here()))?;
                    self.position += 1;
                    let suffixes = self.path_suffixes()?;
                    access_if_path(SyntaxExpr::Name(name), suffixes)
                }
                _ => return self.expected("an escaped name or group"),
            },
            _ => return self.expected("an escaped name or group"),
        };
        let suffixes = self.path_suffixes()?;
        Ok(access_if_path(
            SyntaxExpr::Escape(carets, Box::new(target)),
            suffixes,
        ))
    }

    /// A prior name `_name`, with path suffixes. In cover mode `_` is a
    /// wildcard, and a bare `_name` is checked by role.
    fn rooted_name(&mut self, name: &str) -> Parse<Term> {
        let Some(prior) = name.strip_prefix('_') else {
            return self.expected("a name");
        };
        if self.scope.mode == Mode::Cover && !self.next_is_path_suffix() {
            if prior.is_empty() {
                self.position += 1;
                return Ok(Term::Wildcard);
            }
            if is_glam_name(prior) {
                self.position += 1;
                return Ok(Term::Name(name.to_owned()));
            }
        }
        if !is_glam_name(prior) {
            return self.expected("a name after `_`");
        }
        if prior == "module_origin" {
            return self.error_here("`module_origin` is reserved");
        }
        let prior =
            validate_expr_name(prior).map_err(|message| Fail::error(message).or_at(self.here()))?;
        self.position += 1;
        let suffixes = self.path_suffixes()?;
        Ok(Term::Expr(access_if_path(
            SyntaxExpr::PriorName(prior),
            suffixes,
        )))
    }

    /// Whether a path suffix follows the name at the cursor.
    fn next_is_path_suffix(&self) -> bool {
        let cursor = self.at(self.position + 1);
        cursor.path_suffix_width().is_some()
    }
}

#[cfg(test)]
mod tests;
