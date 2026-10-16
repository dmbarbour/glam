//! The token pass: one walk over a view's tokens with an explicit stack of
//! frames.
//!
//! A delimiter group is a frame, and its contents are interpreted when it
//! closes. A conditional (`if`, `try`, or a postfix `if`) is a frame too.
//! Its own keywords switch its phase as the pass meets them at its level:
//! `then` ends a prefix conditional's guards and `else` a postfix one's, and
//! `else` ends a `then` branch. Its `else` branch is an open tail, which
//! ends at its group member's separator or close, at a keyword only an
//! enclosing conditional accepts, at a `->` where tails end there, or at the
//! end of the view. Closing interprets the conditional into one form piece,
//! as a group's close interprets its cover, so nested conditionals cost heap
//! frames, and each token is walked once.
//!
//! A conditional's guards are read in cover mode, and its clauses from their
//! pieces (`binding`). A braced `do` block is a frame too: a brace group
//! whose members, separated by `;`, are statements read from their pieces.
//! Each guard clause and statement takes its own context at its first token.
//!
//! Other keyword forms open a keyword region, whose groups are deferred for
//! the form's structural parser. Inside a region conditionals are only
//! counted, so that a `then` or `else` still reaches its own, and inside a
//! deferred group nothing is interpreted.
//!
//! A branch holding a form that view-level dispatch parses (`let`, `object`,
//! `where` or `with` at its level, or a leading `match` or `try_match`) is
//! parsed from its view, as the structural parser did, until those forms
//! are frames too: view-level dispatch reads them differently from the item
//! interpreter.

use crate::g_syntax::{ConditionalMode, ConditionalResultMode, IfExpr, SyntaxExpr};

use super::super::do_expr::{empty_do_expr, finish_do_statements};
use super::super::expression_context::{ExpressionContext, validate_expression_floor};
use super::super::input::{TokenRange, TokenView};
use super::super::layout::group_separator;
use super::super::lexical::{Delimiter, GroupId, LeadingTrivia, TokenKind};
use super::super::structural::{
    in_keyword_position, is_layout_empty, parse_expression_in_context, parse_open_tail, trim_layout,
};
use super::binding::{self, needs_view_dispatch};
use super::{
    Cover, Deferred, Fail, GroupBody, Items, Mode, Parse, Piece, Scope, interpret_group,
    may_head_keyword_form, opens_keyword_region, piece_token, regions_enabled, token, trim_padding,
};

/// One frame on the pass's stack.
struct Frame {
    kind: FrameKind,
    pieces: Vec<Piece>,
    /// The mode its groups are read in.
    mode: Mode,
    /// The context its members are read with.
    context: ExpressionContext,
    /// The separator of its innermost group, which ends its open tails.
    separator: Option<&'static str>,
    /// Whether a `->` at its level ends the open tails inside it.
    arrow_ends_tails: bool,
    /// Whether it lies in a keyword region of an enclosing frame.
    inherited_region: bool,
    /// Whether a keyword region is open among its pieces.
    region: bool,
    /// The conditionals headed inside its open region, innermost last.
    region_conditionals: Vec<Phase>,
    /// Whether its pieces since the last `\` are a lambda's parameters, so
    /// that a `->` is the lambda's own.
    lambda_head: bool,
    /// Its first significant token.
    first: Option<usize>,
    /// Line starts after its first significant token, nested frames
    /// included, so a frame without any skips checking its floors.
    line_starts: usize,
    /// Whether a form that view-level dispatch parses stands at a group's
    /// level: `let`, `object`, `where` or `with`, or a leading `match` or
    /// `try_match`.
    view_level: bool,
    /// Whether its next significant token starts a guard clause or a
    /// statement, which takes its own context.
    member_start: bool,
    /// A `do` whose `{` comes next, opening a block frame.
    pending_do: Option<usize>,
}

enum FrameKind {
    View,
    Group {
        delimiter: Delimiter,
        group: GroupId,
        /// The `do` block this brace group is, if any.
        block: Option<Box<Block>>,
    },
    Conditional(Box<Conditional>),
}

/// A braced `do` block: its head and its own context.
struct Block {
    head: usize,
    context: ExpressionContext,
}

