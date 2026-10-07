//! Prefix-shared term parser for ordinary expressions.
//!
//! Production parses every ordinary expression with it; the Chumsky grammar
//! remains only as a fallback for unsupported views and as a differential
//! oracle, which tests run against every expression the corpus parses. See
//! the parser backtracking plan, which tracks the remaining steps.
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
//! - a line-led atom continues an application, and a line-led `\` is its
//!   tail lambda;
//! - a line-led infix operator resumes the chain at its indentation, which is
//!   checked against the caller's `ExpressionContext`.
//!
//! **Errors are deferred to use.** A group whose contents are invalid as an
//! expression keeps its error in its cover, and the error surfaces only if a
//! role consumes the cover. A keyword form's own groups, such as a `do` or
//! `match` body, are never consumed here, so they raise nothing.
//!
//! **Keyword forms** (`if`, `match`, `try`, `try_match`, `do`, `using`, and
//! postfix `if`) are delegated to the structural parsers through the same
//! entry points the Chumsky grammar uses. The cursor then skips to the end
//! the structural parser reports. A postfix `if`, on the same line or a
//! continuation line, ends the innermost open lambda's body (`ChainTail`).
//!
//! **Coverage.** Constructs not yet covered report `Fail::Unsupported`:
//! keywords as lambda parameters, and invalid tokens.

use std::cell::RefCell;

use crate::g_syntax::keywords::{canonical_keyword, g0_keyword, reserved_keyword_message};
use crate::g_syntax::{PathSuffix, SyntaxExpr, SyntaxKeyExpr, SyntaxOperator};

use super::conditional::is_postfix_if_candidate;
use super::expression::{
    ChainTail, InfixChain, StructuralAtomParser, access_if_path, parse_postfix_if_tail,
    parse_structural_atom, quoted_path, structural_atom_parser, syntax_operator,
    validate_dict_colon_members, validate_expr_name,
};
use super::expression_context::ExpressionContext;
use super::input::TokenView;
use super::lexical::{Delimiter, LeadingTrivia, SpannedToken, TokenKind};

