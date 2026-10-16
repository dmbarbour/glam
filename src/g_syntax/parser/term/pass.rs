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
//! A layout `do` block is a frame whose lines the pass judges as the lexer
//! records them, after Haskell's layout algorithm. Its first statement sets
//! its anchor: that token's column on the `do`'s own line (a hanging block),
//! or its line's indentation. A line at the anchor starts a statement, a
//! deeper one continues it, and a shallower one closes the block, as does
//! anything that ends an open tail. A line holding only closing delimiters
//! stays with its statement. Each line start is judged by its nearest layout
//! owner (a group, a layout block, or the view), which every frame records,
//! so judging costs nothing per enclosing conditional.
//!
//! A match (`match`, `try_match`, with or without `*`) is a frame: its
//! subject reads as an expression up to a `with`, which belongs to the
//! innermost match reading its subject at its level, and its arms are a
//! block, braced or layout. While an arm reads its head or guards, its `=>`,
//! `=>?` and `when` close the frames opened in it: an arrow begins its
//! result, and a `when` its guards, or nested arms when no mark follows it
//! on its line. Each frame records the match and the arms block whose
//! keywords it may meet, as it records its layout owner. An arm's pieces are
//! read when its block closes (`binding`).
//!
//! Other keyword forms open a keyword region, whose groups are deferred for
//! the form's structural parser. Inside a region conditionals are only
//! counted, so that a `then` or `else` still reaches its own, and inside a
//! deferred group nothing is interpreted.
//!
//! A branch holding a form that view-level dispatch parses (`let`, `object`,
//! `where`, or a `with` that no match takes, at its level) is parsed from its
//! view, as the structural parser did, until those forms are frames too:
//! view-level dispatch reads them differently from the item interpreter.

use crate::g_syntax::{
    ConditionalMode, ConditionalResultMode, IfExpr, MatchArm, MatchCommitment, MatchExpr,
    MatchWhenExpr, SyntaxExpr, WhenArm,
};

use super::super::do_expr::{empty_do_expr, finish_do_statements, line_begins_do_statement};
use super::super::expression::structural_view_at;
use super::super::expression_context::{ExpressionContext, validate_expression_floor};
use super::super::input::{TokenRange, TokenView};
use super::super::layout::group_separator;
use super::super::lexical::{Delimiter, GroupId, LeadingTrivia, TokenKind};
use super::super::structural::{
    in_keyword_position, is_layout_empty, parse_expression_in_context, parse_open_tail, trim_layout,
};
use super::binding::{self, NestedArms, needs_view_dispatch};
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
    /// level: `let`, `object`, `where`, or a `with` no match takes.
    view_level: bool,
    /// Whether its next significant token starts a member: a guard clause,
    /// a statement, or an arm or its result, which takes its own context.
    member_start: bool,
    /// A block whose `{` comes next.
    pending: Option<Pending>,
    /// The stack index of its nearest layout owner: itself for a group, a
    /// layout block or the view, its parent's for a conditional or a match.
    layout_owner: usize,
    /// The stack index of the match whose `with` it may meet: the innermost
    /// match reading its subject, unless a group or arms block lies between.
    match_owner: Option<usize>,
    /// The stack index of the arms block whose arms' marks it may meet: the
    /// innermost, unless a group lies between.
    arms_owner: Option<usize>,
    /// The stack index of the group or view whose depth its tokens are at.
    level: usize,
    /// For a group or the view, the last scan along a line for arm marks.
    mark_scan: Option<MarkScan>,
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
    Layout(Box<LayoutBlock>),
    Match(Box<Match>),
}

/// A layout block: a `do` block's statements, or a match's arms.
struct LayoutBlock {
    /// The `do`, or the `with` or `when` its arms follow.
    head: usize,
    /// The block's context: a `do` form's, floored at its head's line, or
    /// its arms'.
    context: ExpressionContext,
    /// The indentation its statements align at, once its first sets it.
    anchor: Option<usize>,
    hanging: bool,
    /// The piece positions where its statements begin.
    statements: Vec<usize>,
    /// A hanging block's deeper lines, which must not begin a statement.
    hanging_lines: Vec<usize>,
    /// The line where a hanging block ended, which must not either.
    boundary: Option<usize>,
    failure: Option<Fail>,
    /// Its arms, or none for a `do` block.
    arms: Option<Box<Arms>>,
}

/// A braced block: a `do` block's head and its own context, or a match's
/// arms.
struct Block {
    head: usize,
    context: ExpressionContext,
    arms: Option<Box<Arms>>,
}

/// A block whose `{` comes next.
enum Pending {
    /// A `do`'s statements, after the `do` at this index.
    Do(usize),
    Arms(Box<Arms>),
}