impl Frame {
    fn new(
        kind: FrameKind,
        mode: Mode,
        context: ExpressionContext,
        separator: Option<&'static str>,
        arrow_ends_tails: bool,
        inherited_region: bool,
    ) -> Self {
        Self {
            kind,
            pieces: Vec::new(),
            mode,
            context,
            separator,
            arrow_ends_tails,
            inherited_region,
            region: false,
            region_conditionals: Vec::new(),
            lambda_head: false,
            first: None,
            line_starts: 0,
            view_level: false,
            member_start: false,
            pending_do: None,
        }
    }

    fn in_region(&self) -> bool {
        self.inherited_region || self.region
    }

    /// Whether a `->` ends the open tails of frames opened inside this one:
    /// in guards and statements it does, in a `then` branch it does not, and
    /// an `else` branch ends where the conditional itself does.
    fn nested_arrow(&self) -> bool {
        match &self.kind {
            FrameKind::Conditional(conditional) => match conditional.phase {
                Phase::Guards => true,
                Phase::Then => false,
                Phase::Else => self.arrow_ends_tails,
            },
            _ => self.arrow_ends_tails,
        }
    }

    /// Starts a branch: its pieces, region and line facts begin afresh, and
    /// it reads as an expression.
    fn begin_branch(&mut self, context: ExpressionContext) {
        self.pieces.clear();
        self.mode = Mode::Expression;
        self.context = context;
        self.region = false;
        self.region_conditionals.clear();
        self.lambda_head = false;
        self.first = None;
        self.line_starts = 0;
        self.view_level = false;
    }
}

/// The phase a conditional is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Guards,
    Then,
    Else,
}

struct Conditional {
    head: usize,
    keyword: &'static str,
    phase: Phase,
    /// The `then`, and whether it is `then?`.
    then: Option<(usize, bool)>,
    /// The `else`, which ends a prefix conditional's `then` branch or a
    /// postfix one's guards.
    else_keyword: Option<usize>,
    then_branch: Option<Branch>,
    /// The guards' pieces, once a `then` or `else` ends them.
    guard_pieces: Vec<Piece>,
    /// The enclosing frame's context, from which the conditional's own
    /// derives.
    enclosing: ExpressionContext,
    /// The current branch's context, before its floors are checked.
    branch_context: Option<ExpressionContext>,
    /// The first error found while its phases were switched.
    failure: Option<Fail>,
    /// Line starts in its finished phases, which its parent counts too.
    line_starts: usize,
}

impl Conditional {
    fn postfix(&self) -> bool {
        self.then.is_none() && self.else_keyword.is_some()
    }

    /// The conditional's own context: a prefix form's is floored at its
    /// head's line, as for every delegated keyword form.
    fn context(&self, view: TokenView<'_, '_>) -> ExpressionContext {
        let context = if self.postfix() {
            self.enclosing
        } else {
            view.line_indentation_at(self.head)
                .map_or(self.enclosing, |indentation| {
                    self.enclosing.with_physical_line_floor(indentation)
                })
        };
        context.may_yield()
    }

    fn fail(&mut self, at: usize, message: impl Into<String>) {
        self.failure.get_or_insert(Fail::Error {
            at: Some(at),
            message: message.into(),
        });
    }
}

/// A finished `then` branch.
struct Branch {
    pieces: Vec<Piece>,
    context: Option<ExpressionContext>,
    start: usize,
    end: usize,
    line_starts: usize,
}

/// A conditional's or a `do` block's interpretation, held in the arena
/// until consumed.
pub(super) struct Form {
    pub(super) head: usize,
    pub(super) keyword: &'static str,
    pub(super) shape: FormShape,
    pub(super) result: Parse<SyntaxExpr>,
}

/// How a form reads, which its place among the pieces confirms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FormShape {
    /// `if Guards then A else B`, or a `do` block: an atom.
    Prefix,
    /// `if Guards else B`: a postfix conditional on the operand before it.
    Postfix,
    /// Closed in its guards: an error whose wording depends on its place.
    Unfinished,
}

enum Step {
    Next,
    /// Skip the next token too, as `then?` does its `?`.
    Skip,
    Stop,
}

