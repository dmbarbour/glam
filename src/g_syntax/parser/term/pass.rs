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
//! subject reads as an expression up to an `as`, which belongs to the
//! innermost match reading its subject at its level, and its arms are a
//! block, braced or layout. While an arm reads its head or guards, its `=>`,
//! `=>?` and `when` close the frames opened in it: an arrow begins its
//! result, and a `when` its guards, or nested arms when no mark follows it
//! on its line. Each frame records the match and the arms block whose
//! keywords it may meet, as it records its layout owner. An arm's pieces are
//! read when its block closes (`binding`).
//!
//! A `let` or `using` is a frame, as a match is: a `let`'s bindings are a
//! block above it, braced when a `{` follows on its line and otherwise
//! layout, read binding by binding when the block closes; a `using`'s
//! namespace runs to its `in` or `do`. An `in` ends the bindings or the
//! namespace, as does a layout block's dedent to a line aligned with the
//! `let`, and the body after it is an open tail.
//!
//! A `where` ends the definition, statement, arm or group it stands in: the
//! open tails before it close, and its bindings are a block like a `let`'s,
//! which closes into a form piece that the body's reader takes
//! (`binding::split_where`). A view's own `where` stops the walk, and the
//! body around the view reads its bindings with a walk of their own
//! (`where_group`).
//!
//! In cover mode a pattern's `when` opens a keyword region, whose groups
//! are deferred for the guard-clause parser. Inside a region conditionals
//! are only counted, so that a `then` or `else` still reaches its own, and
//! inside a deferred group nothing is interpreted.
//!
//! An object is a frame whose header is an open tail until its `with`. A
//! `with` body, an object's or an update's, is a block whose tokens are
//! only kept and whose lines are judged: the declaration parser reads its
//! members from their views. An update closes into a form piece whose base
//! the item interpreter takes from the chain before it.
//!
//! The legacy grammar's view-level dispatch parsed `let`, `where`, `with`
//! and `object` from a branch's view; a branch holding one of them where
//! the legacy reading leaves it a token is still parsed that way.

use crate::g_syntax::{
    ConditionalMode, ConditionalResultMode, IfExpr, MatchArm, MatchCommitment, MatchExpr,
    MatchWhenExpr, ObjectBodyDefinition, Severity, SyntaxExpr, WhenArm,
};

use super::super::declaration::{parse_nonempty_object_body, parse_object_body};
use super::super::do_expr::{empty_do_expr, finish_do_statements, line_begins_do_statement};
use super::super::expression::{structural_view_at, syntax_operator};
use super::super::expression_context::{ExpressionContext, validate_expression_floor};
use super::super::input::{TokenRange, TokenView};
use super::super::layout::group_separator;
use super::super::lexical::{Delimiter, GroupId, LeadingTrivia, TokenKind};
use super::super::structural::{
    in_keyword_position, is_layout_empty, keyword_head, line_begins_object_member,
    object_alias_name, parse_body, parse_expression_in_context, parse_object_expression_header,
    parse_open_tail, trim_layout,
};
use super::binding::{self, NestedArms, needs_view_dispatch};
use super::{
    Bindings, Cover, Deferred, Fail, GroupBody, Items, Mode, ParenCover, Parse, Piece, Scope, Term,
    conditional_argument, ends_operand, form_label, heads_form, interpret_group, is_symbol,
    legacy_grammar, may_head_keyword_form, misplaced_tail, opens_keyword_region, piece_token,
    regions_enabled, token, trim_padding,
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
    /// The first `\` among its current member's pieces: a lambda whose body
    /// runs to the member's end.
    lambda: Option<usize>,
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
    /// The stack index of the match whose `as` it may meet: the innermost
    /// match reading its subject, unless a group or arms block lies between.
    match_owner: Option<usize>,
    /// The stack index of the arms block whose arms' marks it may meet: the
    /// innermost, unless a group lies between.
    arms_owner: Option<usize>,
    /// The stack index of the group or view whose depth its tokens are at.
    level: usize,
    /// For a group or the view, the last scan along a line for arm marks.
    mark_scan: Option<MarkScan>,
    /// Whether it holds an object or update body, whose members the
    /// declaration parser reads from their views: its tokens and groups are
    /// only kept, and its lines judged.
    raw: bool,
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
    Scoped(Box<Scoped>),
    Object(Box<ObjectForm>),
}

/// A layout block: a `do` block's statements, or a match's arms.
struct LayoutBlock {
    /// The `do`, or the `as` or `when` its arms follow.
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
    /// The local bindings it holds, a `let`'s or a `where`'s, if any.
    bindings: Option<BindingsKind>,
    /// The object or update whose body it holds, if any.
    members: Option<Members>,
}

/// A braced block: a `do` block's head and its own context, or a match's
/// arms.
struct Block {
    head: usize,
    context: ExpressionContext,
    arms: Option<Box<Arms>>,
    /// The local bindings it holds, a `let`'s or a `where`'s, if any.
    bindings: Option<BindingsKind>,
    /// The object or update whose body it holds, if any.
    members: Option<Members>,
}

/// A block whose `{` comes next.
enum Pending {
    /// A `do`'s statements, after the `do` at this index.
    Do(usize),
    Arms(Box<Arms>),
    /// A `let`'s or `where`'s bindings, after its keyword at this index.
    Bindings(usize, BindingsKind),
    /// An object's or update's body, after the `with` at this index.
    Members(usize, Members),
}

/// Whose body a block of members holds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Members {
    /// An update, `Base (as Name)? with Body`, with its alias.
    Update { alias: Option<String> },
    /// The object frame below the block.
    Object,
}

/// An object expression: its header, then its body after `with`, if any.
/// Its header is an open tail until then.
struct ObjectForm {
    /// Its `object`, or the `abstract` before it.
    head: usize,
    /// The form's own context, floored at its head's line.
    context: ExpressionContext,
    /// Its body, once the block after its `with` closes.
    body: Option<Parse<Vec<ObjectBodyDefinition>>>,
    /// Where its header ends: its `with`, once one begins its body.
    with: Option<usize>,
}

/// Whose local bindings a block holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingsKind {
    Let,
    Where,
}

impl BindingsKind {
    fn keyword(self) -> &'static str {
        match self {
            Self::Let => "let",
            Self::Where => "where",
        }
    }
}

/// A `let` or `using` form: its bindings or namespace, then its body, an
/// open tail.
struct Scoped {
    head: usize,
    keyword: &'static str,
    phase: ScopedPhase,
    /// The form's own context, floored at its head's line.
    context: ExpressionContext,
    /// A `let`'s bindings, once their block closes.
    bindings: Option<Parse<Bindings>>,
    /// Whether its bindings spanned lines, so that no `in` may follow them.
    multiline: bool,
    /// A `using`'s namespace, once its `in` or `do` ends it.
    namespace: Option<Parse<SyntaxExpr>>,
    /// Where its body begins.
    body_start: usize,
    /// Its body's context, from the body's first token.
    body_context: Option<ExpressionContext>,
    failure: Option<Fail>,
    /// Line starts in its finished phases, which its parent counts too.
    line_starts: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedPhase {
    /// A `let`'s bindings, a block above it.
    Bindings,
    /// A `using`'s namespace, up to its `in` or `do`.
    Namespace,
    Body,
}

impl Scoped {
    fn new(
        head: usize,
        keyword: &'static str,
        phase: ScopedPhase,
        context: ExpressionContext,
    ) -> Self {
        Self {
            head,
            keyword,
            phase,
            context,
            bindings: None,
            multiline: false,
            namespace: None,
            body_start: head + 1,
            body_context: None,
            failure: None,
            line_starts: 0,
        }
    }

