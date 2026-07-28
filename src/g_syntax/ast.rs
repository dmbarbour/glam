use crate::core::{Builtin, Value};
use crate::number::Number;

use super::Diagnostic;

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedSource {
    pub declarations: Vec<Declaration>,
    pub diagnostics: Vec<Diagnostic>,
}

pub(crate) struct InspectedSource {
    pub(crate) declarations: Vec<InspectedDeclaration>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

pub(crate) struct InspectedDeclaration {
    pub(crate) line: usize,
    pub(crate) kind: InspectedDeclarationKind,
    pub(crate) preview: String,
}

pub(crate) enum InspectedDeclarationKind {
    Parsed(DeclarationKind),
    MacroDeferred,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Declaration {
    pub line: usize,
    pub kind: DeclarationKind,
    /// A source-inspection aid, not parser input retained by the syntax tree.
    pub preview: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DeclarationKind {
    Language(LanguageDecl),
    Import(ImportDecl),
    Abstract(Vec<String>),
    Unique(Vec<String>),
    Object(ObjectDecl),
    Extend(ObjectExtendDecl),
    Definition(DefinitionDecl),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageDecl {
    pub base: String,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDecl {
    pub reference: ImportReference,
    pub binary: bool,
    pub placement: ImportPlacement,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ObjectDecl {
    pub realization: ObjectRealization,
    pub target: String,
    pub alias: Option<String>,
    pub deps: Vec<SyntaxExpr>,
    pub body: Vec<ObjectBodyDefinition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectRealization {
    Instance,
    Abstract,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ObjectBodyDefinition {
    pub line: usize,
    pub kind: ObjectBodyDefinitionKind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ObjectBodyDefinitionKind {
    Definition(DefinitionDecl),
    Object(ObjectDecl),
    Extend(ObjectExtendDecl),
}

impl ObjectBodyDefinition {
    pub(super) fn definition(&self) -> Option<&DefinitionDecl> {
        match &self.kind {
            ObjectBodyDefinitionKind::Definition(definition) => Some(definition),
            ObjectBodyDefinitionKind::Object(_) | ObjectBodyDefinitionKind::Extend(_) => None,
        }
    }

    pub(super) fn object(&self) -> Option<&ObjectDecl> {
        match &self.kind {
            ObjectBodyDefinitionKind::Object(object) => Some(object),
            ObjectBodyDefinitionKind::Definition(_) | ObjectBodyDefinitionKind::Extend(_) => None,
        }
    }

    pub(super) fn extend(&self) -> Option<&ObjectExtendDecl> {
        match &self.kind {
            ObjectBodyDefinitionKind::Extend(extend) => Some(extend),
            ObjectBodyDefinitionKind::Definition(_) | ObjectBodyDefinitionKind::Object(_) => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ObjectExtendDecl {
    pub realization: ObjectRealization,
    pub target: String,
    pub alias: Option<String>,
    pub body: Vec<ObjectBodyDefinition>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ObjectExpr {
    pub realization: ObjectRealization,
    pub name: Option<Box<SyntaxExpr>>,
    pub alias: Option<String>,
    pub deps: Vec<SyntaxExpr>,
    pub body: Vec<ObjectBodyDefinition>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DoExpr {
    pub steps: Vec<DoStep>,
    pub result_line: usize,
    pub result: Box<SyntaxExpr>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DoStep {
    pub line: usize,
    pub kind: DoStepKind,
}

/// An affine source pattern owned by the built-in front end.
///
/// Pattern structure is expanded within `g_syntax`; it is never reified as a
/// core or evaluator value.
#[derive(Debug, PartialEq, Eq)]
pub struct SyntaxPattern {
    pub kind: SyntaxPatternKind,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SyntaxDictPatternEntry {
    pub path: Vec<SyntaxKeyExpr>,
    pub optional: bool,
    pub pattern: SyntaxPattern,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SyntaxGuardClause {
    Pass,
    Effect(SyntaxExpr),
    EffectBind {
        pattern: SyntaxPattern,
        operation: SyntaxExpr,
    },
    ValueBind {
        pattern: SyntaxPattern,
        value: SyntaxExpr,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct IfExpr {
    pub mode: ConditionalMode,
    pub guards: Vec<SyntaxGuardClause>,
    pub then_mode: ConditionalResultMode,
    pub then_result: Box<SyntaxExpr>,
    pub else_result: Box<SyntaxExpr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionalMode {
    Pure,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchCommitment {
    Cut,
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionalResultMode {
    Ordinary,
    Tentative,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MatchExpr {
    pub line: usize,
    pub mode: ConditionalMode,
    pub commitment: MatchCommitment,
    pub subject: Box<SyntaxExpr>,
    pub arms: Vec<MatchArm>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MatchWhenExpr {
    pub line: usize,
    pub mode: ConditionalMode,
    pub commitment: MatchCommitment,
    pub arms: Vec<WhenArm>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MatchArm {
    pub line: usize,
    pub pattern: SyntaxPattern,
    pub guards: Vec<SyntaxGuardClause>,
    pub outcome: MatchOutcome,
}

#[derive(Debug, PartialEq, Eq)]
pub struct WhenArm {
    pub line: usize,
    pub guards: Vec<SyntaxGuardClause>,
    pub outcome: MatchOutcome,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MatchOutcome {
    Result {
        line: usize,
        mode: ConditionalResultMode,
        expression: SyntaxExpr,
    },
    Nested(Vec<WhenArm>),
}

pub(super) enum SyntaxPatternScopeEvent<'a> {
    Expression(&'a SyntaxExpr),
    Capture(&'a str),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SyntaxPatternKind {
    Capture(String),
    Wildcard,
    Literal(SyntaxPatternLiteral),
    List {
        prefix: Vec<SyntaxPattern>,
        middle: Option<Box<SyntaxPattern>>,
        suffix: Vec<SyntaxPattern>,
    },
    Dict {
        entries: Vec<SyntaxDictPatternEntry>,
        remainder: Option<Box<SyntaxPattern>>,
    },
    QuotedPath(Vec<SyntaxKeyExpr>),
    Group(Box<SyntaxPattern>),
    As(Box<SyntaxPattern>, Box<SyntaxPattern>),
    View {
        view: Box<SyntaxExpr>,
        pattern: Box<SyntaxPattern>,
    },
    Predicate {
        predicate: Box<SyntaxExpr>,
        pattern: Box<SyntaxPattern>,
    },
    Guarded {
        pattern: Box<SyntaxPattern>,
        guards: Vec<SyntaxGuardClause>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum SyntaxPatternLiteral {
    Unit,
    Number(Number),
    Atom(String),
    Text(String),
}

impl SyntaxPattern {
    pub(super) fn capture(name: impl Into<String>) -> Self {
        Self {
            kind: SyntaxPatternKind::Capture(name.into()),
        }
    }

    pub(super) fn wildcard() -> Self {
        Self {
            kind: SyntaxPatternKind::Wildcard,
        }
    }

    /// Visits source captures in their semantic binding order.
    pub(super) fn visit_captures<'a>(&'a self, visitor: &mut impl FnMut(&'a str)) {
        self.visit_scope_events(&mut |event| {
            if let SyntaxPatternScopeEvent::Capture(name) = event {
                visitor(name);
            }
        });
    }

    /// Visits pattern-owned expressions and captures in matching order.
    ///
    /// Each expression is evaluated at its semantic match step. Later
    /// expressions may therefore use captures established by earlier
    /// subpatterns, while views and predicates cannot see their own inner
    /// pattern's captures.
    pub(super) fn visit_scope_events<'a>(
        &'a self,
        visitor: &mut impl FnMut(SyntaxPatternScopeEvent<'a>),
    ) {
        match &self.kind {
            SyntaxPatternKind::Capture(name) => visitor(SyntaxPatternScopeEvent::Capture(name)),
            SyntaxPatternKind::Wildcard | SyntaxPatternKind::Literal(_) => {}
            SyntaxPatternKind::List {
                prefix,
                middle,
                suffix,
            } => {
                for pattern in prefix {
                    pattern.visit_scope_events(visitor);
                }
                if let Some(pattern) = middle {
                    pattern.visit_scope_events(visitor);
                }
                for pattern in suffix {
                    pattern.visit_scope_events(visitor);
                }
            }
            SyntaxPatternKind::Dict { entries, remainder } => {
                for entry in entries {
                    for key in &entry.path {
                        visit_key_scope_events(key, visitor);
                    }
                    entry.pattern.visit_scope_events(visitor);
                }
                if let Some(pattern) = remainder {
                    pattern.visit_scope_events(visitor);
                }
            }
            SyntaxPatternKind::QuotedPath(path) => {
                for key in path {
                    visit_key_scope_events(key, visitor);
                }
            }
            SyntaxPatternKind::Group(pattern) => pattern.visit_scope_events(visitor),
            SyntaxPatternKind::As(left, right) => {
                left.visit_scope_events(visitor);
                right.visit_scope_events(visitor);
            }
            SyntaxPatternKind::View { view, pattern } => {
                visitor(SyntaxPatternScopeEvent::Expression(view));
                pattern.visit_scope_events(visitor);
            }
            SyntaxPatternKind::Predicate { predicate, pattern } => {
                visitor(SyntaxPatternScopeEvent::Expression(predicate));
                pattern.visit_scope_events(visitor);
            }
            SyntaxPatternKind::Guarded { pattern, guards } => {
                pattern.visit_scope_events(visitor);
                for guard in guards {
                    guard.visit_scope_events(visitor);
                }
            }
        }
    }

    pub(super) fn captures(&self) -> Vec<&str> {
        let mut captures = Vec::new();
        self.visit_captures(&mut |name| captures.push(name));
        captures
    }

    pub(super) fn is_irrefutable(&self) -> bool {
        match &self.kind {
            SyntaxPatternKind::Capture(_) | SyntaxPatternKind::Wildcard => true,
            SyntaxPatternKind::Group(pattern) => pattern.is_irrefutable(),
            SyntaxPatternKind::As(left, right) => left.is_irrefutable() && right.is_irrefutable(),
            SyntaxPatternKind::Literal(_)
            | SyntaxPatternKind::List { .. }
            | SyntaxPatternKind::Dict { .. }
            | SyntaxPatternKind::QuotedPath(_)
            | SyntaxPatternKind::View { .. }
            | SyntaxPatternKind::Predicate { .. }
            | SyntaxPatternKind::Guarded { .. } => false,
        }
    }

    /// Visits the primitive recursive-do events produced by this pattern.
    ///
    /// `None` is a compiler-private step; `Some(name)` is the source capture
    /// introduced by that step. This source preview mirrors resolved pattern
    /// expansion solely so pre-resolution diagnostics retain exact step
    /// indices.
    pub(super) fn visit_primitive_events<'a>(&'a self, visitor: &mut impl FnMut(Option<&'a str>)) {
        if self.is_irrefutable() {
            self.visit_irrefutable_input_events(visitor);
        } else {
            visitor(None);
            self.visit_match_events(visitor);
        }
    }

    fn visit_value_events<'a>(&'a self, visitor: &mut impl FnMut(Option<&'a str>)) {
        if self.is_irrefutable() {
            self.visit_irrefutable_input_events(visitor);
        } else {
            visitor(None);
            self.visit_match_events(visitor);
        }
    }

    fn visit_irrefutable_input_events<'a>(&'a self, visitor: &mut impl FnMut(Option<&'a str>)) {
        let captures = self.captures();
        if captures.len() != 1 {
            visitor(None);
        }
        for capture in captures {
            visitor(Some(capture));
        }
    }

    fn visit_match_events<'a>(&'a self, visitor: &mut impl FnMut(Option<&'a str>)) {
        match &self.kind {
            SyntaxPatternKind::Capture(name) => visitor(Some(name)),
            SyntaxPatternKind::Wildcard => {}
            SyntaxPatternKind::Literal(_) | SyntaxPatternKind::QuotedPath(_) => visitor(None),
            SyntaxPatternKind::Group(pattern) => pattern.visit_match_events(visitor),
            SyntaxPatternKind::As(left, right) => {
                left.visit_match_events(visitor);
                right.visit_match_events(visitor);
            }
            SyntaxPatternKind::View { pattern, .. } => {
                visitor(None);
                pattern.visit_match_events(visitor);
            }
            SyntaxPatternKind::Predicate { pattern, .. } => {
                visitor(None);
                pattern.visit_match_events(visitor);
            }
            SyntaxPatternKind::Guarded { pattern, guards } => {
                pattern.visit_match_events(visitor);
                for guard in guards {
                    guard.visit_primitive_events(visitor);
                }
            }
            SyntaxPatternKind::List {
                prefix,
                middle,
                suffix,
            } => {
                visitor(None);
                for pattern in prefix {
                    visitor(None);
                    pattern.visit_value_events(visitor);
                }
                for _ in suffix.iter().rev() {
                    visitor(None);
                    visitor(None);
                }
                if let Some(pattern) = middle {
                    pattern.visit_value_events(visitor);
                } else {
                    visitor(None);
                }
                for pattern in suffix {
                    pattern.visit_match_events(visitor);
                }
            }
            SyntaxPatternKind::Dict { entries, remainder } => {
                visitor(None);
                for entry in entries {
                    visitor(None);
                    entry.pattern.visit_value_events(visitor);
                }
                if let Some(pattern) = remainder {
                    pattern.visit_value_events(visitor);
                } else {
                    visitor(None);
                }
            }
        }
    }
}

impl SyntaxGuardClause {
    pub(super) fn visit_scope_events<'a>(
        &'a self,
        visitor: &mut impl FnMut(SyntaxPatternScopeEvent<'a>),
    ) {
        match self {
            Self::Pass => {}
            Self::Effect(expr) => visitor(SyntaxPatternScopeEvent::Expression(expr)),
            Self::EffectBind { pattern, operation } => {
                visitor(SyntaxPatternScopeEvent::Expression(operation));
                pattern.visit_scope_events(visitor);
            }
            Self::ValueBind { pattern, value } => {
                visitor(SyntaxPatternScopeEvent::Expression(value));
                pattern.visit_scope_events(visitor);
            }
        }
    }

    fn visit_primitive_events<'a>(&'a self, visitor: &mut impl FnMut(Option<&'a str>)) {
        match self {
            Self::Pass => {}
            Self::Effect(_) => visitor(None),
            Self::EffectBind { pattern, .. } | Self::ValueBind { pattern, .. } => {
                pattern.visit_primitive_events(visitor);
            }
        }
    }
}

fn visit_key_scope_events<'a>(
    key: &'a SyntaxKeyExpr,
    visitor: &mut impl FnMut(SyntaxPatternScopeEvent<'a>),
) {
    match key {
        SyntaxKeyExpr::Atom(_) => {}
        SyntaxKeyExpr::Index(expr) | SyntaxKeyExpr::PathIndex(expr) => {
            visitor(SyntaxPatternScopeEvent::Expression(expr));
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DoStepKind {
    Abstract(Vec<String>),
    Bind {
        pattern: SyntaxPattern,
        operation: SyntaxExpr,
    },
    ValueBind {
        pattern: SyntaxPattern,
        value: SyntaxExpr,
    },
    Then(SyntaxExpr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportReference {
    Local(String),
    Builtin(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportPlacement {
    Inline,
    As(String),
    At(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct DefinitionDecl {
    pub target: Vec<SyntaxKeyExpr>,
    pub parameters: Vec<String>,
    pub kind: DefinitionKind,
    pub expr: Option<SyntaxExpr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionKind {
    Introduce,
    Override,
    Update,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SyntaxExpr {
    Unit,
    /// Closed semantic data inserted by a source transformation.
    Embedded(Value),
    Number(Number),
    Text(String),
    Atom(String),
    Effect(Vec<String>),
    Name(String),
    PriorName(String),
    Escape(usize, Box<SyntaxExpr>),
    Access(Box<SyntaxExpr>, Vec<SyntaxKeyExpr>),
    Object(ObjectExpr),
    With {
        base: Box<SyntaxExpr>,
        alias: Option<String>,
        body: Vec<ObjectBodyDefinition>,
    },
    Using {
        namespace: Box<SyntaxExpr>,
        body: Box<SyntaxExpr>,
    },
    /// A dictionary containing one defined path. Braces may be omitted for
    /// path-tagged data such as `tag:value` or `[first, second]:value`.
    PathDict(Vec<SyntaxKeyExpr>, Box<SyntaxExpr>),
    /// A function that places its argument at one defined dictionary path.
    TaggedConstructor(Vec<SyntaxKeyExpr>),
    DictUnion(Vec<SyntaxExpr>),
    List(Vec<SyntaxExpr>),
    Tuple(Vec<SyntaxExpr>),
    Lambda(Vec<String>, Box<SyntaxExpr>),
    Do(DoExpr),
    If(IfExpr),
    Match(MatchExpr),
    MatchWhen(MatchWhenExpr),
    Let {
        bindings: Vec<(String, SyntaxExpr)>,
        body: Box<SyntaxExpr>,
    },
    Apply(Box<SyntaxExpr>, Box<SyntaxExpr>),
    OperatorApply {
        operator: SyntaxOperator,
        left: Box<SyntaxExpr>,
        right: Box<SyntaxExpr>,
    },
    ComparisonChain {
        first: Box<SyntaxExpr>,
        rest: Vec<(SyntaxOperator, SyntaxExpr)>,
    },
    OperatorSection {
        operator: SyntaxOperator,
        left: Option<Box<SyntaxExpr>>,
        right: Option<Box<SyntaxExpr>>,
    },
    Multiply(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Divide(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Add(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Subtract(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Append(Box<SyntaxExpr>, Box<SyntaxExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxOperator {
    Builtin(Builtin),
    BoolAnd,
    BoolOr,
    PipeForward,
    PipeBackward,
    ApplicativeForward,
    ApplicativeBackward,
    ComposeForward,
    ComposeBackward,
    EffectBind,
    KleisliCompose,
    EffectThen,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SyntaxKeyExpr {
    Atom(String),
    Index(Box<SyntaxExpr>),
    PathIndex(Box<SyntaxExpr>),
}

pub(super) fn is_comparison_operator(operator: SyntaxOperator) -> bool {
    matches!(
        operator,
        SyntaxOperator::Builtin(
            Builtin::Greater
                | Builtin::GreaterEqual
                | Builtin::Equal
                | Builtin::NotEqual
                | Builtin::LessEqual
                | Builtin::Less
        )
    )
}

#[derive(Debug)]
pub(super) enum PathSuffix {
    Single(SyntaxKeyExpr),
    Expand(Vec<SyntaxKeyExpr>),
}

pub(super) fn flatten_path_suffixes(suffixes: Vec<PathSuffix>) -> Vec<SyntaxKeyExpr> {
    let mut parts = Vec::new();
    for suffix in suffixes {
        match suffix {
            PathSuffix::Single(part) => parts.push(part),
            PathSuffix::Expand(items) => parts.extend(items),
        }
    }
    parts
}
