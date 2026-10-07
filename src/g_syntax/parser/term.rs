//! Prefix-shared term parser for ordinary expressions.
//!
//! This is slice 2 of the parser redesign: a deterministic replacement for the
//! Chumsky expression grammar. Production does not use it yet. Tests check it
//! against the existing grammar on every expression the corpus parses.
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
//! **Coverage.** Constructs not yet covered report `Fail::Unsupported`:
//! - keyword-headed forms (`if`, `do`, `match`, `try`, `using`,
//!   `abstract_global_path`, postfix `if`), and keywords as lambda
//!   parameters;
//! - invalid tokens.

use std::cell::RefCell;

use crate::g_syntax::keywords::{canonical_keyword, g0_keyword};
use crate::g_syntax::{PathSuffix, SyntaxExpr, SyntaxKeyExpr, SyntaxOperator};

use super::expression::{
    InfixChain, access_if_path, quoted_path, syntax_operator, validate_dict_colon_members,
    validate_expr_name,
};
use super::expression_context::ExpressionContext;
use super::input::TokenView;
use super::lexical::{Delimiter, LeadingTrivia, SpannedToken, TokenKind};

/// Why the term parser produced no syntax tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Fail {
    /// The view uses a construct this parser does not cover yet.
    Unsupported(&'static str),
    /// The view is not a valid expression.
    Error(String),
}

type Parse<T> = Result<T, Fail>;

fn error<T>(message: impl Into<String>) -> Parse<T> {
    Err(Fail::Error(message.into()))
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
                let cover = covers.push(interpret_group(scope, &frame, index)?);
                stack
                    .last_mut()
                    .expect("the view frame encloses every group")
                    .pieces
                    .push(Piece::Group { open, cover });
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
    if top.pieces.iter().any(|piece| is_symbol(view, piece, ",")) {
        return error("unexpected `,` in an expression");
    }
    // Trailing layout ends the expression; leading layout does not start one.
    Items::new(scope, trim_trailing_padding(view, &top.pieces)).item()
}