/// Walks `scope`'s view once, returning the view's top-level pieces and the
/// token where the walk ended. An expression whose tails end at `->` ends
/// at a top-level one that no lambda head or keyword region owns.
pub(super) fn top_pieces(scope: Scope<'_, '_, '_>) -> Parse<(Vec<Piece>, usize)> {
    let view = scope.view;
    super::prescan(view)?;
    let mut pass = Pass {
        scope,
        stack: vec![Frame::new(
            FrameKind::View,
            scope.mode,
            scope.context,
            None,
            scope.context.arrow_ends_tail() || scope.mode == Mode::Cover,
            false,
        )],
        // Only regions mark the keyword forms that are not frames, so the
        // pass stops early only with them; the item stops at the same `->`.
        stops_at_arrow: scope.mode == Mode::Expression
            && scope.context.arrow_ends_tail()
            && regions_enabled(),
    };
    let range = view.range();
    let mut index = range.start();
    while index < range.end() {
        match token(view, index).kind() {
            TokenKind::Open { group, delimiter } => pass.open_group(index, *group, *delimiter)?,
            TokenKind::Close { .. } => pass.close_group(index),
            _ => match pass.token(index) {
                Step::Next => {}
                Step::Skip => index += 1,
                Step::Stop => break,
            },
        }
        index += 1;
    }
    pass.close_conditionals(index);
    let top = pass.stack.pop().expect("the view frame remains");
    debug_assert!(pass.stack.is_empty(), "every group closes inside the view");
    Ok((top.pieces, index))
}

struct Pass<'s, 'lex, 'source> {
    scope: Scope<'s, 'lex, 'source>,
    stack: Vec<Frame>,
    stops_at_arrow: bool,
}

