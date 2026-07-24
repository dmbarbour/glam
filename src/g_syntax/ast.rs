use crate::core::Builtin;
use crate::number::Number;

use super::Diagnostic;

#[derive(Debug, PartialEq, Eq)]
pub struct ParsedSource {
    pub declarations: Vec<Declaration>,
    pub diagnostics: Vec<Diagnostic>,
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

#[derive(Debug, PartialEq, Eq)]
pub enum DoStepKind {
    Abstract(Vec<String>),
    Bind { name: String, operation: SyntaxExpr },
    ValueBind { name: String, value: SyntaxExpr },
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