/// A `match` or `try_match` form.
struct Match {
    head: usize,
    keyword: &'static str,
    /// Whether it is `match*` or `try_match*`.
    open: bool,
    phase: MatchPhase,
    /// The form's own context, floored at its head's line.
    context: ExpressionContext,
    /// Its subject, once its `with` ends it; a `match when` has none.
    subject: Option<Parse<SyntaxExpr>>,
    failure: Option<Fail>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchPhase {
    /// Before its first significant token, which a `match when` begins.
    Head,
    Subject,
    /// After its `with` or `when`: its arms are a block above it, or a
    /// braced block that comes next.
    Arms,
}

/// A block of arms: a match's, a `match when`'s, or those nested under an
/// arm's trailing `when`. Each arm's pieces are read when the block closes.
struct Arms {
    /// The match's head, or the trailing `when` that heads nested arms.
    head: usize,
    kind: ArmsKind,
    /// The context the arms are read with.
    context: ExpressionContext,
    /// Where the pass stands in the current arm.
    phase: ArmPhase,
    arms: Vec<ArmRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArmsKind {
    /// A match's arms: a pattern, an optional guard, and an outcome.
    Match { keyword: &'static str, open: bool },
    /// A `match when`'s arms: a guard and an outcome.
    When { keyword: &'static str, open: bool },
    /// Arms nested under a trailing `when`, read as `match when`'s.
    Nested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArmPhase {
    Head,
    /// After the `when` that guards a match arm's pattern.
    Guards,
    /// After the arm's `=>` or `=>?`.
    Result,
}

struct ArmRecord {
    /// The piece position where the arm begins.
    start: usize,
    /// Its first significant token.
    first: Option<usize>,
    nested: Option<NestedArms>,
    failure: Option<Fail>,
}

/// A block's arms, read.
enum ArmList {
    Match(Vec<MatchArm>),
    When(Vec<WhenArm>),
}

/// A scan along a line for arm marks (see `Pass::line_mark`): from `from`,
/// the first mark or end of the line at the scan's depth is at `to`.
#[derive(Clone, Copy)]
struct MarkScan {
    from: usize,
    to: usize,
    mark: bool,
}

impl Arms {
    fn new(head: usize, kind: ArmsKind, context: ExpressionContext) -> Self {
        Self {
            head,
            kind,
            context,
            phase: ArmPhase::Head,
            arms: Vec::new(),
        }
    }

    fn begin(&mut self, start: usize) {
        self.phase = ArmPhase::Head;
        self.arms.push(ArmRecord {
            start,
            first: None,
            nested: None,
            failure: None,
        });
    }

    /// The context of a member starting at `index`: an arm's head and its
    /// result take the arms' context, and a guard clause its own.
    fn member_context(&mut self, view: TokenView<'_, '_>, index: usize) -> ExpressionContext {
        match self.phase {
            ArmPhase::Guards => ExpressionContext::for_owner_at(view, index).validated(),
            phase => {
                if phase == ArmPhase::Head
                    && let Some(arm) = self.arms.last_mut()
                {
                    arm.first.get_or_insert(index);
                }
                self.context.child_owner_at(view, index).validated()
            }
        }
    }

    /// Whether a `{` may begin the first arm, as a dict pattern does; nested
    /// arms after a `{` are always braced.
    fn brace_may_begin_arm(&self) -> bool {
        self.kind != ArmsKind::Nested
    }

    fn arm_label(&self) -> &'static str {
        match self.kind {
            ArmsKind::Match { .. } => "match arm",
            _ => "match-when arm",
        }
    }

    fn body_label(&self) -> String {
        match self.kind {
            ArmsKind::Match { keyword, open } => format!("{} body", display(keyword, open)),
            ArmsKind::When { keyword, open } => format!("{}-when body", display(keyword, open)),
            ArmsKind::Nested => "nested match-when body".to_owned(),
        }
    }

    /// The error for a block without arms.
    fn missing(&self) -> Fail {
        let message = match self.kind {
            ArmsKind::Match { keyword, open } => format!(
                "layout {} expression requires at least one arm",
                display(keyword, open)
            ),
            ArmsKind::When { keyword, open } => format!(
                "layout `{} when` requires at least one arm",
                display(keyword, open)
            ),
            ArmsKind::Nested => "nested `when` requires at least one child arm".to_owned(),
        };
        Fail::error(message).or_at(Some(self.head))
    }

    /// Records that a trailing `when` heads no block, if `pending` holds
    /// its nested arms.
    fn unfinished_nested(&mut self, pending: Option<Pending>) {
        if let Some(Pending::Arms(nested)) = pending
            && let Some(arm) = self.arms.last_mut()
        {
            arm.nested = Some(NestedArms {
                when: nested.head,
                arms: Err(nested.missing()),
            });
        }
    }
}

/// A match keyword as written, with its `*`.
fn display(keyword: &str, open: bool) -> String {
    if open {
        format!("{keyword}*")
    } else {
        keyword.to_owned()
    }
}

/// A keyword form's context: the enclosing one, floored at its head's line.
fn form_context(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
    head: usize,
) -> ExpressionContext {
    view.line_indentation_at(head)
        .map_or(context, |indentation| {
            context.with_physical_line_floor(indentation)
        })
        .may_yield()
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
            pending: None,
            layout_owner: 0,
            match_owner: None,
            arms_owner: None,
            level: 0,
            mark_scan: None,
        }
    }

    fn in_region(&self) -> bool {
        self.inherited_region || self.region
    }

    fn arms(&self) -> Option<&Arms> {
        match &self.kind {
            FrameKind::Layout(layout) => layout.arms.as_deref(),
            FrameKind::Group {
                block: Some(block), ..
            } => block.arms.as_deref(),
            _ => None,
        }
    }

    fn arms_mut(&mut self) -> Option<&mut Arms> {
        match &mut self.kind {
            FrameKind::Layout(layout) => layout.arms.as_deref_mut(),
            FrameKind::Group {
                block: Some(block), ..
            } => block.arms.as_deref_mut(),
            _ => None,
        }
    }

    /// Whether it reads guards, whose clauses an `and` separates.
    fn in_guards(&self) -> bool {
        match &self.kind {
            FrameKind::Conditional(conditional) => conditional.phase == Phase::Guards,
            _ => self
                .arms()
                .is_some_and(|arms| arms.phase == ArmPhase::Guards),
        }
    }

    /// Whether a `->` ends the open tails of frames opened inside this one:
    /// in guards and statements it does, in a `then` branch or an arm's
    /// result it does not, and an `else` branch ends where the conditional
    /// itself does.
    fn nested_arrow(&self) -> bool {
        if let Some(arms) = self.arms() {
            return arms.phase != ArmPhase::Result;
        }
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

/// A conditional's, a `do` block's or a match's interpretation, held in the
/// arena until consumed.
pub(super) struct Form {
    pub(super) head: usize,
    pub(super) keyword: &'static str,
    pub(super) shape: FormShape,
    pub(super) result: Parse<SyntaxExpr>,
}

/// How a form reads, which its place among the pieces confirms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FormShape {
    /// `if Guards then A else B`, a `do` block or a match: an atom.
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
    pass.close_open_frames(index);
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
        let pending = self.top().pending.take();
        self.significant(index);
        let parent = self.top();
        parent.lambda_head = false;
        let (mode, context, inherited) = (parent.mode, parent.context, parent.in_region());
        // A `do` block's statements may bind patterns, and end their tails
        // at `->`; its context is the `do` form's, floored at its head. So
        // may a match's arms, with their own context.
        let block = match pending.filter(|_| delimiter == Delimiter::Brace) {
            Some(Pending::Do(head)) => Some(Box::new(Block {
                head,
                context: form_context(view, context, head),
                arms: None,
            })),
            Some(Pending::Arms(mut arms)) => {
                arms.begin(0);
                Some(Box::new(Block {
                    head: arms.head,
                    context: arms.context,
                    arms: Some(arms),
                }))
            }
            None => None,
        };
        let (mode, arrow) = if block.is_some() {
            (Mode::Cover, true)
        } else {
            (mode, mode == Mode::Cover)
        };
        self.push(Frame::new(
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

    /// Pushes `frame` with its owners: the layout owner that judges its
    /// lines, the match whose `with` and the arms block whose marks it may
    /// meet, and the group or view whose depth its tokens are at.
    fn push(&mut self, mut frame: Frame) {
        let at = self.stack.len();
        let (layout, matches, arms, level) =
            self.stack.last().map_or((0, None, None, 0), |parent| {
                (
                    parent.layout_owner,
                    parent.match_owner,
                    parent.arms_owner,
                    parent.level,
                )
            });
        (
            frame.layout_owner,
            frame.match_owner,
            frame.arms_owner,
            frame.level,
        ) = match &frame.kind {
            FrameKind::View => (at, None, None, at),
            FrameKind::Group { block, .. } => (
                at,
                None,
                block
                    .as_ref()
                    .and_then(|block| block.arms.as_ref())
                    .map(|_| at),
                at,
            ),
            FrameKind::Layout(layout) if layout.arms.is_some() => (at, None, Some(at), level),
            FrameKind::Layout(_) => (at, matches, arms, level),
            FrameKind::Conditional(_) => (layout, matches, arms, level),
            FrameKind::Match(_) => (layout, Some(at), arms, level),
        };
        self.stack.push(frame);
    }

    fn close_group(&mut self, index: usize) {
        self.close_open_frames(index);
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
        if let Some(mut block) = block {
            if let Some(arms) = block.arms.take() {
                let (kind, head) = (arms.kind, arms.head);
                let result =
                    self.finish_braced_arms(*arms, &frame.pieces, frame.pending, open, index);
                self.top().line_starts += line_starts;
                self.deliver_arms(kind, head, result, index + 1);
                return;
            }
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
            FrameKind::Match(found) => {
                if found.phase == MatchPhase::Head {
                    found.phase = MatchPhase::Subject;
                }
                top.context = found.context.child_owner_at(view, index).validated();
            }
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
            } => {
                top.context = match &mut block.arms {
                    Some(arms) => arms.member_context(view, index),
                    None => block.context.child_owner_at(view, index).validated(),
                }
            }
            FrameKind::Layout(layout) => {
                if layout.anchor.is_none() {
                    // The first statement sets the anchor: hanging on the
                    // `do`'s own line, else at its line's indentation.
                    let line = |at: usize| {
                        view.token_at(at)
                            .and_then(|token| view.line_at_span(token.span()))
                    };
                    let indentation = view.line_indentation_at(index).unwrap_or(0);
                    if line(index) == line(layout.head) {
                        let column = view
                            .token_at(index)
                            .and_then(|token| view.column_at_span(token.span()))
                            .unwrap_or(indentation);
                        layout.anchor = Some(column);
                        layout.hanging = column > indentation;
                        if layout.arms.is_none()
                            && index == layout.head + 1
                            && view
                                .token_at(index)
                                .is_some_and(|token| token.leading() == LeadingTrivia::Joint)
                        {
                            layout.failure.get_or_insert(
                                Fail::error("`do` must be separated from its body")
                                    .or_at(Some(layout.head)),
                            );
                        }
                    } else {
                        layout.anchor = Some(indentation);
                    }
                    layout.statements.push(top.pieces.len());
                    if let Some(arms) = &mut layout.arms {
                        arms.begin(top.pieces.len());
                    }
                }
                top.context = match &mut layout.arms {
                    Some(arms) => arms.member_context(view, index),
                    None => layout.context.child_owner_at(view, index).validated(),
                };
            }
            _ => {}
        }
    }

    fn token(&mut self, index: usize) -> Step {
        let view = self.scope.view;
        let kind = token(view, index).kind();
        match kind {
            TokenKind::LineStart { indentation } => {
                let starts_statement = self.judge_line(index, *indentation);
                let top = self.top();
                if top.first.is_some() {
                    top.line_starts += 1;
                }
                top.pieces.push(Piece::Token(index));
                if starts_statement {
                    // A `do` or a trailing `when` before this line heads no
                    // braced block: the line starts the next statement or
                    // arm, as the structural parser split them.
                    let pending = top.pending.take();
                    if let Some(arms) = top.arms_mut() {
                        arms.unfinished_nested(pending);
                    }
                    top.region = false;
                    top.region_conditionals.clear();
                    top.lambda_head = false;
                    top.member_start = true;
                    if let FrameKind::Layout(layout) = &mut top.kind {
                        layout.statements.push(top.pieces.len());
                        if let Some(arms) = &mut layout.arms {
                            arms.begin(top.pieces.len());
                            top.mode = Mode::Cover;
                        }
                    }
                }
                return Step::Next;
            }
            TokenKind::Symbol(symbol)
                if self
                    .top()
                    .separator
                    .is_some_and(|separator| separator == *symbol) =>
            {
                // A separator ends the open frames and the region before it,
                // and the next member starts its own context.
                self.close_open_frames(index);
                self.significant(index);
                let top = self.top();
                top.region = false;
                top.region_conditionals.clear();
                top.lambda_head = false;
                top.member_start = true;
                top.pieces.push(Piece::Token(index));
                // In a braced block of arms, the next arm begins.
                let start = top.pieces.len();
                if let FrameKind::Group {
                    block: Some(block), ..
                } = &mut top.kind
                    && let Some(arms) = &mut block.arms
                {
                    arms.begin(start);
                    top.mode = Mode::Cover;
                }
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
        // A `when` first after a match's head makes it a `match when`.
        if let FrameKind::Match(found) = &mut top.kind
            && found.phase == MatchPhase::Head
            && matches!(kind, TokenKind::Name("when"))
            && in_keyword_position(view, index)
        {
            found.phase = MatchPhase::Arms;
            let arms = Arms::new(
                found.head,
                ArmsKind::When {
                    keyword: found.keyword,
                    open: found.open,
                },
                found.context,
            );
            self.begin_arms(index, arms);
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
                "do" if may_head_keyword_form(view, index) => {
                    if self.braced_do(index) {
                        let top = self.top();
                        top.pending = Some(Pending::Do(index));
                        top.lambda_head = false;
                        top.pieces.push(Piece::Token(index));
                    } else {
                        self.open_layout_do(index);
                    }
                    return Step::Next;
                }
                "match" | "try_match" if may_head_keyword_form(view, index) => {
                    return self.open_match(index);
                }
                // The first ungrouped `with` after a match's head is its own.
                "with" if in_keyword_position(view, index) => {
                    if self.claim_with(index) {
                        return Step::Next;
                    }
                    self.top().view_level = true;
                }
                "when" if in_keyword_position(view, index) && self.arm_mark(index) => {
                    return Step::Next;
                }
                // An `and` in guards ends a clause, and the `else` tails
                // open in it.
                "and" if in_keyword_position(view, index) && self.guards_below_tails() => {
                    while !self.top().in_guards() {
                        self.close_conditional(index);
                    }
                    let top = self.top();
                    top.lambda_head = false;
                    top.member_start = true;
                    top.pieces.push(Piece::Token(index));
                    return Step::Next;
                }
                "let" | "object" | "where" if in_keyword_position(view, index) => {
                    self.top().view_level = true;
                }
                _ => {}
            },
            TokenKind::Symbol("=>" | "=>?") if self.arm_mark(index) => return Step::Next,
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
        self.next_significant(index + 1).is_some_and(|next| {
            let token = token(view, next);
            matches!(
                token.kind(),
                TokenKind::Open {
                    delimiter: Delimiter::Brace,
                    ..
                }
            ) && (next > index + 1 || token.leading() != LeadingTrivia::Joint)
        })
    }

    /// The first token from `index` on that is not a line start.
    fn next_significant(&self, mut index: usize) -> Option<usize> {
        let view = self.scope.view;
        while index < view.range().end() {
            if !matches!(token(view, index).kind(), TokenKind::LineStart { .. }) {
                return Some(index);
            }
            index += 1;
        }
        None
    }

    /// A token inside the top frame's keyword region: only its end, and the
    /// conditionals counted inside it, matter. Separators are handled before.
    fn region_token(&mut self, index: usize) -> Step {
        let view = self.scope.view;
        let kind = token(view, index).kind();
        // A match's `with` and an arm's marks end the region, as they ended
        // the view its form was parsed from.
        let taken = match kind {
            TokenKind::Name("with") => in_keyword_position(view, index) && self.claim_with(index),
            TokenKind::Name("when") => in_keyword_position(view, index) && self.arm_mark(index),
            TokenKind::Symbol("=>" | "=>?") => self.arm_mark(index),
            _ => false,
        };
        if taken {
            return Step::Next;
        }
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
        self.push(frame);
    }

    /// A `then` or `else` in keyword position, for the innermost conditional
    /// that accepts it; a finished conditional's open tail ends at it.
    fn phase_keyword(&mut self, index: usize) -> Step {
        let view = self.scope.view;
        let is_then = matches!(token(view, index).kind(), TokenKind::Name("then"));
        loop {
            let top = self.top();
            if matches!(top.kind, FrameKind::Layout(_) | FrameKind::Match(_)) {
                // A layout block is an open tail too, and a match without
                // its arms ends unfinished.
                self.close_top(index);
                continue;
            }
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

    /// Whether the top frame reads guards, or `else` branches open in them.
    fn guards_below_tails(&self) -> bool {
        self.stack
            .iter()
            .rev()
            .find(|frame| {
                !matches!(&frame.kind,
                    FrameKind::Conditional(conditional) if conditional.phase == Phase::Else)
            })
            .is_some_and(Frame::in_guards)
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

    /// Closes every conditional, layout block and match above the innermost
    /// group or the view.
    fn close_open_frames(&mut self, index: usize) {
        while matches!(
            self.stack.last().map(|frame| &frame.kind),
            Some(FrameKind::Conditional(_) | FrameKind::Layout(_) | FrameKind::Match(_))
        ) {
            self.close_top(index);
        }
    }

    /// Closes the conditional, layout block or match on top, which ends
    /// before `index`.
    fn close_top(&mut self, index: usize) {
        match self.stack.last().map(|frame| &frame.kind) {
            Some(FrameKind::Conditional(_)) => self.close_conditional(index),
            Some(FrameKind::Layout(_)) => self.close_layout(index),
            Some(FrameKind::Match(_)) => self.close_match(index, None),
            _ => unreachable!("only open tails close before their group"),
        }
    }

    /// Judges a line start by its layout owner. A shallower line closes a
    /// layout block, and is judged again by the next owner; returns whether
    /// the line starts a statement of the block now on top.
    fn judge_line(&mut self, index: usize, indentation: usize) -> bool {
        loop {
            let owner = self.stack.last().map_or(0, |frame| frame.layout_owner);
            let FrameKind::Layout(layout) = &self.stack[owner].kind else {
                return false;
            };
            let (Some(anchor), hanging) = (layout.anchor, layout.hanging) else {
                // A block yet to start lies in a statement of the layout
                // around it, which judges the line: one that ends that
                // statement leaves the block empty.
                let outer = owner
                    .checked_sub(1)
                    .map_or(0, |parent| self.stack[parent].layout_owner);
                let FrameKind::Layout(outer_layout) = &self.stack[outer].kind else {
                    return false;
                };
                let Some(outer_anchor) = outer_layout.anchor else {
                    return false;
                };
                if self.closer_only_line(index) || indentation > outer_anchor {
                    return false;
                }
                while self.stack.len() > owner + 1 {
                    self.close_top(index);
                }
                self.close_layout(index);
                continue;
            };
            if self.closer_only_line(index) || indentation > anchor {
                if hanging && let FrameKind::Layout(layout) = &mut self.stack[owner].kind {
                    layout.hanging_lines.push(index);
                }
                return false;
            }
            while self.stack.len() > owner + 1 {
                self.close_top(index);
            }
            if indentation == anchor {
                return true;
            }
            if let FrameKind::Layout(layout) = &mut self.stack[owner].kind {
                layout.boundary = Some(index);
            }
            self.close_layout(index);
        }
    }

    /// Whether the line after the line start at `index` holds only closing
    /// delimiters.
    fn closer_only_line(&self, index: usize) -> bool {
        let view = self.scope.view;
        let mut next = index + 1;
        let mut closers = 0;
        while let Some(token) = view.token_at(next) {
            match token.kind() {
                TokenKind::Close { .. } => closers += 1,
                TokenKind::LineStart { .. } => break,
                _ => return false,
            }
            next += 1;
        }
        closers > 0
    }

    fn open_layout_do(&mut self, index: usize) {
        let view = self.scope.view;
        let parent = self.top();
        parent.lambda_head = false;
        let context = form_context(view, parent.context, index);
        let separator = parent.separator;
        self.push(Frame::new(
            FrameKind::Layout(Box::new(LayoutBlock {
                head: index,
                context,
                anchor: None,
                hanging: false,
                statements: Vec::new(),
                hanging_lines: Vec::new(),
                boundary: None,
                failure: None,
                arms: None,
            })),
            // Statements may bind patterns, and end their tails at `->`.
            Mode::Cover,
            context,
            separator,
            true,
            false,
        ));
    }

    /// Closes the layout block on top, which ends before `index`, into a
    /// form piece of its parent.
    fn close_layout(&mut self, index: usize) {
        let frame = self.stack.pop().expect("a layout block is open");
        let line_starts = frame.line_starts;
        let FrameKind::Layout(mut layout) = frame.kind else {
            unreachable!("only layout blocks close here");
        };
        if let Some(arms) = layout.arms.take() {
            let (kind, head) = (arms.kind, arms.head);
            let result =
                self.finish_layout_arms(&layout, *arms, &frame.pieces, frame.pending, index);
            self.top().line_starts += line_starts;
            self.deliver_arms(kind, head, result, index);
            return;
        }
        let head = layout.head;
        let result = self.finish_layout(&layout, &frame.pieces, index);
        let covers = self.scope.covers;
        let parent = self.top();
        parent.line_starts += line_starts;
        parent.lambda_head = false;
        parent.pieces.push(Piece::Form {
            head,
            end: index,
            form: covers.form(Form {
                head,
                keyword: "do",
                shape: FormShape::Prefix,
                result,
            }),
        });
    }

    /// A layout `do` block's statements, checked as the structural parser
    /// checked them.
    fn finish_layout(
        &self,
        layout: &LayoutBlock,
        pieces: &[Piece],
        end: usize,
    ) -> Parse<SyntaxExpr> {
        let view = self.scope.view;
        let head = layout.head;
        if let Some(failure) = layout.failure.clone() {
            return Err(failure);
        }
        let Some(anchor) = layout.anchor else {
            return Err(
                Fail::error("layout do expression requires a newline-delimited block")
                    .or_at(Some(head)),
            );
        };
        let first = layout.statements[0];
        let first_token = pieces.get(first).map(piece_token);
        if !layout.context.accepts_layout_anchor(anchor) {
            return Err(Fail::error(format!(
                "do layout begins at indentation {anchor}; expected more than continuation floor {}",
                layout.context.continuation_floor()
            ))
            .or_at(first_token));
        }
        if layout.hanging {
            // A line is judged within the `do` form's own extent, its group
            // member, as the structural parser judged it.
            let extent = structural_view_at(view, head);
            for line in layout.boundary.iter().chain(&layout.hanging_lines) {
                if line_begins_do_statement(line_view(extent, *line)) {
                    let indentation = match token(view, *line).kind() {
                        TokenKind::LineStart { indentation } => *indentation,
                        _ => 0,
                    };
                    return Err(Fail::error(format!(
                        "hanging do statement is indented {indentation} spaces; expected sibling indentation {anchor}"
                    ))
                    .or_at(Some(*line)));
                }
            }
        }
        let scope = Scope {
            view: subview(view, head + 1, end),
            mode: Mode::Cover,
            ..self.scope
        };
        let bounds = layout
            .statements
            .iter()
            .copied()
            .chain(std::iter::once(pieces.len()))
            .collect::<Vec<_>>();
        let mut parsed = Vec::with_capacity(layout.statements.len());
        for pair in bounds.windows(2) {
            let statement = trim_padding(view, &pieces[pair[0]..pair[1]]);
            let Some(first) = statement.first().map(piece_token) else {
                continue;
            };
            let line = view
                .token_at(first)
                .and_then(|token| view.line_at_span(token.span()))
                .unwrap_or(1);
            let context = layout.context.child_owner_at(view, first);
            parsed.push((line, binding::statement(scope, statement, context)?));
        }
        if parsed.is_empty() {
            return Err(
                Fail::error("layout do expression requires at least one statement")
                    .or_at(Some(head)),
            );
        }
        finish_do_statements(parsed).map_err(|message| Fail::error(message).or_at(Some(head)))
    }

    /// Opens a match at its head, skipping a joint `*`.
    fn open_match(&mut self, index: usize) -> Step {
        let view = self.scope.view;
        let keyword = match token(view, index).kind() {
            TokenKind::Name("try_match") => "try_match",
            _ => "match",
        };
        let star = (index + 1 < view.range().end())
            .then(|| token(view, index + 1))
            .filter(|star| matches!(star.kind(), TokenKind::Symbol("*")));
        let open = star.is_some_and(|star| star.leading() == LeadingTrivia::Joint);
        let failure = star.filter(|_| !open).map(|_| {
            Fail::error(format!(
                "the open form is spelled `{keyword}*` without intervening whitespace"
            ))
            .or_at(Some(index + 1))
        });
        let parent = self.top();
        parent.lambda_head = false;
        let context = form_context(view, parent.context, index);
        let separator = parent.separator;
        let mut frame = Frame::new(
            FrameKind::Match(Box::new(Match {
                head: index,
                keyword,
                open,
                phase: MatchPhase::Head,
                context,
                subject: None,
                failure,
            })),
            Mode::Expression,
            context,
            separator,
            false,
            false,
        );
        frame.first = Some(index);
        frame.member_start = true;
        self.push(frame);
        if open { Step::Skip } else { Step::Next }
    }

    /// Takes the `with` at `index` for the innermost match reading its
    /// subject at this level, if any: the frames open in its subject close
    /// before it, and its subject is read. Returns whether it took it.
    fn claim_with(&mut self, index: usize) -> bool {
        let Some(owner) = self.stack.last().and_then(|frame| frame.match_owner) else {
            return false;
        };
        if !matches!(&self.stack[owner].kind,
            FrameKind::Match(found) if found.phase == MatchPhase::Subject)
        {
            return false;
        }
        while self.stack.len() > owner + 1 {
            self.close_top(index);
        }
        let scope = self.scope;
        let view = scope.view;
        let frame = self.top();
        frame.region = false;
        frame.region_conditionals.clear();
        frame.lambda_head = false;
        let pieces = std::mem::take(&mut frame.pieces);
        let FrameKind::Match(found) = &mut frame.kind else {
            unreachable!("a match owns the `with`");
        };
        found.phase = MatchPhase::Arms;
        let subject = trim_padding(view, &pieces);
        found.subject = Some(match subject.first().map(piece_token) {
            None => Err(Fail::error(format!(
                "{} expression requires a subject before `with`",
                display(found.keyword, found.open)
            ))
            .or_at(Some(found.head))),
            Some(first) => {
                let scope = Scope {
                    view: subview(view, first, index),
                    mode: Mode::Expression,
                    ..scope
                };
                let context = found.context.child_owner_at(view, first);
                binding::expression(scope, subject, context, false)
                    .map(|(expression, _)| expression)
            }
        });
        let arms = Arms::new(
            found.head,
            ArmsKind::Match {
                keyword: found.keyword,
                open: found.open,
            },
            found.context,
        );
        self.begin_arms(index, arms);
        true
    }

    /// Starts arms after the `with` or `when` at `keyword`: a braced block
    /// if a `{` comes next, unless that group begins the first arm's
    /// pattern, as a mark after it on its line shows; otherwise a layout
    /// block. The marks on a line where an enclosing arm reads its head or
    /// guards are that arm's, as the structural parser split it first.
    fn begin_arms(&mut self, keyword: usize, arms: Arms) {
        let view = self.scope.view;
        let enclosing_head = self
            .stack
            .last()
            .and_then(|frame| frame.arms_owner)
            .and_then(|owner| self.stack[owner].arms())
            .is_some_and(|enclosing| enclosing.phase != ArmPhase::Result);
        let brace =
            self.next_significant(keyword + 1)
                .and_then(|next| match token(view, next).kind() {
                    TokenKind::Open {
                        group,
                        delimiter: Delimiter::Brace,
                    } => view.group(*group).and_then(|group| group.close_token()),
                    _ => None,
                });
        if let Some(close) = brace
            && !(arms.brace_may_begin_arm()
                && !enclosing_head
                && self.line_mark(close + 1).is_some())
        {
            self.top().pending = Some(Pending::Arms(Box::new(arms)));
            return;
        }
        let parent = self.top();
        parent.lambda_head = false;
        let separator = parent.separator;
        let context = arms.context;
        self.push(Frame::new(
            FrameKind::Layout(Box::new(LayoutBlock {
                head: keyword,
                context,
                anchor: None,
                hanging: false,
                statements: Vec::new(),
                hanging_lines: Vec::new(),
                boundary: None,
                failure: None,
                arms: Some(Box::new(arms)),
            })),
            // An arm's head and guards may bind patterns.
            Mode::Cover,
            context,
            separator,
            true,
            false,
        ));
    }

    /// Takes the `=>`, `=>?` or `when` at `index` for the innermost arms
    /// block at this level whose arm reads its head or guards, if any: the
    /// frames open in the arm close before it. An arrow begins the arm's
    /// result; a `when` begins its guards, or heads nested arms when no mark
    /// follows it on its line. Returns whether it took the mark.
    fn arm_mark(&mut self, index: usize) -> bool {
        let Some(owner) = self.stack.last().and_then(|frame| frame.arms_owner) else {
            return false;
        };
        if self.stack[owner]
            .arms()
            .is_none_or(|arms| arms.phase == ArmPhase::Result)
        {
            return false;
        }
        while self.stack.len() > owner + 1 {
            self.close_top(index);
        }
        let view = self.scope.view;
        let when = matches!(token(view, index).kind(), TokenKind::Name("when"));
        let next = self.line_mark(index + 1);
        // An arm's line holds one arrow, as the structural parser counted
        // the arrows on its first line.
        let mut later = next;
        let second_arrow = loop {
            match later {
                Some(mark) if !matches!(token(view, mark).kind(), TokenKind::Name("when")) => {
                    break !when;
                }
                Some(mark) => later = self.line_mark(mark + 1),
                None => break false,
            }
        };
        let frame = self.top();
        frame.region = false;
        frame.region_conditionals.clear();
        frame.lambda_head = false;
        frame.pieces.push(Piece::Token(index));
        if !when || next.is_some() {
            frame.member_start = true;
            if !when {
                frame.mode = Mode::Expression;
            }
        }
        let arms = frame.arms_mut().expect("an arms owner holds arms");
        if !when {
            if second_arrow {
                let message = format!(
                    "{} permits exactly one top-level `=>` or `=>?`",
                    arms.arm_label()
                );
                if let Some(arm) = arms.arms.last_mut() {
                    arm.failure
                        .get_or_insert(Fail::error(message).or_at(Some(index)));
                }
            }
            arms.phase = ArmPhase::Result;
            return true;
        }
        if next.is_some() {
            arms.phase = ArmPhase::Guards;
            return true;
        }
        // A trailing `when`: its nested arms lie deeper than its arm's
        // first line.
        let first = arms.arms.last().and_then(|arm| arm.first).unwrap_or(index);
        let floor = view.line_indentation_at(first).unwrap_or(0);
        let nested = Arms::new(
            index,
            ArmsKind::Nested,
            arms.context.with_continuation_floor(floor),
        );
        self.begin_arms(index, nested);
        true
    }

    /// The first arm mark from `from` on its line, at the top frame's
    /// depth: an `=>`, `=>?` or `when` in keyword position, before a line
    /// start, a close, the depth's separator, or the end of the view. A
    /// scan resumes where the last one at its depth stopped, so a line
    /// costs one walk however many marks and braces it holds.
    fn line_mark(&mut self, from: usize) -> Option<usize> {
        let view = self.scope.view;
        let level = self.stack.last().map_or(0, |frame| frame.level);
        let frame = &mut self.stack[level];
        if let Some(scan) = frame.mark_scan
            && scan.from <= from
            && from <= scan.to
        {
            return scan.mark.then_some(scan.to);
        }
        let separator = frame.separator;
        let end = view.range().end();
        let mut index = from;
        let (to, mark) = loop {
            if index >= end {
                break (end, false);
            }
            match token(view, index).kind() {
                TokenKind::LineStart { .. } | TokenKind::Close { .. } => break (index, false),
                TokenKind::Symbol("=>" | "=>?") => break (index, true),
                TokenKind::Symbol(symbol) if separator == Some(*symbol) => break (index, false),
                TokenKind::Name("when") if in_keyword_position(view, index) => break (index, true),
                TokenKind::Open { group, .. } => {
                    if let Some(close) = view.group(*group).and_then(|group| group.close_token()) {
                        index = close;
                    }
                }
                _ => {}
            }
            index += 1;
        };
        frame.mark_scan = Some(MarkScan { from, to, mark });
        mark.then_some(to)
    }

    /// Hands a closed block's arms to their owner: nested arms to the arm
    /// whose trailing `when` heads them, and a match's arms to the match,
    /// which closes at `end` with them.
    fn deliver_arms(&mut self, kind: ArmsKind, head: usize, result: Parse<ArmList>, end: usize) {
        if kind != ArmsKind::Nested {
            self.close_match(end, Some(result));
            return;
        }
        let arms = result.map(|list| match list {
            ArmList::When(arms) => arms,
            ArmList::Match(_) => unreachable!("nested arms are read as guards"),
        });
        let arm = self
            .top()
            .arms_mut()
            .and_then(|parent| parent.arms.last_mut())
            .expect("nested arms close into the arm that heads them");
        arm.nested = Some(NestedArms { when: head, arms });
    }

    /// Closes the match on top, which ends before `end`, into a form piece
    /// of its parent: with its arms, or unfinished without them.
    fn close_match(&mut self, end: usize, arms: Option<Parse<ArmList>>) {
        let frame = self.stack.pop().expect("a match is open");
        let line_starts = frame.line_starts;
        let FrameKind::Match(found) = frame.kind else {
            unreachable!("only matches close here");
        };
        let (head, keyword) = (found.head, found.keyword);
        let result = self.finish_match(*found, frame.pending, arms);
        let covers = self.scope.covers;
        let parent = self.top();
        parent.line_starts += line_starts;
        parent.lambda_head = false;
        parent.pieces.push(Piece::Form {
            head,
            end,
            form: covers.form(Form {
                head,
                keyword,
                shape: FormShape::Prefix,
                result,
            }),
        });
    }

    fn finish_match(
        &self,
        found: Match,
        pending: Option<Pending>,
        arms: Option<Parse<ArmList>>,
    ) -> Parse<SyntaxExpr> {
        let view = self.scope.view;
        let head = found.head;
        if let Some(failure) = found.failure {
            return Err(failure);
        }
        let Some(arms) = arms else {
            return Err(match pending {
                // Its braced arms never came.
                Some(Pending::Arms(arms)) => arms.missing(),
                _ => Fail::error(format!(
                    "{} expression requires `with` and at least one layout arm or an explicit `{{}}` body",
                    display(found.keyword, found.open)
                ))
                .or_at(Some(head)),
            });
        };
        let line = view
            .token_at(head)
            .and_then(|token| view.line_at_span(token.span()))
            .unwrap_or(1);
        let mode = if found.keyword == "try_match" {
            ConditionalMode::Host
        } else {
            ConditionalMode::Pure
        };
        let commitment = if found.open {
            MatchCommitment::Open
        } else {
            MatchCommitment::Cut
        };
        Ok(match (found.subject, arms?) {
            (Some(subject), ArmList::Match(arms)) => SyntaxExpr::Match(MatchExpr {
                line,
                mode,
                commitment,
                subject: Box::new(subject?),
                arms,
            }),
            (None, ArmList::When(arms)) => SyntaxExpr::MatchWhen(MatchWhenExpr {
                line,
                mode,
                commitment,
                arms,
            }),
            _ => unreachable!("a match with a subject reads match arms"),
        })
    }

    /// A layout block's arms, checked as the structural parser checked them.
    fn finish_layout_arms(
        &self,
        layout: &LayoutBlock,
        mut arms: Arms,
        pieces: &[Piece],
        pending: Option<Pending>,
        end: usize,
    ) -> Parse<ArmList> {
        let view = self.scope.view;
        arms.unfinished_nested(pending);
        let Some(anchor) = layout.anchor else {
            return Err(arms.missing());
        };
        if !layout.context.accepts_layout_anchor(anchor) {
            let first = arms.arms.first().and_then(|arm| arm.first);
            return Err(Fail::error(format!(
                "{} begins at indentation {anchor}; expected more than continuation floor {}",
                arms.body_label(),
                layout.context.continuation_floor()
            ))
            .or_at(first));
        }
        let ends = arms
            .arms
            .iter()
            .skip(1)
            .map(|arm| arm.start)
            .chain(std::iter::once(pieces.len()))
            .collect::<Vec<_>>();
        let members = arms
            .arms
            .iter()
            .zip(ends)
            .map(|(arm, end)| &pieces[arm.start..end])
            .collect::<Vec<_>>();
        let scope = Scope {
            view: subview(view, layout.head + 1, end),
            mode: Mode::Cover,
            ..self.scope
        };
        let missing = arms.missing();
        let list = self.read_arms(scope, arms, members)?;
        let empty = match &list {
            ArmList::Match(list) => list.is_empty(),
            ArmList::When(list) => list.is_empty(),
        };
        if empty { Err(missing) } else { Ok(list) }
    }

    /// A braced block's arms, split at its `;`s: one leading and one
    /// trailing `;` are allowed, and `{}` holds none.
    fn finish_braced_arms(
        &self,
        mut arms: Arms,
        pieces: &[Piece],
        pending: Option<Pending>,
        open: usize,
        close: usize,
    ) -> Parse<ArmList> {
        let view = self.scope.view;
        arms.unfinished_nested(pending);
        let fail = |message: String| Err(Fail::error(message).or_at(Some(open)));
        let is_semicolon = |piece: &Piece| {
            matches!(piece, Piece::Token(index)
                if matches!(token(view, *index).kind(), TokenKind::Symbol(";")))
        };
        let members = pieces.split(is_semicolon).collect::<Vec<_>>();
        debug_assert_eq!(members.len(), arms.arms.len(), "each member begins an arm");
        let empty = |member: &[Piece]| trim_padding(view, member).is_empty();
        if members.iter().all(|member| empty(member)) && members.len() > 1 {
            return fail(format!(
                "{} cannot contain only semicolons; use `{{}}` for an empty body",
                arms.body_label()
            ));
        }
        let last = members.len() - 1;
        if (1..last).any(|position| empty(members[position])) {
            return fail(format!(
                "{} contains an empty member between semicolons",
                arms.body_label()
            ));
        }
        let scope = Scope {
            view: subview(view, open + 1, close),
            mode: Mode::Cover,
            ..self.scope
        };
        self.read_arms(scope, arms, members)
    }

    /// Reads each arm from its pieces, skipping empty members.
    fn read_arms(
        &self,
        scope: Scope<'_, '_, '_>,
        arms: Arms,
        members: Vec<&[Piece]>,
    ) -> Parse<ArmList> {
        let view = self.scope.view;
        let mut list = match arms.kind {
            ArmsKind::Match { .. } => ArmList::Match(Vec::new()),
            _ => ArmList::When(Vec::new()),
        };
        for (member, arm) in members.into_iter().zip(arms.arms) {
            let member = trim_padding(view, member);
            if member.is_empty() {
                continue;
            }
            if let Some(failure) = arm.failure {
                return Err(failure);
            }
            match &mut list {
                ArmList::Match(list) => {
                    list.push(binding::match_arm(scope, member, arm.nested, arms.context)?);
                }
                ArmList::When(list) => {
                    list.push(binding::when_arm(scope, member, arm.nested, arms.context)?);
                }
            }
        }
        Ok(list)
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

/// The line after the line start at `index`, to the next line start at the
/// same delimiter depth or the end of `view`.
fn line_view<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    index: usize,
) -> TokenView<'lex, 'source> {
    let mut end = index + 1;
    while end < view.range().end()
        && let Some(token) = view.token_at(end)
    {
        match token.kind() {
            TokenKind::LineStart { .. } => break,
            TokenKind::Open { group, .. } => {
                end = view
                    .group(*group)
                    .and_then(|group| group.close_token())
                    .map_or(end, |close| close.min(view.range().end() - 1));
            }
            _ => {}
        }
        end += 1;
    }
    subview(view, index + 1, end.min(view.range().end()))
}

fn subview<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    start: usize,
    end: usize,
) -> TokenView<'lex, 'source> {
    view.subview(TokenRange::new(start, end).expect("ordered token indices form a range"))
        .expect("a conditional's parts lie in its view")
}