/// Rejects, before any group is interpreted, constructs this parser does not
/// cover yet, so an uncovered construct is never misreported as an error.
fn prescan(view: TokenView<'_, '_>) -> Parse<()> {
    for token in view.tokens() {
        match token.kind() {
            TokenKind::Name(name) if is_unsupported_keyword(name) => {
                return Err(Fail::Unsupported("a keyword-headed form"));
            }
            TokenKind::InvalidNumber(_) | TokenKind::Unknown(_) => {
                return Err(Fail::Unsupported("an invalid token"));
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_unsupported_keyword(name: &str) -> bool {
    g0_keyword(name).is_some()
        && !matches!(name, "and" | "or" | "module" | "self" | "module_origin")
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
    Group { open: usize, cover: usize },
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
            Cover::Paren(_) | Cover::Brace(_) => Shape::Other,
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
}

enum ParenCover {
    Unit,
    Grouped(SyntaxExpr),
    Tuple(Vec<SyntaxExpr>),
    Section(SyntaxExpr),
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
            validate_dict_colon_members(view, group).map_err(Fail::Error)?;
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
        .map_err(Fail::Error)
}

/// A dict member: a pun `:name`, a path member `path: value`, or an
/// expression. A path member is recognized before anything is consumed: the
/// path's extent is measured by shape, and only a following `:` commits.
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
    let mut cursor = Items::new(scope, member);
    if let Some(end) = cursor.data_path_end()
        && let colon = end + cursor.line_starts_at(end).0
        && member
            .get(colon)
            .is_some_and(|piece| is_symbol(view, piece, ":"))
    {
        let path = cursor.data_path()?;
        debug_assert_eq!(cursor.position, end);
        let value = trim_padding(view, &member[colon + 1..]);
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
    /// The indentation of each line-led operator, in order.
    anchors: Vec<usize>,
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
            .ok_or_else(|| Fail::Error("expected an expression".to_owned()))?;
        self.anchors
            .into_iter()
            .try_fold(InfixChain::from_parts(first, self.rest), |chain, anchor| {
                chain.with_resumption_anchor(anchor, context)
            })
            .map_err(Fail::Error)
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
    fn take_group(&mut self) -> Cover {
        let Some(Piece::Group { cover, .. }) = self.peek() else {
            unreachable!("take_group is called on a group");
        };
        self.position += 1;
        self.scope.covers.take(*cover)
    }

    fn take_grouped(&mut self) -> SyntaxExpr {
        match self.take_group() {
            Cover::Paren(ParenCover::Grouped(expr)) => expr,
            _ => unreachable!("the shape was checked as grouped"),
        }
    }

    fn take_keys(&mut self) -> Vec<SyntaxKeyExpr> {
        match self.take_group() {
            Cover::Bracket(items) => items.into_iter().map(SyntaxKeyExpr::from_item).collect(),
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
            // After an operand: the end of the item, or an infix operator,
            // which resumes the chain at its indentation when line-led.
            let (breaks, indentation) = self.line_starts_at(0);
            let Some(piece) = self.peek_at(breaks) else {
                break 'operand;
            };
            let Some(operator) = piece_operator(self.scope.view, piece) else {
                return error("unexpected token after an expression");
            };
            self.position += breaks + 1;
            self.skip_line_starts();
            let chain = &mut levels.last_mut().expect("levels remain").chain;
            chain.pending = Some(operator);
            chain.anchors.extend(indentation);
            if self.peek().is_none() {
                return error("an infix operator requires a right operand");
            }
        }
        // Close lambdas innermost first; each one ends with the item.
        loop {
            let level = levels.pop().expect("levels remain while closing");
            let Some((params, attach)) = level.lambda else {
                debug_assert!(levels.is_empty(), "only the item level has no lambda");
                return level.chain.finish(self.scope.context);
            };
            let body = level
                .chain
                .finish(self.scope.context)?
                .resolve()
                .map_err(Fail::Error)?;
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
                return error("expected local name");
            }
            if canonical_keyword(name).is_some() {
                return Err(Fail::Unsupported("a keyword lambda parameter"));
            }
            params.push((*name).to_owned());
            self.position += 1;
            self.skip_line_starts();
        }
        if params.is_empty() {
            return error("a lambda requires a parameter");
        }
        if !self.symbol_at(0, "->") {
            return error("expected `->` after lambda parameters");
        }
        self.position += 1;
        self.skip_line_starts();
        if self.peek().is_none() {
            return error("a lambda requires a body");
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
            if !(spaced && self.starts_atom_at(breaks)) {
                break;
            }
            self.position += breaks;
            if self.symbol_at(0, ".") {
                return error(
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
                None => return error("expected an expression"),
                Some(Piece::Group { .. }) => match self.shape_at(0) {
                    Some(Shape::Bracket { empty: false }) if tagged => {
                        tags.push(self.take_keys());
                        self.position += 1;
                    }
                    Some(Shape::Bracket { empty: true }) if tagged => {
                        return error("dictionary paths cannot be empty");
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
        if !self.joint_at(0) || self.data_path_end().is_none() {
            return error("expected a path after `:`");
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
            Some(_) => error("expected a dictionary path"),
            None => match self.kind() {
                Some(TokenKind::Name(name)) if is_glam_name(name) => self.named_path(name),
                _ => error("expected a dictionary path"),
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
        let base = match self.take_group() {
            Cover::Bracket(items) => SyntaxExpr::List(items),
            Cover::Brace(members) => SyntaxExpr::DictUnion(members),
            Cover::Paren(ParenCover::Unit) => SyntaxExpr::Unit,
            Cover::Paren(ParenCover::Grouped(expr) | ParenCover::Section(expr)) => expr,
            Cover::Paren(ParenCover::Tuple(items)) => SyntaxExpr::Tuple(items),
        };
        let suffixes = self.path_suffixes()?;
        Ok(access_if_path(base, suffixes))
    }

    fn base_atom(&mut self) -> Parse<SyntaxExpr> {
        let Some(kind) = self.kind() else {
            return error("expected an expression");
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
            _ => return error("expected an expression"),
        };
        let suffixes = self.path_suffixes()?;
        Ok(access_if_path(base, suffixes))
    }

    /// `'name` or `'.path`, with path suffixes.
    fn quoted(&mut self) -> Parse<SyntaxExpr> {
        self.position += 1;
        if !self.joint_at(0) {
            return error("expected a quoted name or path after `'`");
        }
        let base = if self.symbol_at(0, ".") {
            let suffixes = self.path_suffixes()?;
            if suffixes.is_empty() {
                return error("expected a quoted path after `'`");
            }
            quoted_path(suffixes)
        } else {
            match self.kind() {
                Some(TokenKind::Name(name)) if is_glam_name(name) => {
                    self.position += 1;
                    SyntaxExpr::Atom((*name).to_owned())
                }
                _ => return error("expected a quoted name or path after `'`"),
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
            return error("expected an effect name after `.`");
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
            return error("expected an escaped name or group");
        }
        let target = match self.peek() {
            Some(Piece::Group { .. }) if self.shape_at(0) == Some(Shape::Grouped) => {
                self.take_grouped()
            }
            Some(Piece::Token(_)) => match self.kind() {
                Some(TokenKind::Name(name)) => {
                    let name = validate_expr_name(name).map_err(Fail::Error)?;
                    self.position += 1;
                    let suffixes = self.path_suffixes()?;
                    access_if_path(SyntaxExpr::Name(name), suffixes)
                }
                _ => return error("expected an escaped name or group"),
            },
            _ => return error("expected an escaped name or group"),
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
            return error("expected name");
        };
        if !is_glam_name(prior) {
            return error("expected name after `_`");
        }
        if prior == "module_origin" {
            return error("`module_origin` is reserved");
        }
        let prior = validate_expr_name(prior).map_err(Fail::Error)?;
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
    let base = SyntaxExpr::Name(validate_expr_name(&name).map_err(Fail::Error)?);
    Ok(if path.is_empty() {
        base
    } else {
        SyntaxExpr::Access(Box::new(base), path)
    })
}

#[cfg(test)]
mod tests;