    fn fail(&mut self, at: usize, message: impl Into<String>) {
        self.failure
            .get_or_insert(Fail::error(message).or_at(Some(at)));
    }
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
    /// Its subject, once its `as` ends it; a `match when` has none.
    subject: Option<Parse<SyntaxExpr>>,
    failure: Option<Fail>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchPhase {
    /// Before its first significant token, which a `match when` begins.
    Head,
    Subject,
    /// After its `as` or `when`: its arms are a block above it, or a
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

impl ArmsKind {
    /// The match its arms belong to, as errors name it.
    fn label(self) -> String {
        match self {
            Self::Match { keyword, open } => format!("`{}` expression", display(keyword, open)),
            Self::When { keyword, open } => format!("`{} when` expression", display(keyword, open)),
            Self::Nested => "nested `when` arms".to_string(),
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

/// The context of an object's or update's members after the `with` at
/// `head`: the enclosing one, floored at the `with`'s line when an infix
/// operator leads that line, as the structural parser floored it.
fn members_context(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
    head: usize,
) -> ExpressionContext {
    let mut first = head;
    while let Some(previous) = first.checked_sub(1)
        && view.range().contains(previous)
        && !matches!(token(view, previous).kind(), TokenKind::LineStart { .. })
    {
        first = previous;
    }
    match view.line_indentation_at(head) {
        Some(indentation) if syntax_operator(token(view, first)).is_some() => {
            context.with_continuation_floor(indentation)
        }
        _ => context,
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
            lambda: None,
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
            raw: false,
        }
    }

    fn in_region(&self) -> bool {
        self.inherited_region || self.region || self.raw
    }

    /// Whether it is an open tail: a conditional's `else` branch or fallback,
    /// a `let`'s or `using`'s body, or an object's header before its body.
    fn open_tail(&self) -> bool {
        match &self.kind {
            FrameKind::Conditional(conditional) => conditional.phase == Phase::Else,
            FrameKind::Scoped(scoped) => scoped.phase == ScopedPhase::Body,
            FrameKind::Object(_) => true,
            _ => false,
        }
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
            FrameKind::Scoped(scoped) => scoped.phase == ScopedPhase::Body && self.arrow_ends_tails,
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
        self.lambda = None;
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
    /// Whether it follows an operand in its item, where an `if` reads as
    /// postfix and a `try` has no reading.
    after_operand: bool,
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
    /// Whether its last part ended at its own `}` or a dedent, after which
    /// what may resume after a dedent may follow it.
    pub(super) ends_with_block: bool,
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
    /// A `where`'s bindings, which end the body before them.
    Where,
    /// An update's body, whose base is the chain before it.
    With,
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
        violation: None,
    };
    let index = pass.walk(view.range().start())?;
    pass.close_open_frames(index);
    if let Some(fail) = pass.violation {
        return Err(fail);
    }
    let top = pass.stack.pop().expect("the view frame remains");
    debug_assert!(pass.stack.is_empty(), "every group closes inside the view");
    Ok((top.pieces, index))
}

/// Reads the `where` at `where_index`, which ends the body that `scope`'s
/// view holds before it: its bindings, read as a frame reads them, and the
/// token where they end, before anything that follows them.
pub(super) fn where_group(
    scope: Scope<'_, '_, '_>,
    where_index: usize,
) -> Parse<(Bindings, usize)> {
    let view = scope.view;
    super::prescan(view)?;
    let mut base = Frame::new(
        FrameKind::View,
        scope.mode,
        scope.context,
        None,
        false,
        false,
    );
    // The body before the `where` stands for the view's first token.
    base.first = Some(where_index);
    let mut pass = Pass {
        scope,
        stack: vec![base],
        stops_at_arrow: false,
        violation: None,
    };
    pass.open_where(where_index);
    let end = pass.walk(where_index + 1)?;
    pass.close_open_frames(end);
    if let Some(fail) = pass.violation {
        return Err(fail);
    }
    let top = pass.stack.pop().expect("the view frame remains");
    let pieces = trim_padding(view, &top.pieces);
    let Some((Piece::Form { form, .. }, rest)) = pieces.split_first() else {
        unreachable!("a `where` delivers its bindings first");
    };
    let end = rest.first().map_or(end, piece_token);
    match scope.covers.take_form(*form).result? {
        SyntaxExpr::Let { bindings, .. } => Ok((bindings, end)),
        _ => unreachable!("a `where` holds its bindings around a placeholder"),
    }
}

struct Pass<'s, 'lex, 'source> {
    scope: Scope<'s, 'lex, 'source>,
    stack: Vec<Frame>,
    stops_at_arrow: bool,
    /// The first break of the grammar review's rules, which fails the view.
    violation: Option<Fail>,
}

impl Pass<'_, '_, '_> {
    /// Walks the view's tokens from `index`, returning where the walk ended:
    /// the view's end, or a token that stops it.
    fn walk(&mut self, mut index: usize) -> Parse<usize> {
        let view = self.scope.view;
        while index < view.range().end() {
            match token(view, index).kind() {
                TokenKind::Open { group, delimiter } => {
                    self.open_group(index, *group, *delimiter)?;
                }
                TokenKind::Close { .. } => self.close_group(index),
                _ => match self.token(index) {
                    Step::Next => {}
                    Step::Skip => index += 1,
                    Step::Stop => break,
                },
            }
            index += 1;
        }
        Ok(index)
    }

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
                bindings: None,
                members: None,
            })),
            Some(Pending::Arms(mut arms)) => {
                arms.begin(0);
                Some(Box::new(Block {
                    head: arms.head,
                    context: arms.context,
                    arms: Some(arms),
                    bindings: None,
                    members: None,
                }))
            }
            // A `let`'s bindings read as expressions, and own their arrows.
            Some(Pending::Bindings(head, kind)) => Some(Box::new(Block {
                head,
                context,
                arms: None,
                bindings: Some(kind),
                members: None,
            })),
            // An object's or update's members, which the declaration parser
            // reads from their views.
            Some(Pending::Members(head, members)) => Some(Box::new(Block {
                head,
                context: members_context(view, context, head),
                arms: None,
                bindings: None,
                members: Some(members),
            })),
            None => None,
        };
        let raw = block.as_ref().is_some_and(|block| block.members.is_some());
        let (mode, arrow) = match &block {
            Some(block) if block.bindings.is_some() || block.members.is_some() => {
                (Mode::Expression, false)
            }
            Some(_) => (Mode::Cover, true),
            None => (mode, mode == Mode::Cover),
        };
        let mut frame = Frame::new(
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
        );
        frame.raw = raw;
        self.push(frame);
        Ok(())
    }

    /// Pushes `frame` with its owners: the layout owner that judges its
    /// lines, the match whose `as` and the arms block whose marks it may
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
            FrameKind::Conditional(_) | FrameKind::Scoped(_) => (layout, matches, arms, level),
            // An object's header holds no match's `as` and no arm's marks.
            FrameKind::Object(_) => (layout, None, None, level),
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
                // Nested arms end their arm, so what follows a braced block
                // without arm marks on its line was meant as a guard binding
                // a dictionary.
                let marks = frame.pieces.iter().any(|piece| {
                    matches!(piece, Piece::Token(at)
                        if matches!(token(view, *at).kind(), TokenKind::Symbol("=>" | "=>?")))
                });
                if kind == ArmsKind::Nested
                    && !marks
                    && continues_tail(view, index + 1)
                    && !matches!(token(view, index + 1).kind(), TokenKind::Symbol(";"))
                {
                    self.violate(
                        Fail::error(
                            "a `{` after `when` begins nested arms; parenthesize a dictionary pattern that begins a guard",
                        )
                        .or_at(Some(open)),
                    );
                }
                let result =
                    self.finish_braced_arms(*arms, &frame.pieces, frame.pending, open, index);
                self.top().line_starts += line_starts;
                self.deliver_arms(kind, head, result, index + 1, true);
                return;
            }
            if let Some(members) = block.members.take() {
                let result = self.finish_braced_members(&block, open, index);
                self.top().line_starts += line_starts;
                self.deliver_members(block.head, members, result, index + 1, true);
                return;
            }
            if let Some(kind) = block.bindings {
                let result = self.finish_braced_bindings(&block, kind, &frame.pieces, open, index);
                self.top().line_starts += line_starts;
                match kind {
                    BindingsKind::Let => self.deliver_bindings(result, false, None),
                    BindingsKind::Where => self.deliver_where(block.head, result, index + 1),
                }
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
                    ends_with_block: true,
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
        } else if view_level
            && delimiter == Delimiter::Parenthesis
            && !legacy_grammar()
            && !body.pieces.iter().any(|piece| is_symbol(view, piece, ","))
        {
            // A parenthesized expression is a body, whose keyword forms
            // that are no frames yet its structural parser reads, and never
            // a pattern, which holds no keyword form.
            let contents = view
                .group_contents(group)
                .expect("an open frame's group exists");
            covers.ready(
                parse_body(contents, body.context.complete())
                    .map(|expression| Cover::Paren(ParenCover::Grouped(Term::Expr(expression))))
                    .unwrap_or_else(|diagnostics| {
                        Cover::Invalid(Fail::Reported(diagnostics).or_at(Some(open)))
                    }),
            )
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
                            let message = match layout.bindings {
                                Some(kind) => format!(
                                    "`{}` must be separated from its bindings",
                                    kind.keyword()
                                ),
                                None => "`do` must be separated from its body".to_owned(),
                            };
                            layout
                                .failure
                                .get_or_insert(Fail::error(message).or_at(Some(layout.head)));
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
            FrameKind::Scoped(scoped) => {
                let owner = scoped.context.child_owner_at(view, index);
                top.context = if scoped.phase == ScopedPhase::Body {
                    let owner = owner.complete_keeping_arrow();
                    scoped.body_context = Some(owner);
                    owner.validated()
                } else {
                    owner.complete().validated()
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
                    top.lambda = None;
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
                top.lambda = None;
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
        if top.raw {
            self.significant(index);
            self.top().pieces.push(Piece::Token(index));
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
        let after_operand = matches!(kind, TokenKind::Name("if" | "try")) && self.follows_operand();
        let begins_member = {
            let top = self.top();
            top.first.is_none() || top.member_start
        };
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
                    self.open_conditional(index, after_operand);
                    return Step::Next;
                }
                "let"
                    if heads_form(view, index)
                        && in_keyword_position(view, index)
                        && self.form_follows(index)
                        && (!legacy_grammar() || self.legacy_let(begins_member)) =>
                {
                    self.open_let(index);
                    return Step::Next;
                }
                "using" if may_head_keyword_form(view, index) => {
                    self.open_using(index);
                    return Step::Next;
                }
                "in" if in_keyword_position(view, index) => return self.scoped_in(index),
                "do" if may_head_keyword_form(view, index) => {
                    // A `do` after a `using`'s namespace begins its body
                    // (`using Dict do Body` is `using Dict in do Body`).
                    if matches!(&self.top().kind,
                        FrameKind::Scoped(scoped) if scoped.phase == ScopedPhase::Namespace)
                        && !trim_padding(view, &self.top().pieces).is_empty()
                    {
                        self.begin_scoped_body(index, index);
                        self.significant(index);
                    }
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
                // The first ungrouped `as` after a match's head is its own.
                "as" if in_keyword_position(view, index) && self.claim_as(index) => {
                    return Step::Next;
                }
                "with" if in_keyword_position(view, index) => return self.with_keyword(index),
                "object" | "abstract"
                    if heads_form(view, index)
                        && in_keyword_position(view, index)
                        && self.object_follows(index)
                        && (!legacy_grammar() || self.legacy_let(begins_member)) =>
                {
                    return self.open_object(index);
                }
                "when" if in_keyword_position(view, index) && self.arm_mark(index) => {
                    return Step::Next;
                }
                // An `and` in guards ends a clause, and the `else` tails
                // open in it.
                "and" if in_keyword_position(view, index) && self.guards_below_tails() => {
                    while !self.top().in_guards() {
                        self.close_top(index);
                    }
                    self.lambda_ends_at(index);
                    let top = self.top();
                    top.lambda_head = false;
                    top.lambda = None;
                    top.member_start = true;
                    top.pieces.push(Piece::Token(index));
                    return Step::Next;
                }
                "where" if in_keyword_position(view, index) && !legacy_grammar() => {
                    return self.where_ends(index, begins_member);
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
        if matches!(kind, TokenKind::Symbol("\\")) {
            top.lambda.get_or_insert(index);
        }
        top.lambda_head = match kind {
            TokenKind::Symbol("\\") => true,
            TokenKind::Name(_) => top.lambda_head,
            _ => false,
        };
        top.pieces.push(Piece::Token(index));
        Step::Next
    }

    /// Whether the `do` at `index` heads a braced block: a `{` follows it on
    /// its line, separated from it as the `do` form requires. The legacy
    /// grammar also took a `{` on the next line.
    fn braced_do(&self, index: usize) -> bool {
        let view = self.scope.view;
        let next = if legacy_grammar() {
            self.next_significant(index + 1)
        } else {
            Some(index + 1).filter(|next| view.range().contains(*next))
        };
        next.is_some_and(|next| {
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
        // A match's `as` and an arm's marks end the region, as they ended
        // the view its form was parsed from.
        let taken = match kind {
            TokenKind::Name("as") => in_keyword_position(view, index) && self.claim_as(index),
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

    fn open_conditional(&mut self, index: usize, after_operand: bool) {
        let view = self.scope.view;
        let keyword = match token(view, index).kind() {
            TokenKind::Name("try") => "try",
            _ => "if",
        };
        if after_operand && keyword == "try" {
            self.violate(conditional_argument(index, keyword));
        }
        let parent = self.top();
        parent.lambda_head = false;
        let (context, separator, arrow) = (parent.context, parent.separator, parent.nested_arrow());
        let mut frame = Frame::new(
            FrameKind::Conditional(Box::new(Conditional {
                head: index,
                keyword,
                after_operand,
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
            if matches!(top.kind, FrameKind::Layout(_) | FrameKind::Match(_))
                || matches!(&top.kind, FrameKind::Scoped(scoped) if scoped.phase == ScopedPhase::Body)
            {
                // A layout block and a `let`'s or `using`'s body are open
                // tails too, and a match without its arms ends unfinished.
                self.close_top(index);
                continue;
            }
            // After an operand, an `if` reads as postfix, so a `then` there
            // makes it an argument (see `open_conditional`).
            if let Some(FrameKind::Conditional(conditional)) =
                self.stack.last().map(|top| &top.kind)
                && is_then
                && conditional.phase == Phase::Guards
                && conditional.after_operand
            {
                let fail = conditional_argument(conditional.head, conditional.keyword);
                self.violate(fail);
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
            .find(|frame| !frame.open_tail())
            .is_some_and(Frame::in_guards)
    }

    /// Notes a tail form at `head` whose last part ends on its line before
    /// the token at `end`, returning whether a token that could continue it
    /// ends it there. Only the end of its region may follow such a form
    /// (Syntax.md "Tail Position").
    fn ends_inline(&mut self, label: &str, head: usize, end: usize) -> bool {
        let view = self.scope.view;
        let inside = continues_tail(view, end);
        if inside {
            let follower = view
                .token_at(end)
                .and_then(|token| view.source().source_slice(token.span()))
                .unwrap_or("?");
            self.violate(misplaced_tail(head, label, &format!("`{follower}`")));
        }
        inside
    }

    /// Records a break of the grammar review's rules, unless the oracle
    /// reads the legacy grammar; the first one fails the view.
    fn violate(&mut self, fail: Fail) {
        if self.violation.is_none() && !legacy_grammar() {
            self.violation = Some(fail);
        }
    }

    /// A `where` ends the definition, statement or arm it stands in, or a
    /// parenthesized expression (Syntax.md "Local Definitions"): the open
    /// tails before it close, and the pass stops at it when the view is that
    /// body. A statement or arm holding one is parsed from its view, and so
    /// are a group's contents. Elsewhere it would scope only part of an
    /// expression, which parentheses or `let … in` say.
    fn where_ends(&mut self, index: usize, begins_member: bool) -> Step {
        while self.top().open_tail() {
            self.close_top(index);
        }
        let single = self.stack.len() == 1;
        let top = self.top();
        let block = matches!(
            &top.kind,
            FrameKind::Layout(_) | FrameKind::Group { block: Some(_), .. }
        );
        let misplaced = match &top.kind {
            _ if begins_member && block => None,
            FrameKind::Conditional(conditional) if conditional.phase == Phase::Guards => {
                Some("a conditional's guards")
            }
            FrameKind::Conditional(_) => Some("a `then` branch"),
            FrameKind::Match(_) => Some("a match's subject"),
            FrameKind::Scoped(scoped) if scoped.phase == ScopedPhase::Namespace => {
                Some("a `using` namespace")
            }
            FrameKind::Scoped(_) => Some("a `let`'s bindings"),
            _ if top
                .arms()
                .is_some_and(|arms| arms.phase != ArmPhase::Result) =>
            {
                Some("an arm's pattern or guards")
            }
            FrameKind::Group {
                delimiter, block, ..
            } if block.is_none() && *delimiter != Delimiter::Parenthesis => {
                Some("a list or dictionary item")
            }
            FrameKind::View if top.mode == Mode::Cover => Some("a pattern"),
            _ => None,
        };
        if let Some(place) = misplaced {
            self.violate(
                Fail::error(format!(
                    "`where` cannot stand in {place}: it scopes a whole definition, statement or arm; parenthesize `(… where …)` or use `let … in` for part of an expression"
                ))
                .or_at(Some(index)),
            );
        } else if begins_member && block {
            self.violate(
                Fail::error(
                    "`where` cannot begin a statement, arm or binding: indent it to scope the one above, or dedent it below the block to scope the definition",
                )
                .or_at(Some(index)),
            );
        } else if single && !begins_member {
            return Step::Stop;
        } else if !begins_member && self.form_follows(index) {
            self.open_where(index);
            return Step::Next;
        }
        let top = self.top();
        top.view_level = true;
        top.lambda_head = false;
        top.pieces.push(Piece::Token(index));
        Step::Next
    }

    /// Notes a lambda open in the top frame's member, whose body the keyword
    /// or arrow at `index` would end.
    fn lambda_ends_at(&mut self, index: usize) {
        if let Some(lambda) = self.stack.last().and_then(|frame| frame.lambda) {
            self.ends_inline("lambda", lambda, index);
        }
    }

    /// Whether the top frame's current member ends with an operand, which
    /// an `if` or `try` here would follow.
    fn follows_operand(&self) -> bool {
        let view = self.scope.view;
        let Some(top) = self.stack.last() else {
            return false;
        };
        if top.member_start || top.first.is_none() {
            return false;
        }
        let last = top.pieces.iter().rev().find(|piece| {
            !matches!(piece, Piece::Token(index)
                if matches!(token(view, *index).kind(), TokenKind::LineStart { .. }))
        });
        match last {
            Some(Piece::Group { .. } | Piece::Form { .. }) => true,
            Some(Piece::Token(index)) => ends_operand(view, *index),
            None => false,
        }
    }

    /// Whether `pieces` end with a form whose last part ended at its own `}`
    /// or a dedent.
    fn ends_with_block(&self, pieces: &[Piece]) -> bool {
        let view = self.scope.view;
        let last = pieces.iter().rev().find(|piece| {
            !matches!(piece, Piece::Token(index)
                if matches!(token(view, *index).kind(), TokenKind::LineStart { .. }))
        });
        matches!(last, Some(Piece::Form { form, .. })
            if self.scope.covers.form_ends_with_block(*form))
    }

    /// Closes conditionals whose `else` branch ends at the `->` at `index`.
    fn close_tails_at_arrow(&mut self, index: usize) {
        while let Some(top) = self.stack.last()
            && top.arrow_ends_tails
            && top.open_tail()
        {
            self.close_top(index);
        }
    }

    /// Closes every conditional, layout block and match above the innermost
    /// group or the view.
    fn close_open_frames(&mut self, index: usize) {
        while matches!(
            self.stack.last().map(|frame| &frame.kind),
            Some(
                FrameKind::Conditional(_)
                    | FrameKind::Layout(_)
                    | FrameKind::Match(_)
                    | FrameKind::Scoped(_)
                    | FrameKind::Object(_)
            )
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
            Some(FrameKind::Match(_)) => self.close_match(index, None, false),
            Some(FrameKind::Scoped(_)) => self.close_scoped(index),
            Some(FrameKind::Object(_)) => self.close_object(index),
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
                if let FrameKind::Layout(layout) = &mut self.stack[owner].kind
                    && (hanging || layout.bindings.is_some())
                {
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
                bindings: None,
                members: None,
            })),
            // Statements may bind patterns, and end their tails at `->`.
            Mode::Cover,
            context,
            separator,
            true,
            false,
        ));
    }

    /// Whether anything of a form follows its head at `index` in its group
    /// member; a lone keyword reads as a name, which the item reports.
    fn form_follows(&self, index: usize) -> bool {
        let view = self.scope.view;
        self.next_significant(index + 1).is_some_and(|next| {
            !matches!(
                token(view, next).kind(),
                TokenKind::Close { .. } | TokenKind::Symbol("," | ";")
            )
        })
    }

    /// Whether a `let` heads a form where the legacy grammar's view-level
    /// dispatch parsed one: first in a view it parsed, which a group is
    /// not, unless it is a block.
    fn legacy_let(&mut self, begins_member: bool) -> bool {
        begins_member && !matches!(self.top().kind, FrameKind::Group { block: None, .. })
    }

    /// Opens a `let` at `index`. Its bindings are a block above it: braced
    /// when a `{` follows on its line, otherwise a layout block whose first
    /// binding sets its anchor, as a `do` block's first statement does.
    fn open_let(&mut self, index: usize) {
        let view = self.scope.view;
        let parent = self.top();
        parent.lambda_head = false;
        let context = form_context(view, parent.context, index);
        let (separator, arrow) = (parent.separator, parent.nested_arrow());
        let mut frame = Frame::new(
            FrameKind::Scoped(Box::new(Scoped::new(
                index,
                "let",
                ScopedPhase::Bindings,
                context,
            ))),
            Mode::Expression,
            context,
            separator,
            arrow,
            false,
        );
        frame.first = Some(index);
        self.push(frame);
        // The legacy grammar skipped line breaks to find the `{`.
        let next = if legacy_grammar() {
            self.next_significant(index + 1)
        } else {
            Some(index + 1).filter(|next| view.range().contains(*next))
        };
        if next.is_some_and(|next| {
            matches!(
                token(view, next).kind(),
                TokenKind::Open {
                    delimiter: Delimiter::Brace,
                    ..
                }
            )
        }) {
            self.top().pending = Some(Pending::Bindings(index, BindingsKind::Let));
            return;
        }
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
                bindings: Some(BindingsKind::Let),
                members: None,
            })),
            // A binding's value reads as an expression, and owns its arrows.
            Mode::Expression,
            context,
            separator,
            false,
            false,
        ));
    }

    /// Opens a `using` at `index`, whose namespace runs to its `in` or `do`.
    fn open_using(&mut self, index: usize) {
        let view = self.scope.view;
        let parent = self.top();
        parent.lambda_head = false;
        let context = form_context(view, parent.context, index);
        let (separator, arrow) = (parent.separator, parent.nested_arrow());
        let mut frame = Frame::new(
            FrameKind::Scoped(Box::new(Scoped::new(
                index,
                "using",
                ScopedPhase::Namespace,
                context,
            ))),
            Mode::Expression,
            context,
            separator,
            arrow,
            false,
        );
        frame.first = Some(index);
        frame.member_start = true;
        self.push(frame);
    }

    /// An `in` in keyword position, for the innermost `let` or `using` at
    /// this level still reading its bindings or namespace: the open tails
    /// and the bindings block before it close, and its body begins. A
    /// finished form's body ends at it.
    fn scoped_in(&mut self, index: usize) -> Step {
        let view = self.scope.view;
        loop {
            let top = self.top();
            let empty = trim_padding(view, &top.pieces).is_empty();
            let tail = top.open_tail();
            match &mut top.kind {
                FrameKind::Scoped(scoped) if scoped.phase != ScopedPhase::Body => break,
                // A multi-line `let` whose body a line began, as an `in`.
                FrameKind::Scoped(scoped) if scoped.multiline && empty => {
                    scoped.fail(scoped.head, "multi-line let expression must not use `in`");
                    return Step::Next;
                }
                FrameKind::Layout(_) | FrameKind::Match(_) => self.close_top(index),
                _ if tail => self.close_top(index),
                _ => {
                    // A stray keyword, which the item reports.
                    top.lambda_head = false;
                    top.pieces.push(Piece::Token(index));
                    return Step::Next;
                }
            }
        }
        if let FrameKind::Scoped(scoped) = &mut self.top().kind
            && scoped.multiline
        {
            scoped.fail(scoped.head, "multi-line let expression must not use `in`");
        }
        self.begin_scoped_body(index, index + 1);
        Step::Next
    }

    /// Ends the bindings or namespace of the `let` or `using` on top at the
    /// keyword at `index`, and begins its body at `start`.
    fn begin_scoped_body(&mut self, index: usize, start: usize) {
        let view = self.scope.view;
        let top = self.stack.last().expect("a scoped form is open");
        let FrameKind::Scoped(scoped) = &top.kind else {
            unreachable!("only a scoped form begins its body here");
        };
        let (phase, head, context) = (scoped.phase, scoped.head, scoped.context);
        let namespace = (phase == ScopedPhase::Namespace)
            .then(|| self.namespace(context, head, &top.pieces, index));
        let stray = phase == ScopedPhase::Bindings && !trim_padding(view, &top.pieces).is_empty();
        let top = self.top();
        let FrameKind::Scoped(scoped) = &mut top.kind else {
            unreachable!("only a scoped form begins its body here");
        };
        match phase {
            ScopedPhase::Bindings => {
                if stray {
                    scoped.fail(
                        head,
                        "a `let`'s braced bindings end at their `}`, which `in` must follow",
                    );
                }
                if scoped.bindings.is_none() {
                    scoped.fail(head, "let expression requires at least one binding");
                }
            }
            ScopedPhase::Namespace => scoped.namespace = namespace,
            ScopedPhase::Body => unreachable!("a body begins once"),
        }
        scoped.phase = ScopedPhase::Body;
        scoped.body_start = start;
        scoped.line_starts += top.line_starts;
        top.pending = None;
        top.begin_branch(context);
    }

    /// A `using`'s namespace, from its pieces before the `in` or `do` at
    /// `end`.
    fn namespace(
        &self,
        context: ExpressionContext,
        head: usize,
        pieces: &[Piece],
        end: usize,
    ) -> Parse<SyntaxExpr> {
        let view = self.scope.view;
        let pieces = trim_padding(view, pieces);
        let Some(first) = pieces.first().map(piece_token) else {
            return Err(
                Fail::error("using expression requires a namespace before its body")
                    .or_at(Some(head)),
            );
        };
        let scope = Scope {
            view: subview(view, first, end),
            mode: Mode::Expression,
            ..self.scope
        };
        binding::expression(
            scope,
            pieces,
            context.child_owner_at(view, first),
            false,
            false,
        )
        .map(|(expression, _)| expression)
    }

    /// A `with` in keyword position: an object's body after its header, or
    /// an update's after its base and any `as Name` alias. The legacy
    /// grammar's base was everything before the `with`, so it ended the
    /// open tails before it.
    fn with_keyword(&mut self, index: usize) -> Step {
        let view = self.scope.view;
        if let FrameKind::Object(object) = &mut self.top().kind
            && object.with.is_none()
        {
            object.with = Some(index);
            self.open_members(index, Members::Object);
            return Step::Next;
        }
        // A lone keyword reads as a name, which the item reports.
        if !self.form_follows(index) {
            let top = self.top();
            top.lambda_head = false;
            top.pieces.push(Piece::Token(index));
            return Step::Next;
        }
        if legacy_grammar() && self.legacy_with(index) {
            return Step::Next;
        }
        // `Dict as Name with Defs` names the update's base.
        let top = self.top();
        let pieces = &top.pieces;
        let significant = pieces
            .iter()
            .rev()
            .filter(|piece| {
                !matches!(piece, Piece::Token(at)
                    if matches!(token(view, *at).kind(), TokenKind::LineStart { .. }))
            })
            .take(2)
            .collect::<Vec<_>>();
        let alias = match significant.as_slice() {
            [Piece::Token(name), Piece::Token(as_index)]
                if matches!(token(view, *as_index).kind(), TokenKind::Name("as"))
                    && in_keyword_position(view, *as_index) =>
            {
                let name_view = subview(view, *name, name + 1);
                if object_alias_name(name_view) == Some("_") {
                    Some(None)
                } else {
                    object_alias_name(name_view).map(|name| Some(name.to_owned()))
                }
            }
            _ => None,
        };
        let alias = match alias {
            Some(alias) => {
                // The alias is the update's, not its base's.
                while let Some(Piece::Token(last)) = top.pieces.last()
                    && !matches!(token(view, *last).kind(), TokenKind::Name("as"))
                {
                    top.pieces.pop();
                }
                top.pieces.pop();
                alias
            }
            None => None,
        };
        top.lambda_head = false;
        self.open_members(index, Members::Update { alias });
        Step::Next
    }

    /// Reads the `with` at `index` as the legacy grammar's view-level
    /// dispatch did, returning whether it kept the `with` a token. That
    /// dispatch searched a view's tokens outside groups, up to a match's
    /// head, for its first `with`, which took everything before it as its
    /// base, unless a form led the view that owned what followed it. A
    /// `with` in a group was a reserved word, or the group's own view's.
    fn legacy_with(&mut self, index: usize) -> bool {
        let view = self.scope.view;
        if self
            .stack
            .iter()
            .skip(1)
            .any(|frame| matches!(frame.kind, FrameKind::Group { block: None, .. }))
        {
            let top = self.top();
            top.view_level = true;
            top.lambda_head = false;
            top.pieces.push(Piece::Token(index));
            return true;
        }
        let owned = self.stack[0].first.is_some_and(|first| {
            matches!(
                token(view, first).kind(),
                TokenKind::Name(
                    "if" | "try" | "match" | "try_match" | "let" | "object" | "abstract"
                )
            )
        });
        let searched = !owned
            && self.stack.iter().skip(1).all(|frame| {
                !matches!(frame.kind, FrameKind::Match(_) | FrameKind::Group { .. })
                    && frame.arms().is_none()
            });
        if searched {
            while self.stack.len() > 1 {
                self.close_top(index);
            }
        }
        false
    }

    /// Opens the body after the `with` at `index`: braced when a `{`
    /// follows on its line, otherwise a layout block, hanging when its first
    /// member follows the `with` on its line. Its members are only kept for
    /// the declaration parser, which reads them from their views.
    fn open_members(&mut self, index: usize, members: Members) {
        let view = self.scope.view;
        // The legacy grammar skipped line breaks to find the `{`.
        let next = if legacy_grammar() {
            self.next_significant(index + 1)
        } else {
            Some(index + 1).filter(|next| view.range().contains(*next))
        };
        let parent = self.top();
        if next.is_some_and(|next| {
            matches!(
                token(view, next).kind(),
                TokenKind::Open {
                    delimiter: Delimiter::Brace,
                    ..
                }
            )
        }) {
            parent.pending = Some(Pending::Members(index, members));
            return;
        }
        let context = members_context(view, parent.context, index);
        let separator = parent.separator;
        let mut frame = Frame::new(
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
                bindings: None,
                members: Some(members),
            })),
            Mode::Expression,
            context,
            separator,
            false,
            false,
        );
        frame.raw = true;
        self.push(frame);
    }

    /// A layout block's members, from the `with` at `head` to `end`, read
    /// by the declaration parser, with the structural parser's checks.
    fn finish_layout_members(
        &self,
        layout: &LayoutBlock,
        members: &Members,
        end: usize,
    ) -> Parse<Vec<ObjectBodyDefinition>> {
        let view = self.scope.view;
        let head = layout.head;
        if layout.anchor.is_none() && *members == Members::Object {
            return Err(Fail::error(
                "object `with` requires a body; use `with {}` for an explicit empty body",
            )
            .or_at(Some(head)));
        }
        if let Some(anchor) = layout.anchor {
            if !layout.context.accepts_layout_anchor(anchor) {
                let first = layout.statements.first().copied();
                return Err(Fail::error(format!(
                    "nested layout begins at indentation {anchor}; expected more than continuation floor {}",
                    layout.context.continuation_floor()
                ))
                .or_at(first.map_or(Some(head + 1), |_| self.next_significant(head + 1))));
            }
            if layout.hanging
                && let Some(line) = layout.boundary.filter(|line| *line == end)
                && line_begins_object_member(line_view(structural_view_at(view, head), line))
            {
                let indentation = match token(view, line).kind() {
                    TokenKind::LineStart { indentation } => *indentation,
                    _ => 0,
                };
                return Err(Fail::error(format!(
                    "hanging `with` member is indented {indentation} spaces; expected sibling indentation {anchor}"
                ))
                .or_at(Some(line)));
            }
        }
        let line = view
            .token_at(head)
            .and_then(|token| view.line_at_span(token.span()))
            .unwrap_or(1);
        let empty = match members {
            Members::Update { .. } => {
                "`with` body cannot be empty; use `with {}` for an explicit empty body"
            }
            Members::Object => {
                "object `with` body cannot be empty; use `with {}` for an explicit empty body"
            }
        };
        let body = subview(view, head + 1, end.max(head + 1));
        let mut diagnostics = Vec::new();
        let parsed =
            parse_nonempty_object_body(body, line, empty, layout.context, &mut diagnostics);
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            return Err(Fail::Reported(diagnostics));
        }
        Ok(parsed.unwrap_or_default())
    }

    /// A braced block's members, read by the declaration parser.
    fn finish_braced_members(
        &self,
        block: &Block,
        open: usize,
        close: usize,
    ) -> Parse<Vec<ObjectBodyDefinition>> {
        let view = self.scope.view;
        let mut diagnostics = Vec::new();
        let body = parse_object_body(
            subview(view, open, close + 1),
            block.context,
            &mut diagnostics,
        );
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            return Err(Fail::Reported(diagnostics));
        }
        Ok(body)
    }

    /// Hands a closed block's members to their owner: an update becomes a
    /// form piece whose base the item fills in, and an object closes.
    fn deliver_members(
        &mut self,
        head: usize,
        members: Members,
        body: Parse<Vec<ObjectBodyDefinition>>,
        end: usize,
        ends_with_block: bool,
    ) {
        match members {
            Members::Update { alias } => {
                let covers = self.scope.covers;
                let parent = self.top();
                parent.lambda_head = false;
                parent.pieces.push(Piece::Form {
                    head,
                    end,
                    form: covers.form(Form {
                        head,
                        keyword: "with",
                        shape: FormShape::With,
                        result: body.map(|body| SyntaxExpr::With {
                            base: Box::new(SyntaxExpr::Unit),
                            alias,
                            body,
                        }),
                        ends_with_block,
                    }),
                });
            }
            Members::Object => {
                let FrameKind::Object(object) = &mut self.top().kind else {
                    unreachable!("an object's members close into it");
                };
                object.body = Some(body);
                self.close_object_ending(end, ends_with_block);
            }
        }
    }

    /// Whether an object follows the `object` or `abstract` at `index`: an
    /// `abstract` heads one only before `object`, and a lone keyword reads
    /// as a name.
    fn object_follows(&self, index: usize) -> bool {
        let view = self.scope.view;
        match token(view, index).kind() {
            TokenKind::Name("abstract") => self.next_significant(index + 1).is_some_and(|next| {
                matches!(token(view, next).kind(), TokenKind::Name("object"))
                    && in_keyword_position(view, next)
            }),
            _ => self.form_follows(index),
        }
    }

    /// Opens an object expression at its `object`, or the `abstract` before
    /// it.
    fn open_object(&mut self, index: usize) -> Step {
        let view = self.scope.view;
        let parent = self.top();
        parent.lambda_head = false;
        let context = form_context(view, parent.context, index);
        let (separator, arrow) = (parent.separator, parent.nested_arrow());
        let mut frame = Frame::new(
            FrameKind::Object(Box::new(ObjectForm {
                head: index,
                context,
                body: None,
                with: None,
            })),
            Mode::Expression,
            context,
            separator,
            arrow,
            false,
        );
        frame.first = Some(index);
        frame.member_start = true;
        self.push(frame);
        // `abstract object` is one head.
        match token(view, index).kind() {
            TokenKind::Name("abstract") => match self.next_significant(index + 1) {
                Some(object) if object == index + 1 => Step::Skip,
                _ => Step::Next,
            },
            _ => Step::Next,
        }
    }

    /// Closes the object on top, which ends before `index` without a body:
    /// its header is an open tail.
    fn close_object(&mut self, index: usize) {
        let FrameKind::Object(object) = &self.stack.last().expect("an object is open").kind else {
            unreachable!("only objects close here");
        };
        let head = object.head;
        self.ends_inline(&form_label("object"), head, index);
        self.close_object_ending(index, false);
    }

    /// Closes the object on top, which ends before `end`, into a form piece
    /// of its parent: its header read from its view, with its body.
    fn close_object_ending(&mut self, end: usize, ends_with_block: bool) {
        let view = self.scope.view;
        let frame = self.stack.pop().expect("an object is open");
        let FrameKind::Object(object) = frame.kind else {
            unreachable!("only objects close here");
        };
        let head = object.head;
        let header_end = object.with.map_or(end, |with| with + 1);
        let header = trim_layout(subview(view, head, header_end.max(head + 1)));
        let result = parse_object_expression_header(header, object.context)
            .map_err(Fail::Reported)
            .and_then(|mut expression| {
                if let Some(body) = object.body {
                    expression.body = body?;
                }
                Ok(SyntaxExpr::Object(expression))
            });
        let covers = self.scope.covers;
        let parent = self.top();
        parent.line_starts += frame.line_starts;
        parent.lambda_head = false;
        parent.pieces.push(Piece::Form {
            head,
            end,
            form: covers.form(Form {
                head,
                keyword: "object",
                shape: FormShape::Prefix,
                result,
                ends_with_block,
            }),
        });
    }

    /// Opens the bindings of the `where` at `index`, which ends the body on
    /// top: a braced block when a `{` follows it on its line, otherwise a
    /// layout block, hanging when its first binding follows it on its line.
    fn open_where(&mut self, index: usize) {
        let view = self.scope.view;
        let parent = self.top();
        parent.lambda_head = false;
        parent.lambda = None;
        let (context, separator) = (parent.context, parent.separator);
        let braced = view.range().contains(index + 1)
            && matches!(
                token(view, index + 1).kind(),
                TokenKind::Open {
                    delimiter: Delimiter::Brace,
                    ..
                }
            );
        if braced {
            parent.pending = Some(Pending::Bindings(index, BindingsKind::Where));
            return;
        }
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
                bindings: Some(BindingsKind::Where),
                members: None,
            })),
            Mode::Expression,
            context,
            separator,
            false,
            false,
        ));
    }

    /// Hands a closed `where`'s bindings to the body below it, as a form
    /// piece that ends the body (see `binding::split_where`). Its result
    /// holds the bindings around a placeholder body.
    fn deliver_where(&mut self, head: usize, bindings: Parse<Bindings>, end: usize) {
        let covers = self.scope.covers;
        let parent = self.top();
        parent.lambda_head = false;
        parent.pieces.push(Piece::Form {
            head,
            end,
            form: covers.form(Form {
                head,
                keyword: "where",
                shape: FormShape::Where,
                result: bindings.map(|bindings| SyntaxExpr::Let {
                    bindings,
                    body: Box::new(SyntaxExpr::Unit),
                }),
                ends_with_block: false,
            }),
        });
    }

    /// Hands a closed block's bindings to the `let` on top. A layout block
    /// that a shallower line ended at `boundary` begins the body there, a
    /// line aligned with the `let`.
    fn deliver_bindings(
        &mut self,
        bindings: Parse<Bindings>,
        multiline: bool,
        boundary: Option<usize>,
    ) {
        let view = self.scope.view;
        let top = self.top();
        let FrameKind::Scoped(scoped) = &mut top.kind else {
            unreachable!("bindings close into their `let`");
        };
        // A misplaced binding, as on a shallower line, reports itself.
        let misplaced = bindings.is_err();
        scoped.bindings = Some(bindings);
        scoped.multiline = multiline;
        let Some(line) = boundary else {
            return;
        };
        let head = scoped.head;
        let indentation = match token(view, line).kind() {
            TokenKind::LineStart { indentation } => *indentation,
            _ => 0,
        };
        let column = view
            .token_at(head)
            .and_then(|token| view.column_at_span(token.span()))
            .unwrap_or(0);
        if indentation != column && !misplaced {
            scoped.fail(
                line,
                format!(
                    "multi-line let body is indented {indentation} spaces; expected alignment with `let` at indentation {column}"
                ),
            );
        }
        self.begin_scoped_body(line, line);
    }

    /// Closes the layout block on top, which ends before `index`, into a
    /// form piece of its parent.
    fn close_layout(&mut self, index: usize) {
        let frame = self.stack.pop().expect("a layout block is open");
        let line_starts = frame.line_starts;
        let FrameKind::Layout(mut layout) = frame.kind else {
            unreachable!("only layout blocks close here");
        };
        if let Some(members) = layout.members.take() {
            let result = self.finish_layout_members(&layout, &members, index);
            self.top().line_starts += line_starts;
            let ends_with_block = !self.ends_inline(&form_label("with"), layout.head, index);
            self.deliver_members(layout.head, members, result, index, ends_with_block);
            return;
        }
        if let Some(kind) = layout.bindings {
            let multiline = layout.anchor.is_some()
                && (!layout.hanging
                    || layout.statements.len() > 1
                    || !layout.hanging_lines.is_empty());
            let result = self.finish_layout_bindings(&layout, kind, &frame.pieces);
            self.top().line_starts += line_starts;
            match kind {
                BindingsKind::Let => {
                    let boundary = layout.boundary.filter(|line| *line == index);
                    self.deliver_bindings(result, multiline, boundary);
                }
                BindingsKind::Where => self.deliver_where(layout.head, result, index),
            }
            return;
        }
        if let Some(arms) = layout.arms.take() {
            let (kind, head) = (arms.kind, arms.head);
            let result =
                self.finish_layout_arms(&layout, *arms, &frame.pieces, frame.pending, index);
            self.top().line_starts += line_starts;
            // Nested arms end with their match, which names the form.
            let ends_with_block =
                kind == ArmsKind::Nested || !self.ends_inline(&kind.label(), head, index);
            self.deliver_arms(kind, head, result, index, ends_with_block);
            return;
        }
        let head = layout.head;
        let ends_with_block = !self.ends_inline(&form_label("do"), head, index);
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
                ends_with_block,
            }),
        });
    }

    /// A layout block's bindings, checked as the structural parser checked
    /// them: a deeper line that binds is a misplaced sibling, unless a
    /// `where` begins it, which scopes the binding above.
    fn finish_layout_bindings(
        &self,
        layout: &LayoutBlock,
        kind: BindingsKind,
        pieces: &[Piece],
    ) -> Parse<Bindings> {
        let view = self.scope.view;
        let head = layout.head;
        let keyword = kind.keyword();
        if let Some(failure) = layout.failure.clone() {
            return Err(failure);
        }
        let Some(anchor) = layout.anchor else {
            let message = match kind {
                BindingsKind::Let => "let expression requires at least one binding",
                BindingsKind::Where => {
                    "where expression requires at least one binding or an explicit `{}` group"
                }
            };
            return Err(Fail::error(message).or_at(Some(head)));
        };
        let first = layout.statements[0];
        let first_token = pieces.get(first).map(piece_token);
        if !layout.context.accepts_layout_anchor(anchor) {
            let layout_name = match kind {
                BindingsKind::Let => "let layout",
                BindingsKind::Where => "where binding layout",
            };
            return Err(Fail::error(format!(
                "{layout_name} begins at indentation {anchor}; expected more than continuation floor {}",
                layout.context.continuation_floor()
            ))
            .or_at(first_token));
        }
        // A deeper line that binds was a misplaced sibling to the legacy
        // grammar. Until a `with` or `object` body is a frame whose own
        // lines it judges, such a line may be one of its members, so only
        // the line that ended the block is checked now.
        let extent = structural_view_at(view, head);
        let deeper = layout.hanging_lines.iter().filter(|_| legacy_grammar());
        let lines = deeper.chain(&layout.boundary);
        for line in lines {
            let line_view = line_view(extent, *line);
            let binds = line_view
                .top_level()
                .any(|indexed| matches!(indexed.token().kind(), TokenKind::Symbol("=")));
            if binds && (legacy_grammar() || keyword_head(line_view, "where").is_none()) {
                let indentation = match token(view, *line).kind() {
                    TokenKind::LineStart { indentation } => *indentation,
                    _ => 0,
                };
                return Err(Fail::error(format!(
                    "multi-line {keyword} binding is indented {indentation} spaces; expected sibling indentation {anchor}"
                ))
                .or_at(Some(*line)));
            }
        }
        if let Some(semicolon) = pieces.iter().find(|piece| is_symbol(view, piece, ";")) {
            let braced = match kind {
                BindingsKind::Let => "let { ... } in ...",
                BindingsKind::Where => "where { ... }",
            };
            return Err(Fail::error(format!(
                "naked semicolon-separated `{keyword}` bindings are not supported; use `{braced}`"
            ))
            .or_at(Some(piece_token(semicolon))));
        }
        let bounds = layout
            .statements
            .iter()
            .copied()
            .chain(std::iter::once(pieces.len()))
            .collect::<Vec<_>>();
        let scope = Scope {
            view: subview(view, head + 1, view.range().end()),
            mode: Mode::Expression,
            ..self.scope
        };
        let mut bindings = Vec::with_capacity(layout.statements.len());
        for pair in bounds.windows(2) {
            let binding = trim_padding(view, &pieces[pair[0]..pair[1]]);
            let Some(first) = binding.first().map(piece_token) else {
                continue;
            };
            let context = layout.context.child_owner_at(view, first);
            bindings.push(binding::local_binding(scope, binding, context)?);
        }
        if bindings.is_empty() {
            return Err(Fail::error(format!(
                "{keyword} expression requires at least one binding"
            ))
            .or_at(Some(head)));
        }
        Ok(bindings)
    }

    /// A braced block's bindings, separated by `;`, of which one leading
    /// and one trailing may stand alone.
    fn finish_braced_bindings(
        &self,
        block: &Block,
        kind: BindingsKind,
        pieces: &[Piece],
        open: usize,
        close: usize,
    ) -> Parse<Bindings> {
        let view = self.scope.view;
        let keyword = kind.keyword();
        let fail = |message: String| Err(Fail::error(message).or_at(Some(open)));
        if trim_padding(view, pieces).is_empty() {
            return Ok(Vec::new());
        }
        let is_semicolon = |piece: &Piece| {
            matches!(piece, Piece::Token(index)
                if matches!(token(view, *index).kind(), TokenKind::Symbol(";")))
        };
        let members = pieces.split(is_semicolon).collect::<Vec<_>>();
        let empty = |member: &&[Piece]| trim_padding(view, member).is_empty();
        if members.iter().all(empty) {
            return fail(format!(
                "`{keyword}` binding group cannot contain only semicolons; use `{{}}` for an empty body"
            ));
        }
        let last = members.len() - 1;
        if members
            .iter()
            .enumerate()
            .any(|(at, member)| empty(member) && at != 0 && at != last)
        {
            return fail(format!(
                "`{keyword}` binding group contains an empty member between semicolons"
            ));
        }
        let scope = Scope {
            view: subview(view, open + 1, close),
            mode: Mode::Expression,
            ..self.scope
        };
        let context = block.context;
        members
            .into_iter()
            .map(|member| trim_padding(view, member))
            .filter(|member| !member.is_empty())
            .map(|member| {
                let first = piece_token(&member[0]);
                binding::local_binding(scope, member, context.child_owner_at(view, first))
            })
            .collect()
    }

    /// Closes the `let` or `using` on top, which ends before `index`, into a
    /// form piece of its parent, followed by any pieces of its body after a
    /// `->` where the body ended.
    fn close_scoped(&mut self, index: usize) {
        let frame = self.stack.pop().expect("a scoped form is open");
        let FrameKind::Scoped(scoped) = frame.kind else {
            unreachable!("only scoped forms close here");
        };
        let line_starts = scoped.line_starts + frame.line_starts;
        let (head, keyword) = (scoped.head, scoped.keyword);
        let complete = scoped.phase == ScopedPhase::Body;
        let block = self.ends_with_block(&frame.pieces);
        let (result, rest) = self.finish_scoped(*scoped, frame.pieces, frame.line_starts, index);
        let end = rest.first().map_or(index, piece_token);
        if complete && !block {
            self.ends_inline(&form_label(keyword), head, end);
        }
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
                ends_with_block: complete && block,
            }),
        });
        parent.pieces.extend(rest);
    }

    /// A closed `let` or `using`, and any pieces of its body after a `->`
    /// where the body ended.
    fn finish_scoped(
        &self,
        scoped: Scoped,
        pieces: Vec<Piece>,
        line_starts: usize,
        end: usize,
    ) -> (Parse<SyntaxExpr>, Vec<Piece>) {
        let view = self.scope.view;
        let head = scoped.head;
        let fail = |message: String| (Err(Fail::error(message).or_at(Some(head))), Vec::new());
        if let Some(failure) = scoped.failure {
            return (Err(failure), Vec::new());
        }
        // Bindings that failed explain what follows them.
        if let Some(Err(failure)) = scoped.bindings {
            return (Err(failure), Vec::new());
        }
        match scoped.phase {
            ScopedPhase::Bindings => {
                return fail("multi-line let expression requires a body".to_owned());
            }
            ScopedPhase::Namespace if trim_padding(view, &pieces).is_empty() => {
                return fail(
                    "using expression requires a namespace and `in` or `do` body".to_owned(),
                );
            }
            ScopedPhase::Namespace => {
                return fail("using expression requires `in` or a trailing `do` body".to_owned());
            }
            ScopedPhase::Body => {}
        }
        if trim_padding(view, &pieces).is_empty() {
            return fail(format!("{} expression requires a body", scoped.keyword));
        }
        let branch = Branch {
            pieces,
            context: scoped.body_context,
            start: scoped.body_start,
            end,
            line_starts,
        };
        let (body, rest) = match self.else_branch(branch, scoped.context) {
            Ok(parsed) => parsed,
            Err(fail) => return (Err(fail), Vec::new()),
        };
        let result = if scoped.keyword == "let" {
            scoped
                .bindings
                .expect("a `let`'s body follows its bindings")
                .map(|bindings| SyntaxExpr::Let {
                    bindings,
                    body: Box::new(body),
                })
        } else {
            scoped
                .namespace
                .expect("a `using`'s body follows its namespace")
                .map(|namespace| SyntaxExpr::Using {
                    namespace: Box::new(namespace),
                    body: Box::new(body),
                })
        };
        (result, rest)
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

    /// Takes the `as` at `index` for the innermost match reading its
    /// subject at this level, if any: the frames open in its subject close
    /// before it, and its subject is read. Returns whether it took it. So an
    /// `as` in a subject, as in `Dict as Name with Defs`, is parenthesized,
    /// as `layout::match_arms_keywords` also reads it.
    fn claim_as(&mut self, index: usize) -> bool {
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
        self.lambda_ends_at(index);
        let scope = self.scope;
        let view = scope.view;
        // `Dict as Name with Defs` names an update's base, which an `as`
        // that ends a subject cannot do.
        let name_with = |at: usize| {
            view.range().contains(at + 2)
                && matches!(token(view, at + 1).kind(), TokenKind::Name(_))
                && matches!(token(view, at + 2).kind(), TokenKind::Name("with"))
                && in_keyword_position(view, at + 2)
        };
        if name_with(index) {
            self.violate(
                Fail::error(
                    "a match's first `as` ends its subject; parenthesize an update that names its base, as in `match (d as n with {…}) as …`",
                )
                .or_at(Some(index)),
            );
        }
        let frame = self.top();
        frame.region = false;
        frame.region_conditionals.clear();
        frame.lambda_head = false;
        frame.lambda = None;
        let pieces = std::mem::take(&mut frame.pieces);
        let FrameKind::Match(found) = &mut frame.kind else {
            unreachable!("a match owns the `as`");
        };
        found.phase = MatchPhase::Arms;
        let subject = trim_padding(view, &pieces);
        found.subject = Some(match subject.first().map(piece_token) {
            None => Err(Fail::error(format!(
                "{} expression requires a subject before `as`",
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
                binding::expression(scope, subject, context, false, false)
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

    /// Starts arms after the `as` or `when` at `keyword`: a braced block
    /// if a `{` comes next, unless that group begins the first arm's
    /// pattern, as a mark after it on its line shows; otherwise a layout
    /// block. The marks on a line where an enclosing arm reads its head or
    /// guards are that arm's, as the structural parser split it first.
    fn begin_arms(&mut self, keyword: usize, arms: Arms) {
        let view = self.scope.view;
        // A `{` on the keyword's line delimits the arms. The legacy grammar
        // skipped line breaks to find it, and read it as a first arm's
        // dictionary pattern when a mark followed the group on its line,
        // unless an enclosing arm read its head or guards there.
        let legacy = legacy_grammar();
        let next = if legacy {
            self.next_significant(keyword + 1)
        } else {
            Some(keyword + 1).filter(|next| view.range().contains(*next))
        };
        let brace = next.and_then(|next| match token(view, next).kind() {
            TokenKind::Open {
                group,
                delimiter: Delimiter::Brace,
            } => view.group(*group).and_then(|group| group.close_token()),
            _ => None,
        });
        if let Some(close) = brace
            && !(legacy
                && arms.brace_may_begin_arm()
                && !self.enclosing_head()
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
                bindings: None,
                members: None,
            })),
            // An arm's head and guards may bind patterns.
            Mode::Cover,
            context,
            separator,
            true,
            false,
        ));
    }

    /// Whether an arm enclosing the top frame reads its head or guards.
    fn enclosing_head(&self) -> bool {
        self.stack
            .last()
            .and_then(|frame| frame.arms_owner)
            .and_then(|owner| self.stack[owner].arms())
            .is_some_and(|enclosing| enclosing.phase != ArmPhase::Result)
    }

    /// Takes the `=>`, `=>?` or `when` at `index` for the innermost arms
    /// block at this level whose arm reads its head or guards, if any: the
    /// frames open in the arm close before it. An arrow begins the arm's
    /// result; a `when` heads nested arms when it ends its line or a `{`
    /// follows it, and otherwise begins its guards. Returns whether it took
    /// the mark.
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
        self.lambda_ends_at(index);
        let view = self.scope.view;
        let when = matches!(token(view, index).kind(), TokenKind::Name("when"));
        let next = self.line_mark(index + 1);
        // A `when` ending its line, or before a `{` on its line, heads
        // nested arms, and any other begins guards; the legacy grammar read
        // nested arms wherever no mark followed it on its line.
        let nested = when
            && match view
                .token_at(index + 1)
                .filter(|_| view.range().contains(index + 1))
                .map(|token| token.kind())
            {
                _ if legacy_grammar() => next.is_none(),
                None
                | Some(
                    TokenKind::LineStart { .. }
                    | TokenKind::Open {
                        delimiter: Delimiter::Brace,
                        ..
                    },
                ) => true,
                Some(_) => false,
            };
        // The legacy grammar allowed one arrow on an arm's first line, since
        // the structural parser counted them there. Now a later arrow there
        // belongs to a nested match's hanging arms, and a second arrow of
        // this arm's own is an error when the arm is read.
        let mut later = next;
        let second_arrow = legacy_grammar()
            && loop {
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
        frame.lambda = None;
        frame.pieces.push(Piece::Token(index));
        if !nested {
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
        if !nested {
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
    fn deliver_arms(
        &mut self,
        kind: ArmsKind,
        head: usize,
        result: Parse<ArmList>,
        end: usize,
        ends_with_block: bool,
    ) {
        if kind != ArmsKind::Nested {
            self.close_match(end, Some(result), ends_with_block);
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
    fn close_match(&mut self, end: usize, arms: Option<Parse<ArmList>>, ends_with_block: bool) {
        let frame = self.stack.pop().expect("a match is open");
        let line_starts = frame.line_starts;
        let FrameKind::Match(found) = frame.kind else {
            unreachable!("only matches close here");
        };
        let (head, keyword) = (found.head, found.keyword);
        // A subject that never met its `as` but holds a `with` was likely
        // written with `with` before the arms.
        let view = self.scope.view;
        let covers = self.scope.covers;
        let with = frame.pieces.iter().any(|piece| match piece {
            Piece::Token(index) => {
                matches!(token(view, *index).kind(), TokenKind::Name("with"))
                    && in_keyword_position(view, *index)
            }
            Piece::Form { form, .. } => covers.form_shape(*form).0 == FormShape::With,
            Piece::Group { .. } => false,
        });
        let result = self.finish_match(*found, frame.pending, arms, with);
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
                ends_with_block,
            }),
        });
    }

    fn finish_match(
        &self,
        found: Match,
        pending: Option<Pending>,
        arms: Option<Parse<ArmList>>,
        subject_with: bool,
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
                _ if subject_with => Fail::error(format!(
                    "{} expression requires `as` before its arms; a `with` in its subject begins an update",
                    display(found.keyword, found.open)
                ))
                .or_at(Some(head)),
                _ => Fail::error(format!(
                    "{} expression requires `as` and at least one layout arm or an explicit `{{}}` body",
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
        // A first arm that is only a brace group was meant as braced arms,
        // which a `{` delimits only on its keyword's line.
        let braces = members.first().is_some_and(|member| {
            matches!(trim_padding(view, member), [Piece::Group { open, .. }]
                if matches!(token(view, *open).kind(),
                    TokenKind::Open { delimiter: Delimiter::Brace, .. }))
        });
        let keyword = keyword_text(view, layout.head);
        let list = self.read_arms(scope, arms, members).map_err(|fail| {
            if braces && !legacy_grammar() {
                Fail::error(format!(
                    "a `{{` delimits arms only on the line of its `{keyword}`; on a later line it begins a dictionary pattern"
                ))
                .or_at(Some(layout.head))
            } else {
                fail
            }
        })?;
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
        // An arm mark after the group on its line shows a first arm's
        // dictionary pattern, which begins on the next line instead.
        let first_arms = arms.brace_may_begin_arm();
        self.read_arms(scope, arms, members).map_err(|fail| {
            if first_arms && !legacy_grammar() && mark_follows_on_line(view, close + 1) {
                let keyword = keyword_text(view, open.saturating_sub(1));
                Fail::error(format!(
                    "a `{{` on the line of `{keyword}` delimits the arms; begin a dictionary pattern on the next line"
                ))
                .or_at(Some(open))
            } else {
                fail
            }
        })
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
        // A complete prefix conditional is a tail form, which only a block
        // may end before the end of its member; a postfix one is an
        // operator, but a lambda may end its fallback.
        let (complete, postfix, lambda, block) = match &frame.kind {
            FrameKind::Conditional(conditional) => (
                conditional.phase == Phase::Else && !conditional.postfix(),
                conditional.postfix(),
                frame.lambda,
                self.ends_with_block(&frame.pieces),
            ),
            _ => unreachable!("only conditionals close here"),
        };
        let (mut form, rest) = self.finish(frame, index);
        let head = form.head;
        let end = rest.first().map_or(index, piece_token);
        if complete {
            form.ends_with_block = block;
            if !block {
                self.ends_inline(&form_label(form.keyword), head, end);
            }
        } else if postfix && let Some(lambda) = lambda {
            self.ends_inline("lambda", lambda, end);
        }
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
            ends_with_block: false,
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

/// Whether the token at `index`, where a tail ended, could continue it: it
/// is no line start, closer or separator, nor a keyword that no expression
/// continues with (`then`, `else`, `in`, `where`, `=>`, `=>?`, or a
/// binding's or view's `->`, and a match's `as`, which no expression in its
/// subject continues with), and the view has not ended.
fn continues_tail(view: TokenView<'_, '_>, index: usize) -> bool {
    view.range().contains(index)
        && view.token_at(index).is_some_and(|token| {
            !matches!(
                token.kind(),
                TokenKind::LineStart { .. }
                    | TokenKind::Close { .. }
                    | TokenKind::Symbol("," | ";" | "=>" | "=>?" | "->")
                    | TokenKind::Name("then" | "else" | "in" | "where" | "as")
            )
        })
}

/// The text of the token at `index`.
fn keyword_text<'source>(view: TokenView<'_, 'source>, index: usize) -> &'source str {
    view.token_at(index)
        .and_then(|token| view.source().source_slice(token.span()))
        .unwrap_or("?")
}

/// Whether an `=>` or `=>?` follows `from` on its line, outside groups.
fn mark_follows_on_line(view: TokenView<'_, '_>, from: usize) -> bool {
    let mut index = from;
    while view.range().contains(index) {
        match token(view, index).kind() {
            TokenKind::LineStart { .. } | TokenKind::Close { .. } => return false,
            TokenKind::Symbol("=>" | "=>?") => return true,
            TokenKind::Open { group, .. } => {
                match view.group(*group).and_then(|group| group.close_token()) {
                    Some(close) => index = close,
                    None => return false,
                }
            }
            _ => {}
        }
        index += 1;
    }
    false
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