/// Why the term parser produced no syntax tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Fail {
    /// The view uses a construct this parser does not cover yet.
    Unsupported(&'static str),
    /// The view is not a valid expression. `at` is the token where parsing
    /// stopped, when known.
    Error { at: Option<usize>, message: String },
    /// A structural parser rejected a keyword form with these diagnostics.
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
/// chain, as `parse_expression_chain_view` does. `context` checks the
/// indentation of line-led infix operators.
pub(super) fn parse_term_chain(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Parse<InfixChain> {
    prescan(view)?;
    let covers = Covers::default();
    let scope = Scope {
        view,
        context,
        covers: &covers,
    };
    let mut stack = vec![Frame {
        delimiter: None,
        group: None,
        close: 0,
        pieces: Vec::new(),
    }];
    let range = view.range();
    let mut index = range.start();
    while index < range.end() {
        let token = token(view, index);
        match token.kind() {
            TokenKind::Open { group, delimiter } => {
                let close = view
                    .group(*group)
                    .and_then(|group| group.close_token())
                    .filter(|close| *close < range.end())
                    .ok_or(Fail::Unsupported("a delimiter group crossing the view"))?;
                stack.push(Frame {
                    delimiter: Some(*delimiter),
                    group: Some(*group),
                    close,
                    pieces: Vec::new(),
                });
            }
            TokenKind::Close { .. } => {
                let frame = stack.pop().expect("a close token ends an open frame");
                let open = view
                    .group(frame.group.expect("a group frame records its group"))
                    .expect("an open frame's group exists")
                    .open_token();
                let cover = covers.push(
                    interpret_group(scope, &frame, index)
                        .unwrap_or_else(|fail| Cover::Invalid(fail.or_at(Some(open)))),
                );
                stack
                    .last_mut()
                    .expect("the view frame encloses every group")
                    .pieces
                    .push(Piece::Group {
                        open,
                        close: index,
                        cover,
                    });
            }
            _ => stack
                .last_mut()
                .expect("the view frame is never popped")
                .pieces
                .push(Piece::Token(index)),
        }
        index += 1;
    }
    let top = stack.pop().expect("the view frame remains");
    debug_assert!(stack.is_empty(), "every group closes inside the view");
    // Trailing layout ends the expression; leading layout does not start one.
    Items::new(scope, trim_trailing_padding(view, &top.pieces)).item()
}

/// Rejects, before any group is interpreted, constructs this parser does not
/// cover yet, so an uncovered construct is never misreported as an error.
fn prescan(view: TokenView<'_, '_>) -> Parse<()> {
    if view.tokens().iter().any(|token| {
        matches!(
            token.kind(),
            TokenKind::InvalidNumber(_) | TokenKind::Unknown(_)
        )
    }) {
        return Err(Fail::Unsupported("an invalid token"));
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
}

struct Frame {
    delimiter: Option<Delimiter>,
    group: Option<super::lexical::GroupId>,
    close: usize,
    pieces: Vec<Piece>,
}

/// One element of a group's flat contents: a token, or a nested group
/// already interpreted into its cover form, held in the arena.
enum Piece {
    Token(usize),
    Group {
        open: usize,
        close: usize,
        cover: usize,
    },
}

/// Every interpreted group's cover. A cover is inspected freely but taken
/// exactly once, by the role that consumes it, so no subtree is copied.
#[derive(Default)]
struct Covers {
    slots: RefCell<Vec<Option<Cover>>>,
}

impl Covers {
    fn push(&self, cover: Cover) -> usize {
        let mut slots = self.slots.borrow_mut();
        slots.push(Some(cover));
        slots.len() - 1
    }

    fn take(&self, id: usize) -> Cover {
        self.slots.borrow_mut()[id]
            .take()
            .expect("a cover is consumed exactly once")
    }

    fn shape(&self, id: usize) -> Shape {
        match self.slots.borrow()[id]
            .as_ref()
            .expect("a cover is inspected before it is consumed")
        {
            Cover::Bracket(items) => Shape::Bracket {
                empty: items.is_empty(),
            },
            Cover::Paren(ParenCover::Grouped(_)) => Shape::Grouped,
            Cover::Paren(_) | Cover::Brace(_) | Cover::Invalid(_) => Shape::Other,
        }
    }
}

/// What a cover can become without consuming it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Bracket { empty: bool },
    Grouped,
    Other,
}

/// A group's contents, interpreted once and converted by the parent's role.
enum Cover {
    Paren(ParenCover),
    /// A list, or a key path whose items convert by `SyntaxKeyExpr::from_item`.
    Bracket(Vec<SyntaxExpr>),
    Brace(Vec<SyntaxExpr>),
    /// Contents that are not a valid expression group; the failure surfaces
    /// only if a role consumes the group.
    Invalid(Fail),
}

enum ParenCover {
    Unit,
    Grouped(SyntaxExpr),
    Tuple(Vec<SyntaxExpr>),
    Section(SyntaxExpr),
}

/// The token that starts a piece: the token itself, or a group's open.
fn piece_token(piece: &Piece) -> usize {
    match piece {
        Piece::Token(index) => *index,
        Piece::Group { open, .. } => *open,
    }
}

fn piece_leading(view: TokenView<'_, '_>, piece: &Piece) -> LeadingTrivia {
    match piece {
        Piece::Token(index) => token(view, *index).leading(),
        Piece::Group { open, .. } => token(view, *open).leading(),
    }
}

fn piece_kind<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    piece: &Piece,
) -> Option<&'lex TokenKind<'source>> {
    match piece {
        Piece::Token(index) => Some(token(view, *index).kind()),
        Piece::Group { .. } => None,
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
        Piece::Group { .. } => None,
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

fn interpret_group(scope: Scope<'_, '_, '_>, frame: &Frame, close: usize) -> Parse<Cover> {
    let view = scope.view;
    let delimiter = frame.delimiter.expect("only group frames close");
    debug_assert_eq!(frame.close, close);
    match delimiter {
        Delimiter::Parenthesis => interpret_paren(scope, frame, close).map(Cover::Paren),
        Delimiter::Bracket => {
            let segments = segments(view, &frame.pieces);
            let items = separated_items(&segments, true, true)?;
            resolved_items(scope, items).map(Cover::Bracket)
        }
        Delimiter::Brace => {
            let group = frame.group.expect("a brace frame records its group");
            validate_dict_colon_members(view, group).map_err(Fail::error)?;
            let segments = segments(view, &frame.pieces);
            let items = separated_items(&segments, true, true)?;
            items
                .into_iter()
                .map(|member| dict_member(scope, member))
                .collect::<Parse<Vec<_>>>()
                .map(Cover::Brace)
        }
    }
}

fn interpret_paren(scope: Scope<'_, '_, '_>, frame: &Frame, close: usize) -> Parse<ParenCover> {
    let view = scope.view;
    let contents = trim_padding(view, &frame.pieces);
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
        return resolved(scope, contents).map(ParenCover::Grouped);
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

fn resolved_items(scope: Scope<'_, '_, '_>, items: Vec<&[Piece]>) -> Parse<Vec<SyntaxExpr>> {
    items
        .into_iter()
        .map(|item| resolved(scope, item))
        .collect()
}

fn resolved(scope: Scope<'_, '_, '_>, pieces: &[Piece]) -> Parse<SyntaxExpr> {
    Items::new(scope, pieces)
        .item()?
        .resolve()
        .map_err(Fail::error)
}

/// A dict member: a pun `:name`, a path member `path: value`, or an
/// expression. A path member is recognized before anything is consumed: the
/// path's extent is measured by shape, and only a `:` joint to it commits.
/// A space or line break may follow the colon.
fn dict_member(scope: Scope<'_, '_, '_>, member: &[Piece]) -> Parse<SyntaxExpr> {
    let view = scope.view;
    if is_symbol(view, &member[0], ":") {
        let Some(TokenKind::Name(name)) = member.get(1).and_then(|piece| piece_kind(view, piece))
        else {
            return error("dictionary shorthand `:name` requires a bare value name after `:`");
        };
        return Ok(SyntaxExpr::PathDict(
            vec![SyntaxKeyExpr::Atom((*name).to_owned())],
            Box::new(SyntaxExpr::Name((*name).to_owned())),
        ));
    }
    // A path member's colon is joint to its path, and its value runs to the
    // member's end; any other member, such as `f :tag`, is an expression.
    let mut cursor = Items::new(scope, member);
    if let Some(end) = cursor.data_path_end()
        && member
            .get(end)
            .is_some_and(|piece| is_symbol(view, piece, ":"))
        && cursor.joint_at(end)
    {
        let path = cursor.data_path()?;
        debug_assert_eq!(cursor.position, end);
        let value = trim_padding(view, &member[end + 1..]);
        if value.is_empty() {
            return error("a dictionary path member requires a value");
        }
        return Ok(SyntaxExpr::PathDict(
            path,
            Box::new(resolved(scope, value)?),
        ));
    }
    resolved(scope, member)
}

fn is_glam_name(name: &str) -> bool {
    name.starts_with(|character: char| character.is_ascii_alphabetic())
}

/// The flat interpreter over one item's pieces.
struct Items<'p, 'lex, 'source> {
    scope: Scope<'p, 'lex, 'source>,
    pieces: &'p [Piece],
    position: usize,
}

/// One open lambda in an item. Its body runs to the end of the item.
struct Level {
    chain: Chain,
    lambda: Option<(Vec<String>, Attach)>,
}

#[derive(Default)]
struct Chain {
    first: Option<SyntaxExpr>,
    rest: Vec<(SyntaxOperator, SyntaxExpr)>,
    pending: Option<SyntaxOperator>,
    /// The indentation and token of each line-led operator, in order.
    anchors: Vec<(usize, usize)>,
}

impl Chain {
    fn push_operand(&mut self, operand: SyntaxExpr) {
        match self.pending.take() {
            Some(operator) => self.rest.push((operator, operand)),
            None => {
                debug_assert!(self.first.is_none(), "operands alternate with operators");
                self.first = Some(operand);
            }
        }
    }

    fn finish(self, context: ExpressionContext) -> Parse<InfixChain> {
        if self.pending.is_some() {
            return error("an infix operator requires a right operand");
        }
        let first = self
            .first
            .ok_or_else(|| Fail::error("expected an expression"))?;
        self.anchors.into_iter().try_fold(
            InfixChain::from_parts(first, self.rest),
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
}

/// How a finished lambda joins the level that opened it.
enum Attach {
    /// As the next operand.
    Operand,
    /// As the final argument of an application head.
    TailArgument(SyntaxExpr),
}

impl<'p, 'lex, 'source> Items<'p, 'lex, 'source> {
    fn new(scope: Scope<'p, 'lex, 'source>, pieces: &'p [Piece]) -> Self {
        Self {
            scope,
            pieces,
            position: 0,
        }
    }

    fn shape_at(&self, offset: usize) -> Option<Shape> {
        match self.peek_at(offset) {
            Some(Piece::Group { cover, .. }) => Some(self.scope.covers.shape(*cover)),
            _ => None,
        }
    }

    /// Takes the cover at the cursor and advances past it.
    /// Takes the cover at the cursor and advances past it, surfacing a
    /// deferred error.
    fn take_group(&mut self) -> Parse<Cover> {
        let Some(Piece::Group { cover, .. }) = self.peek() else {
            unreachable!("take_group is called on a group");
        };
        self.position += 1;
        match self.scope.covers.take(*cover) {
            Cover::Invalid(fail) => Err(fail),
            cover => Ok(cover),
        }
    }

    fn take_grouped(&mut self) -> SyntaxExpr {
        match self.take_group() {
            Ok(Cover::Paren(ParenCover::Grouped(expr))) => expr,
            _ => unreachable!("the shape was checked as grouped"),
        }
    }

    fn take_keys(&mut self) -> Vec<SyntaxKeyExpr> {
        match self.take_group() {
            Ok(Cover::Bracket(items)) => items.into_iter().map(SyntaxKeyExpr::from_item).collect(),
            _ => unreachable!("the shape was checked as a bracket"),
        }
    }

    /// Where a data path starting at the cursor would end, measured by
    /// shape without consuming anything.
    fn data_path_end(&self) -> Option<usize> {
        let mut position = self.position;
        match self.peek() {
            Some(Piece::Group { cover, .. }) => match self.scope.covers.shape(*cover) {
                Shape::Bracket { empty: false } | Shape::Grouped => return Some(position + 1),
                _ => return None,
            },
            Some(piece @ Piece::Token(_)) => match piece_kind(self.scope.view, piece) {
                Some(TokenKind::Name(name)) if is_glam_name(name) => position += 1,
                _ => return None,
            },
            None => return None,
        }
        let mut cursor = Items {
            scope: self.scope,
            pieces: self.pieces,
            position,
        };
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
                self.scope.covers.shape(*cover),
                Shape::Bracket { .. } | Shape::Grouped
            ),
            Some(piece @ Piece::Token(_)) => matches!(
                piece_kind(self.scope.view, piece),
                Some(TokenKind::Name(name)) if is_glam_name(name)
            ),
            None => false,
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

    /// Parses the whole item: an infix chain of applications, where a lambda
    /// takes the rest of the item as its body.
    fn item(mut self) -> Parse<InfixChain> {
        let mut levels = vec![Level {
            chain: Chain::default(),
            lambda: None,
        }];
        'operand: loop {
            // Expect an operand: a lambda or an application.
            if self.symbol_at(0, "\\") {
                let params = self.lambda_head()?;
                levels.push(Level {
                    chain: Chain::default(),
                    lambda: Some((params, Attach::Operand)),
                });
                continue 'operand;
            }
            let head = self.application_head()?;
            if let Some(breaks) = self.tail_lambda_layout() {
                self.position += breaks;
                let params = self.lambda_head()?;
                levels.push(Level {
                    chain: Chain::default(),
                    lambda: Some((params, Attach::TailArgument(head))),
                });
                continue 'operand;
            }
            levels
                .last_mut()
                .expect("an item always has a level")
                .chain
                .push_operand(head);
            // After an operand: the end of the item, a postfix `if`, or an
            // infix operator, which resumes the chain at its indentation when
            // line-led.
            let (breaks, indentation) = self.line_starts_at(0);
            let Some(piece) = self.peek_at(breaks) else {
                break 'operand;
            };
            if self.postfix_if_at(breaks) {
                self.position += breaks;
                return self.postfix_if(levels);
            }
            let Some(operator) = piece_operator(self.scope.view, piece) else {
                self.position += breaks;
                if self.symbol_at(0, ",") {
                    return self
                        .error_here("unexpected `,` in an expression; parenthesize a tuple");
                }
                // A path or tag separated from its continuation by a space.
                if self.symbol_at(0, ".") && !self.joint_at(1) {
                    self.position += 1;
                    return self.expected("a name, bracket or parenthesis adjacent to `.`");
                }
                if self.symbol_at(0, ":") && !self.joint_at(1) {
                    self.position += 1;
                    return self.expected("a tag payload adjacent to `:`");
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

    /// Closes lambdas innermost first. Each one ends with the item, so the
    /// item's chain then ends in an open lambda.
    fn close_levels(&self, mut levels: Vec<Level>) -> Parse<InfixChain> {
        let tail = if levels.len() > 1 {
            ChainTail::OpenLambda
        } else {
            ChainTail::Closed
        };
        loop {
            let level = levels.pop().expect("levels remain while closing");
            let Some((params, attach)) = level.lambda else {
                debug_assert!(levels.is_empty(), "only the item level has no lambda");
                return level
                    .chain
                    .finish(self.scope.context)
                    .map(|chain| chain.with_tail(tail))
                    .map_err(|fail| fail.or_at(self.here()));
            };
            let body = level
                .chain
                .finish(self.scope.context)
                .and_then(|chain| chain.resolve().map_err(Fail::error))
                .map_err(|fail| fail.or_at(self.here()))?;
            let lambda = SyntaxExpr::Lambda(params, Box::new(body));
            let operand = match attach {
                Attach::Operand => lambda,
                Attach::TailArgument(head) => SyntaxExpr::Apply(Box::new(head), Box::new(lambda)),
            };
            levels
                .last_mut()
                .expect("a lambda's opening level remains")
                .chain
                .push_operand(operand);
        }
    }

    /// Whether the piece at `offset` is an `if` that the grammar reads as a
    /// postfix conditional rather than a prefix one.
    fn postfix_if_at(&self, offset: usize) -> bool {
        matches!(
            self.peek_at(offset),
            Some(Piece::Token(index))
                if matches!(token(self.scope.view, *index).kind(), TokenKind::Name("if"))
                    && !self.named_tag_at(offset)
                    && is_postfix_if_candidate(self.scope.view, *index)
        )
    }

    /// A postfix `if` after the innermost level's chain, which becomes that
    /// level's result. An open lambda binds a maximal trailing expression, so
    /// the conditional is the innermost lambda's body, or the whole item when
    /// no lambda is open. Nothing may follow it in the item.
    fn postfix_if(mut self, mut levels: Vec<Level>) -> Parse<InfixChain> {
        let if_index = self.here().expect("a postfix `if` is a piece");
        let parsed = parse_postfix_if_tail(self.scope.view, if_index, self.scope.context)
            .map_err(Fail::Reported)?;
        self.position += 1;
        self.skip_to_token(parsed.end())?;
        self.skip_line_starts();
        if self.peek().is_some() {
            return self.expected("the end of the expression after a postfix `if`");
        }
        let level = levels.last_mut().expect("an item always has a level");
        let left = std::mem::take(&mut level.chain)
            .finish(self.scope.context)
            .and_then(|chain| chain.resolve().map_err(Fail::error))
            .map_err(|fail| fail.or_at(Some(if_index)))?;
        let SyntaxExpr::If(mut postfix) = parsed
            .into_expression()
            .map_err(|message| Fail::error(message).or_at(Some(if_index)))?
        else {
            unreachable!("a postfix conditional parses to an if expression");
        };
        postfix.then_result = Box::new(left);
        level.chain.push_operand(SyntaxExpr::If(postfix));
        self.close_levels(levels)
    }

    /// Advances past every piece that starts before token `end`, where a
    /// structural parser stopped.
    fn skip_to_token(&mut self, end: usize) -> Parse<()> {
        while let Some(piece) = self.peek() {
            if piece_token(piece) >= end {
                return Ok(());
            }
            if let Piece::Group { close, .. } = piece
                && *close >= end
            {
                return Err(Fail::Unsupported("a structural form ending inside a group"));
            }
            self.position += 1;
        }
        // A form may end past the item only over layout the item trimmed.
        let item_end = self.pieces.last().map_or(0, |piece| match piece {
            Piece::Token(index) => index + 1,
            Piece::Group { close, .. } => close + 1,
        });
        let only_layout = (item_end..end).all(|index| {
            self.scope
                .view
                .token_at(index)
                .is_some_and(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
        });
        if !only_layout {
            return self.error_here("structural expression extends beyond its token range");
        }
        Ok(())
    }

    /// A keyword form delegated to its structural parser, with path
    /// suffixes.
    fn structural_atom(&mut self, parse: StructuralAtomParser) -> Parse<SyntaxExpr> {
        let head = self.here().expect("a keyword head is a piece");
        let parsed = parse_structural_atom(self.scope.view, head, self.scope.context, parse)
            .map_err(Fail::Reported)?;
        self.position += 1;
        self.skip_to_token(parsed.end())?;
        let expression = parsed
            .into_expression()
            .map_err(|message| Fail::error(message).or_at(Some(head)))?;
        let suffixes = self.path_suffixes()?;
        Ok(access_if_path(expression, suffixes))
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

    /// The line starts before a tail lambda argument at the cursor: a `\`
    /// after a space or after line breaks.
    fn tail_lambda_layout(&self) -> Option<usize> {
        let (breaks, _) = self.line_starts_at(0);
        let spaced = breaks > 0 || self.leading_at(0) == Some(LeadingTrivia::Space);
        (spaced && self.symbol_at(breaks, "\\")).then_some(breaks)
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
            if canonical_keyword(name).is_some() {
                return Err(Fail::Unsupported("a keyword lambda parameter"));
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
    fn application_head(&mut self) -> Parse<SyntaxExpr> {
        let mut function = self.atom()?;
        loop {
            let (breaks, _) = self.line_starts_at(0);
            let spaced = breaks > 0 || self.leading_at(0) == Some(LeadingTrivia::Space);
            // A postfix `if` ends the application.
            if !(spaced && self.starts_atom_at(breaks)) || self.postfix_if_at(breaks) {
                break;
            }
            self.position += breaks;
            if self.symbol_at(0, ".") {
                return self.error_here(
                    "dot-leading application arguments must be parenthesized; write `f (.bar)` or use `<|`",
                );
            }
            let argument = self.atom()?;
            function = SyntaxExpr::Apply(Box::new(function), Box::new(argument));
        }
        Ok(function)
    }

    /// Whether an argument atom starts at `offset`. The operator names `and`
    /// and `or` start one only as a tag's key, as in `and:x`.
    fn starts_atom_at(&self, offset: usize) -> bool {
        match self.peek_at(offset) {
            Some(Piece::Group { .. }) => true,
            Some(piece @ Piece::Token(_)) => match piece_kind(self.scope.view, piece) {
                Some(TokenKind::Name(name)) => {
                    !matches!(*name, "and" | "or") || self.named_tag_at(offset)
                }
                Some(TokenKind::Number(_) | TokenKind::Text(_) | TokenKind::Embedded(_)) => true,
                Some(TokenKind::Symbol(symbol)) => matches!(*symbol, ":" | "'" | "." | "^"),
                _ => false,
            },
            None => false,
        }
    }

    /// Whether a named tag, `name.path:payload`, starts at `offset`.
    fn named_tag_at(&self, offset: usize) -> bool {
        let cursor = Items {
            scope: self.scope,
            pieces: self.pieces,
            position: self.position + offset,
        };
        cursor.data_path_end().is_some_and(|end| {
            let colon = end - self.position;
            self.symbol_at(colon, ":") && self.joint_at(colon) && self.joint_at(colon + 1)
        })
    }

    /// One atom, with any tag prefixes (`a:`, `[k]:`, `(p):`) applied to its
    /// payload. The prefixes are a loop, not recursion.
    fn atom(&mut self) -> Parse<SyntaxExpr> {
        let mut tags: Vec<Vec<SyntaxKeyExpr>> = Vec::new();
        let base = loop {
            let tagged = self.symbol_at(1, ":") && self.joint_at(1) && self.joint_at(2);
            match self.peek() {
                None => return self.expected("an expression"),
                Some(Piece::Group { .. }) => match self.shape_at(0) {
                    Some(Shape::Bracket { empty: false }) if tagged => {
                        tags.push(self.take_keys());
                        self.position += 1;
                    }
                    Some(Shape::Bracket { empty: true }) if tagged => {
                        return self.error_here("dictionary paths cannot be empty");
                    }
                    Some(Shape::Grouped) if tagged => {
                        let index = self.take_grouped();
                        tags.push(vec![SyntaxKeyExpr::PathIndex(Box::new(index))]);
                        self.position += 1;
                    }
                    _ => break self.literal_group()?,
                },
                Some(Piece::Token(_)) => match self.kind() {
                    Some(TokenKind::Symbol(":")) => break self.constructor()?,
                    Some(TokenKind::Name(name)) if is_glam_name(name) => {
                        // A keyword heads a form unless it is a tag's key.
                        if !self.named_tag_at(0) {
                            if *name == "abstract_global_path" {
                                break self.abstract_global_path()?;
                            }
                            if let Some(parse) = structural_atom_parser(name) {
                                break self.structural_atom(parse)?;
                            }
                        }
                        let path = self.named_path(name)?;
                        if self.symbol_at(0, ":") && self.joint_at(0) && self.joint_at(1) {
                            tags.push(path);
                            self.position += 1;
                            continue;
                        }
                        break name_with_path(path)?;
                    }
                    _ => break self.base_atom()?,
                },
            }
        };
        Ok(tags.into_iter().rev().fold(base, |payload, path| {
            SyntaxExpr::PathDict(path, Box::new(payload))
        }))
    }

    /// `:path`, a tag constructor.
    fn constructor(&mut self) -> Parse<SyntaxExpr> {
        self.position += 1;
        if !self.joint_at(0) {
            return self.expected("a path adjacent to `:`");
        }
        if self.data_path_end().is_none() {
            return self.expected("a path after `:`");
        }
        self.data_path().map(SyntaxExpr::TaggedConstructor)
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

    /// Consumes the data path at the cursor, whose extent `data_path_end`
    /// has accepted: a named path, a bracket key path, or a parenthesized
    /// path index.
    fn data_path(&mut self) -> Parse<Vec<SyntaxKeyExpr>> {
        match self.shape_at(0) {
            Some(Shape::Bracket { empty: false }) => Ok(self.take_keys()),
            Some(Shape::Grouped) => Ok(vec![SyntaxKeyExpr::PathIndex(Box::new(
                self.take_grouped(),
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
                Some(Shape::Bracket { .. }) => PathSuffix::Expand(self.take_keys()),
                Some(Shape::Grouped) => {
                    PathSuffix::Single(SyntaxKeyExpr::PathIndex(Box::new(self.take_grouped())))
                }
                Some(Shape::Other) => unreachable!("path_suffix_width accepts no other group"),
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
    /// section, with path suffixes.
    fn literal_group(&mut self) -> Parse<SyntaxExpr> {
        let base = match self.take_group()? {
            Cover::Bracket(items) => SyntaxExpr::List(items),
            Cover::Brace(members) => SyntaxExpr::DictUnion(members),
            Cover::Paren(ParenCover::Unit) => SyntaxExpr::Unit,
            Cover::Paren(ParenCover::Grouped(expr) | ParenCover::Section(expr)) => expr,
            Cover::Paren(ParenCover::Tuple(items)) => SyntaxExpr::Tuple(items),
            Cover::Invalid(_) => unreachable!("take_group surfaces invalid covers"),
        };
        let suffixes = self.path_suffixes()?;
        Ok(access_if_path(base, suffixes))
    }

    fn base_atom(&mut self) -> Parse<SyntaxExpr> {
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
            TokenKind::Symbol(".") => return self.effect(),
            TokenKind::Symbol("^") => return self.escape(),
            TokenKind::Name(name) => return self.rooted_name(name),
            _ => return self.expected("an expression"),
        };
        let suffixes = self.path_suffixes()?;
        Ok(access_if_path(base, suffixes))
    }

    /// `'name` or `'.path`, with path suffixes.
    fn quoted(&mut self) -> Parse<SyntaxExpr> {
        self.position += 1;
        if !self.joint_at(0) {
            return self.expected("a quoted name or path after `'`");
        }
        let base = if self.symbol_at(0, ".") {
            let suffixes = self.path_suffixes()?;
            if suffixes.is_empty() {
                return self.expected("a quoted path after `'`");
            }
            quoted_path(suffixes)
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
        Ok(access_if_path(base, suffixes))
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
                self.take_grouped()
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

    /// A prior name `_name`, with path suffixes.
    fn rooted_name(&mut self, name: &str) -> Parse<SyntaxExpr> {
        let Some(prior) = name.strip_prefix('_') else {
            return self.expected("a name");
        };
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
        Ok(access_if_path(SyntaxExpr::PriorName(prior), suffixes))
    }
}

/// A named path that is not a tag: a validated name, then an access.
fn name_with_path(mut path: Vec<SyntaxKeyExpr>) -> Parse<SyntaxExpr> {
    let SyntaxKeyExpr::Atom(name) = path.remove(0) else {
        unreachable!("named paths begin with an atom key");
    };
    let base = SyntaxExpr::Name(validate_expr_name(&name).map_err(Fail::error)?);
    Ok(if path.is_empty() {
        base
    } else {
        SyntaxExpr::Access(Box::new(base), path)
    })
}

#[cfg(test)]
mod tests;