impl Pass<'_, '_, '_> {
    fn top(&mut self) -> &mut Frame {
        self.stack
            .last_mut()
            .expect("the view frame is never popped")
    }

    fn open_group(&mut self, index: usize, group: GroupId, delimiter: Delimiter) -> Parse<()> {
        let view = self.scope.view;
        view.group(group)
            .and_then(|group| group.close_token())
            .filter(|close| *close < view.range().end())
            .ok_or(Fail::Unsupported("a delimiter group crossing the view"))?;
        let pending = self.top().pending_do.take();
        self.significant(index);
        let parent = self.top();
        parent.lambda_head = false;
        let (mode, context, inherited) = (parent.mode, parent.context, parent.in_region());
        // A `do` block's statements may bind patterns, and end their tails
        // at `->`; its context is the `do` form's, floored at its head.
        let block = pending
            .filter(|_| delimiter == Delimiter::Brace)
            .map(|head| {
                Box::new(Block {
                    head,
                    context: view
                        .line_indentation_at(head)
                        .map_or(context, |indentation| {
                            context.with_physical_line_floor(indentation)
                        })
                        .may_yield(),
                })
            });
        let (mode, arrow) = if block.is_some() {
            (Mode::Cover, true)
        } else {
            (mode, mode == Mode::Cover)
        };
        self.stack.push(Frame::new(
            FrameKind::Group {
                delimiter,
                group,
                block,
            },
            mode,
            context,
            Some(group_separator(view.source(), delimiter, index)),
            arrow,
            inherited,
        ));
        Ok(())
    }

    fn close_group(&mut self, index: usize) {
        self.close_conditionals(index);
        let frame = self.stack.pop().expect("a close token ends an open frame");
        let FrameKind::Group {
            delimiter,
            group,
            block,
        } = frame.kind
        else {
            unreachable!("conditionals close before their group");
        };
        let view = self.scope.view;
        let open = view
            .group(group)
            .expect("an open frame's group exists")
            .open_token();
        let view_level = frame.view_level;
        let line_starts = frame.line_starts;
        if let Some(block) = block {
            let result = self.finish_block(&block, &frame.pieces, open, index);
            let covers = self.scope.covers;
            let parent = self.top();
            parent.line_starts += line_starts;
            parent.lambda_head = false;
            // The form replaces its `do` and the line starts after it.
            while let Some(Piece::Token(last)) = parent.pieces.last()
                && *last != block.head
            {
                parent.pieces.pop();
            }
            parent.pieces.pop();
            parent.pieces.push(Piece::Form {
                head: block.head,
                end: index + 1,
                form: covers.form(Form {
                    head: block.head,
                    keyword: "do",
                    shape: FormShape::Prefix,
                    result,
                }),
            });
            return;
        }
        let body = GroupBody {
            delimiter,
            group,
            pieces: frame.pieces,
            mode: frame.mode,
            context: frame.context,
        };
        let scope = Scope {
            mode: body.mode,
            context: body.context,
            ..self.scope
        };
        let covers = self.scope.covers;
        let parent = self.top();
        parent.line_starts += line_starts;
        let cover = if parent.in_region() {
            covers.defer(Deferred {
                body,
                open,
                close: index,
                in_parent_region: parent.region,
            })
        } else {
            covers.ready(
                interpret_group(scope, &body, index)
                    .unwrap_or_else(|fail| Cover::Invalid(fail.or_at(Some(open)))),
            )
        };
        parent.pieces.push(Piece::Group {
            open,
            close: index,
            cover,
            view_level,
        });
    }

    /// Notes a significant token in the top frame. One that starts a branch,
    /// a guard clause or a statement starts that member's context.
    fn significant(&mut self, index: usize) {
        let view = self.scope.view;
        let top = self.top();
        let starting = top.first.is_none() || top.member_start;
        top.first.get_or_insert(index);
        if !starting {
            return;
        }
        top.member_start = false;
        match &mut top.kind {
            FrameKind::Conditional(conditional) => match conditional.phase {
                Phase::Guards => {
                    top.context = ExpressionContext::for_owner_at(view, index).validated()
                }
                phase => {
                    let owner = conditional.context(view).child_owner_at(view, index);
                    let owner = if phase == Phase::Else {
                        owner.complete_keeping_arrow()
                    } else {
                        owner.complete()
                    };
                    conditional.branch_context = Some(owner);
                    top.context = owner.validated();
                }
            },
            FrameKind::Group {
                block: Some(block), ..
            } => top.context = block.context.child_owner_at(view, index).validated(),
            _ => {}
        }
    }

    fn token(&mut self, index: usize) -> Step {
        let view = self.scope.view;
        let kind = token(view, index).kind();
        match kind {
            TokenKind::LineStart { .. } => {
                let top = self.top();
                if top.first.is_some() {
                    top.line_starts += 1;
                }
                top.pieces.push(Piece::Token(index));
                return Step::Next;
            }
            TokenKind::Symbol(symbol)
                if self
                    .top()
                    .separator
                    .is_some_and(|separator| separator == *symbol) =>
            {
                // A separator ends the conditionals and the region before it,
                // and the next member starts its own context.
                self.close_conditionals(index);
                self.significant(index);
                let top = self.top();
                top.region = false;
                top.region_conditionals.clear();
                top.lambda_head = false;
                top.member_start = true;
                top.pieces.push(Piece::Token(index));
                return Step::Next;
            }
            _ => {}
        }
        // Nothing in a deferred group is interpreted here, but its line
        // starts still count toward the floors around it.
        let top = self.top();
        if top.inherited_region {
            top.first.get_or_insert(index);
            top.pieces.push(Piece::Token(index));
            return Step::Next;
        }
        self.significant(index);
        if self.top().region {
            return self.region_token(index);
        }
        match kind {
            // Only a keyword's position needs judging.
            TokenKind::Name(name) => match *name {
                "then" | "else" if in_keyword_position(view, index) => {
                    return self.phase_keyword(index);
                }
                // A form opens where the term parser always delegated one,
                // a tag's payload included.
                "if" | "try" if may_head_keyword_form(view, index) => {
                    self.open_conditional(index);
                    return Step::Next;
                }
                "do" if may_head_keyword_form(view, index) && self.braced_do(index) => {
                    let top = self.top();
                    top.pending_do = Some(index);
                    top.lambda_head = false;
                    top.pieces.push(Piece::Token(index));
                    return Step::Next;
                }
                // An `and` in guards ends a clause.
                "and"
                    if in_keyword_position(view, index)
                        && matches!(
                            &self.top().kind,
                            FrameKind::Conditional(conditional)
                                if conditional.phase == Phase::Guards
                        ) =>
                {
                    let top = self.top();
                    top.lambda_head = false;
                    top.member_start = true;
                    top.pieces.push(Piece::Token(index));
                    return Step::Next;
                }
                "let" | "object" | "where" | "with" if in_keyword_position(view, index) => {
                    self.top().view_level = true;
                }
                "match" | "try_match"
                    if self.top().first == Some(index) && in_keyword_position(view, index) =>
                {
                    self.top().view_level = true;
                }
                _ => {}
            },
            TokenKind::Symbol("->") if !self.top().lambda_head => {
                // With regions off, a keyword form's own arrows are not hidden.
                if regions_enabled() {
                    self.close_tails_at_arrow(index);
                }
                if self.stops_at_arrow && self.stack.len() == 1 && !self.stack[0].region {
                    return Step::Stop;
                }
            }
            _ => {}
        }
        let top = self.top();
        if matches!(kind, TokenKind::Name(_)) && opens_keyword_region(view, index, top.mode) {
            top.region = true;
        }
        top.lambda_head = match kind {
            TokenKind::Symbol("\\") => true,
            TokenKind::Name(_) => top.lambda_head,
            _ => false,
        };
        top.pieces.push(Piece::Token(index));
        Step::Next
    }

    /// Whether the `do` at `index` heads a braced block: its next significant
    /// token is a `{`, separated from it as the `do` form requires.
    fn braced_do(&self, index: usize) -> bool {
        let view = self.scope.view;
        let mut next = index + 1;
        while view
            .token_at(next)
            .is_some_and(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
        {
            next += 1;
        }
        view.token_at(next).is_some_and(|token| {
            matches!(
                token.kind(),
                TokenKind::Open {
                    delimiter: Delimiter::Brace,
                    ..
                }
            ) && (next > index + 1 || token.leading() != LeadingTrivia::Joint)
        })
    }

    /// A token inside the top frame's keyword region: only its end, and the
    /// conditionals counted inside it, matter. Separators are handled before.
    fn region_token(&mut self, index: usize) -> Step {
        let view = self.scope.view;
        let kind = token(view, index).kind();
        let top = self.top();
        let is_conditional = matches!(top.kind, FrameKind::Conditional(_));
        match kind {
            TokenKind::Name("if" | "try") if may_head_keyword_form(view, index) => {
                top.region_conditionals.push(Phase::Guards);
            }
            TokenKind::Name("then") if in_keyword_position(view, index) => {
                match top.region_conditionals.last_mut() {
                    Some(phase @ Phase::Guards) => *phase = Phase::Then,
                    Some(_) => {}
                    None if is_conditional => {
                        top.region = false;
                        return self.phase_keyword(index);
                    }
                    None => {}
                }
            }
            // A counted conditional takes its `else`; otherwise the frame does.
            TokenKind::Name("else")
                if in_keyword_position(view, index)
                    && top.region_conditionals.pop().is_none()
                    && is_conditional =>
            {
                top.region = false;
                return self.phase_keyword(index);
            }
            _ => {}
        }
        top.pieces.push(Piece::Token(index));
        Step::Next
    }

    fn open_conditional(&mut self, index: usize) {
        let view = self.scope.view;
        let keyword = match token(view, index).kind() {
            TokenKind::Name("try") => "try",
            _ => "if",
        };
        let parent = self.top();
        parent.lambda_head = false;
        let (context, separator, arrow) = (parent.context, parent.separator, parent.nested_arrow());
        let mut frame = Frame::new(
            FrameKind::Conditional(Box::new(Conditional {
                head: index,
                keyword,
                phase: Phase::Guards,
                then: None,
                else_keyword: None,
                then_branch: None,
                guard_pieces: Vec::new(),
                enclosing: context,
                branch_context: None,
                failure: None,
                line_starts: 0,
            })),
            // Guards may bind patterns.
            Mode::Cover,
            context,
            separator,
            arrow,
            false,
        );
        frame.first = Some(index);
        frame.member_start = true;
        self.stack.push(frame);
    }

    /// A `then` or `else` in keyword position, for the innermost conditional
    /// that accepts it; a finished conditional's open tail ends at it.
    fn phase_keyword(&mut self, index: usize) -> Step {
        let view = self.scope.view;
        let is_then = matches!(token(view, index).kind(), TokenKind::Name("then"));
        loop {
            let top = self.top();
            let FrameKind::Conditional(conditional) = &mut top.kind else {
                // A stray keyword, which the item reports.
                top.lambda_head = false;
                top.pieces.push(Piece::Token(index));
                return Step::Next;
            };
            match (conditional.phase, is_then) {
                (Phase::Guards, true) => {
                    let tentative = view.token_at(index + 1).is_some_and(|next| {
                        next.leading() == LeadingTrivia::Joint
                            && matches!(next.kind(), TokenKind::Symbol("?"))
                    });
                    conditional.then = Some((index, tentative));
                    conditional.phase = Phase::Then;
                    conditional.line_starts += top.line_starts;
                    conditional.guard_pieces = std::mem::take(&mut top.pieces);
                    let context = conditional.enclosing;
                    top.begin_branch(context);
                    return if tentative { Step::Skip } else { Step::Next };
                }
                (Phase::Guards, false) => {
                    if conditional.keyword == "try" {
                        conditional.fail(index, "try expression requires `then` before `else`");
                    }
                    conditional.else_keyword = Some(index);
                    conditional.phase = Phase::Else;
                    conditional.line_starts += top.line_starts;
                    conditional.guard_pieces = std::mem::take(&mut top.pieces);
                    let context = conditional.enclosing;
                    top.begin_branch(context);
                    return Step::Next;
                }
                (Phase::Then, false) => {
                    let (then, tentative) = conditional.then.expect("a `then` began the branch");
                    conditional.then_branch = Some(Branch {
                        pieces: std::mem::take(&mut top.pieces),
                        context: conditional.branch_context.take(),
                        start: then + 1 + usize::from(tentative),
                        end: index,
                        line_starts: top.line_starts,
                    });
                    conditional.else_keyword = Some(index);
                    conditional.phase = Phase::Else;
                    conditional.line_starts += top.line_starts;
                    let context = conditional.enclosing;
                    top.begin_branch(context);
                    return Step::Next;
                }
                (Phase::Then, true) => {
                    let message = format!(
                        "unexpected `then`; a {} branch is already active",
                        conditional.keyword
                    );
                    conditional.fail(index, message);
                    return Step::Next;
                }
                (Phase::Else, _) => self.close_conditional(index),
            }
        }
    }

    /// Closes conditionals whose `else` branch ends at the `->` at `index`.
    fn close_tails_at_arrow(&mut self, index: usize) {
        while let Some(top) = self.stack.last()
            && top.arrow_ends_tails
            && matches!(&top.kind, FrameKind::Conditional(conditional) if conditional.phase == Phase::Else)
        {
            self.close_conditional(index);
        }
    }

    /// Closes every conditional above the innermost group or the view.
    fn close_conditionals(&mut self, index: usize) {
        while matches!(
            self.stack.last().map(|frame| &frame.kind),
            Some(FrameKind::Conditional(_))
        ) {
            self.close_conditional(index);
        }
    }

    /// Closes the conditional on top, which ends before `index`, into a
    /// form piece of its parent.
    fn close_conditional(&mut self, index: usize) {
        let frame = self.stack.pop().expect("a conditional is open");
        let line_starts = frame.line_starts
            + match &frame.kind {
                FrameKind::Conditional(conditional) => conditional.line_starts,
                _ => 0,
            };
        let (form, rest) = self.finish(frame, index);
        let head = form.head;
        let end = rest.first().map_or(index, piece_token);
        let covers = self.scope.covers;
        let parent = self.top();
        parent.line_starts += line_starts;
        parent.lambda_head = false;
        parent.pieces.push(Piece::Form {
            head,
            end,
            form: covers.form(form),
        });
        parent.pieces.extend(rest);
    }

    /// Interprets a closed conditional, returning its form and any pieces
    /// of its `else` branch after the `->` where that branch ended.
    fn finish(&self, frame: Frame, end: usize) -> (Form, Vec<Piece>) {
        let Frame {
            kind: FrameKind::Conditional(mut conditional),
            pieces,
            line_starts,
            ..
        } = frame
        else {
            unreachable!("only conditionals finish");
        };
        let shape = if conditional.then.is_some() {
            FormShape::Prefix
        } else if conditional.else_keyword.is_some() {
            FormShape::Postfix
        } else {
            FormShape::Unfinished
        };
        let mut form = Form {
            head: conditional.head,
            keyword: conditional.keyword,
            shape,
            result: Err(Fail::error("an unfinished conditional")),
        };
        let else_branch = Branch {
            pieces,
            context: None,
            start: conditional.else_keyword.map_or(end, |keyword| keyword + 1),
            end,
            line_starts,
        };
        match self.interpret(&mut conditional, else_branch) {
            Ok((expression, rest)) => {
                form.result = Ok(expression);
                (form, rest)
            }
            Err(Some(fail)) => {
                form.result = Err(fail);
                (form, Vec::new())
            }
            // Closed in its guards: the item words the error by its place.
            Err(None) => (form, Vec::new()),
        }
    }

    fn interpret(
        &self,
        conditional: &mut Conditional,
        mut else_branch: Branch,
    ) -> Result<(SyntaxExpr, Vec<Piece>), Option<Fail>> {
        if let Some(failure) = conditional.failure.take() {
            return Err(Some(failure));
        }
        if conditional.phase == Phase::Guards {
            return Err(None);
        }
        let view = self.scope.view;
        let (head, keyword) = (conditional.head, conditional.keyword);
        let fail = |at: usize, message: String| Err(Some(Fail::error(message).or_at(Some(at))));
        let Some(else_index) = conditional.else_keyword else {
            return fail(
                head,
                format!("{keyword} expression requires `else` and a fallback result"),
            );
        };
        let postfix = conditional.postfix();
        let guards_end = conditional.then.map_or(else_index, |(then, _)| then);
        let guards = trim_layout(subview(view, head + 1, guards_end));
        if is_layout_empty(guards) {
            return fail(
                head,
                if postfix {
                    "postfix `if` requires guards before `else`".to_owned()
                } else {
                    format!("{keyword} expression requires a guard before `then`")
                },
            );
        }
        if let (Some((then, tentative)), Some(branch)) =
            (conditional.then, &conditional.then_branch)
            && trim_padding(view, &branch.pieces).is_empty()
        {
            let spelling = if tentative { "then?" } else { "then" };
            return fail(
                then,
                format!("{keyword} expression requires a result after `{spelling}`"),
            );
        }
        if trim_padding(view, &else_branch.pieces).is_empty() {
            return fail(
                else_index,
                if postfix {
                    "postfix `if` requires a fallback after `else`".to_owned()
                } else {
                    format!("{keyword} expression requires a fallback after `else`")
                },
            );
        }
        let construct = if postfix {
            "if guard".to_owned()
        } else {
            format!("{keyword} guard")
        };
        let scope = Scope {
            view: subview(view, head + 1, guards_end),
            mode: Mode::Cover,
            ..self.scope
        };
        let guards =
            binding::guard_clauses(scope, &conditional.guard_pieces, &construct).map_err(Some)?;
        let context = conditional.context(view);
        let then_result = conditional
            .then_branch
            .take()
            .map(|branch| self.then_branch(branch, context))
            .transpose()
            .map_err(Some)?;
        else_branch.context = conditional.branch_context.take();
        let (else_result, rest) = self.else_branch(else_branch, context).map_err(Some)?;
        let then_mode = match conditional.then {
            Some((_, true)) => ConditionalResultMode::Tentative,
            _ => ConditionalResultMode::Ordinary,
        };
        let expression = SyntaxExpr::If(IfExpr {
            mode: if keyword == "try" {
                ConditionalMode::Host
            } else {
                ConditionalMode::Pure
            },
            guards,
            then_mode,
            // A postfix conditional's result is the operand before it, which
            // the item fills in.
            then_result: Box::new(then_result.unwrap_or(SyntaxExpr::Unit)),
            else_result: Box::new(else_result),
        });
        Ok((expression, rest))
    }

    /// A `then` branch, which runs to its `else`.
    fn then_branch(&self, branch: Branch, conditional: ExpressionContext) -> Parse<SyntaxExpr> {
        let view = self.scope.view;
        let whole = subview(view, branch.start, branch.end);
        let trimmed = trim_layout(whole);
        if needs_view_dispatch(view, &branch.pieces) {
            return parse_expression_in_context(trimmed, conditional.child_owner(trimmed))
                .map_err(Fail::Reported);
        }
        let context = self.checked(trimmed, &branch)?;
        let scope = Scope {
            view: whole,
            context,
            mode: Mode::Expression,
            ..self.scope
        };
        let at = trimmed.first_significant().map(|(index, _)| index);
        Items::new(scope, trim_padding(view, &branch.pieces))
            .item()?
            .resolve()
            .map_err(|message| Fail::error(message).or_at(at))
    }

    /// An `else` branch, an open tail, with any pieces after a `->` where it
    /// ended.
    fn else_branch(
        &self,
        branch: Branch,
        conditional: ExpressionContext,
    ) -> Parse<(SyntaxExpr, Vec<Piece>)> {
        let view = self.scope.view;
        let whole = subview(view, branch.start, branch.end);
        let trimmed = trim_layout(whole);
        let mut pieces = branch.pieces;
        if needs_view_dispatch(view, &pieces) {
            let (expression, end) =
                parse_open_tail(trimmed, conditional.child_owner(trimmed), branch.end)
                    .map_err(Fail::Reported)?;
            let split = pieces
                .iter()
                .position(|piece| piece_token(piece) >= end)
                .filter(|_| end < trimmed.range().end())
                .unwrap_or(pieces.len());
            return Ok((expression, pieces.split_off(split)));
        }
        let context = self.checked(
            trimmed,
            &Branch {
                pieces: Vec::new(),
                ..branch
            },
        )?;
        let scope = Scope {
            view: whole,
            context,
            mode: Mode::Expression,
            ..self.scope
        };
        let leading = pieces.len() - leading_trimmed(view, &pieces).len();
        let trimmed_pieces = trim_padding(view, &pieces);
        let mut items = Items::new(scope, trimmed_pieces);
        items.stop_at_arrow = context.arrow_ends_tail();
        let at = trimmed.first_significant().map(|(index, _)| index);
        let (chain, stop) = items.expression()?;
        let expression = chain
            .resolve()
            .map_err(|message| Fail::error(message).or_at(at))?;
        let rest = match stop {
            Some(position) => pieces.split_off(leading + position),
            None => Vec::new(),
        };
        Ok((expression, rest))
    }

    /// A braced `do` block's statements, split at its `;`s: one leading and
    /// one trailing `;` are allowed, and `do {}` is `.r ()`.
    fn finish_block(
        &self,
        block: &Block,
        pieces: &[Piece],
        open: usize,
        close: usize,
    ) -> Parse<SyntaxExpr> {
        let view = self.scope.view;
        let head = block.head;
        let fail = |message: String| Err(Fail::error(message).or_at(Some(head)));
        if trim_padding(view, pieces).is_empty() {
            let line = view
                .token_at(head)
                .and_then(|token| view.line_at_span(token.span()))
                .unwrap_or(1);
            return Ok(empty_do_expr(line));
        }
        let is_semicolon = |piece: &Piece| {
            matches!(piece, Piece::Token(index)
                if matches!(token(view, *index).kind(), TokenKind::Symbol(";")))
        };
        let mut statements = pieces.split(is_semicolon).collect::<Vec<_>>();
        if statements
            .first()
            .is_some_and(|statement| trim_padding(view, statement).is_empty())
        {
            statements.remove(0);
        }
        if statements
            .last()
            .is_some_and(|statement| trim_padding(view, statement).is_empty())
        {
            statements.pop();
        }
        if statements.is_empty() {
            return fail("`do {;}` is invalid; a semicolon is not an empty computation".to_owned());
        }
        let scope = Scope {
            view: subview(view, open + 1, close),
            mode: Mode::Cover,
            ..self.scope
        };
        let mut parsed = Vec::with_capacity(statements.len());
        for statement in statements {
            let statement = trim_padding(view, statement);
            let Some(first) = statement.first().map(piece_token) else {
                return fail(
                    "braced do block contains an empty statement between semicolons".to_owned(),
                );
            };
            let line = view
                .token_at(first)
                .and_then(|token| view.line_at_span(token.span()))
                .unwrap_or(1);
            let context = block.context.child_owner_at(view, first);
            parsed.push((line, binding::statement(scope, statement, context)?));
        }
        finish_do_statements(parsed).or_else(fail)
    }

    /// A branch's context, its floors checked where it spans lines.
    fn checked(&self, view: TokenView<'_, '_>, branch: &Branch) -> Parse<ExpressionContext> {
        let owner = branch
            .context
            .expect("a branch with a significant token has a context");
        if branch.line_starts == 0 {
            return Ok(owner.validated());
        }
        let (context, diagnostics) = validate_expression_floor(view, owner);
        if diagnostics.is_empty() {
            Ok(context)
        } else {
            Err(Fail::Reported(diagnostics))
        }
    }
}

/// The pieces after any leading line starts.
fn leading_trimmed<'a>(view: TokenView<'_, '_>, mut pieces: &'a [Piece]) -> &'a [Piece] {
    while let Some((Piece::Token(index), rest)) = pieces.split_first()
        && matches!(token(view, *index).kind(), TokenKind::LineStart { .. })
    {
        pieces = rest;
    }
    pieces
}

fn subview<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    start: usize,
    end: usize,
) -> TokenView<'lex, 'source> {
    view.subview(TokenRange::new(start, end).expect("ordered token indices form a range"))
        .expect("a conditional's parts lie in its view")
}
