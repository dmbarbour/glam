//! Cover terms: a pattern position's parse before its role is known.
//!
//! In cover mode the term parser builds these instead of expressions. A term
//! converts to a `SyntaxExpr` or a `SyntaxPattern` once its role is known,
//! and a term the role cannot accept is a precise error. Leaves that read
//! alike in both roles, and forms that only expressions have, are held as
//! expressions; a pattern role converts such a leaf by its shape.
//!
//! **Positions.** A pattern position is complete (a match arm's head, or a
//! parenthesized pattern's contents) or nested (any other). Views,
//! predicates and local guards need a complete position. Completeness passes
//! through `as`, a view's pattern and a guarded pattern; data constructors
//! (tuples, lists, dicts, tags and `++`) make their parts nested, except a
//! spaced tag's payload, `path: p`, which is complete.
//!
//! Conversion recurses on the term's nesting, which only cover mode builds:
//! source nesting within one pattern view.

use crate::core::Builtin;
use crate::g_syntax::keywords::{canonical_keyword, reserved_keyword_message};
use crate::g_syntax::{
    PathSuffix, SyntaxDictPatternEntry, SyntaxExpr, SyntaxGuardClause, SyntaxKeyExpr,
    SyntaxOperator, SyntaxPattern, SyntaxPatternKind, SyntaxPatternLiteral,
};
use crate::number::Number;

use super::super::expression::{InfixChain, quoted_path, validate_expr_name};

/// A term in a position that may hold a pattern.
#[derive(Debug)]
pub(in crate::g_syntax::parser) enum Term {
    /// An expression: a leaf that a pattern reads by its shape (a number,
    /// text, unit or atom), or a form only expressions have.
    Expr(SyntaxExpr),
    /// A name: a capture in a pattern, a reference in an expression.
    Name(String),
    Wildcard,
    /// A quoted path, `'.a.[k]`, kept as keys: an expression builds a list,
    /// and a pattern compares computed components by value.
    Quoted(Vec<SyntaxKeyExpr>),
    /// A parenthesized term, whose contents are a complete position.
    Group(Box<Term>),
    /// A spaced tag's payload: a complete position wherever it stands,
    /// with no group of its own.
    Complete(Box<Term>),
    Tuple(Vec<Term>),
    List(Vec<Term>),
    /// A dict, whose members are terms: a tag is an entry, as `{a: x}` is
    /// the union of `a:x`, and any other term a dict to merge or a
    /// pattern's remainder.
    Dict(Vec<Term>),
    Tag(Vec<SyntaxKeyExpr>, Box<Term>),
    /// A dict pattern's optional entry, `path?: value`.
    OptionalTag(Vec<SyntaxKeyExpr>, Box<Term>),
    /// A pun `:name`: `name:name`, which a pattern reads as capturing
    /// `name` under its key.
    Pun(String),
    /// An application. Its function is an expression in both roles; in a
    /// pattern, the application is a predicate over its last argument.
    Apply(Box<SyntaxExpr>, Box<Term>),
    /// An unresolved infix chain; a pattern accepts only `++`.
    Chain(Box<Term>, Vec<(SyntaxOperator, Term)>),
    As(Box<Term>, Box<Term>),
    View {
        view: Box<SyntaxExpr>,
        pattern: Box<Term>,
    },
    Guarded {
        pattern: Box<Term>,
        guards: Vec<SyntaxGuardClause>,
    },
}

/// Where a pattern stands; see the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::g_syntax::parser) enum Position {
    Complete,
    Nested,
}

const EXPECTED_PATTERN: &str =
    "expected a capture, literal, list pattern, quoted-path pattern, or parenthesized pattern";

impl Term {
    pub(in crate::g_syntax::parser) fn into_expr(self) -> Result<SyntaxExpr, String> {
        Ok(match self {
            Self::Expr(expr) => expr,
            Self::Name(name) => name_expr(&name)?,
            Self::Wildcard => return Err("expected a name after `_`".to_owned()),
            Self::Quoted(path) => quoted_path(vec![PathSuffix::Expand(path)]),
            Self::Group(term) | Self::Complete(term) => term.into_expr()?,
            Self::Tuple(items) => SyntaxExpr::Tuple(exprs(items)?),
            Self::List(items) => SyntaxExpr::List(exprs(items)?),
            Self::Dict(members) => SyntaxExpr::DictUnion(exprs(members)?),
            Self::Tag(path, payload) => SyntaxExpr::PathDict(path, Box::new(payload.into_expr()?)),
            Self::OptionalTag(..) => {
                return Err(
                    "an optional entry `path?:` is valid only in a dictionary pattern".to_owned(),
                );
            }
            Self::Pun(name) => {
                let name = validate_expr_name(&name)?;
                SyntaxExpr::PathDict(
                    vec![SyntaxKeyExpr::Atom(name.clone())],
                    Box::new(SyntaxExpr::Name(name)),
                )
            }
            Self::Apply(function, argument) => {
                SyntaxExpr::Apply(function, Box::new(argument.into_expr()?))
            }
            Self::Chain(first, rest) => InfixChain::from_parts(
                first.into_expr()?,
                rest.into_iter()
                    .map(|(operator, operand)| {
                        operand.into_expr().map(|operand| (operator, operand))
                    })
                    .collect::<Result<_, _>>()?,
            )
            .resolve()?,
            Self::As(..) => {
                return Err("`as` joins two patterns and is not valid in an expression".to_owned());
            }
            Self::View { .. } => {
                return Err("a view arrow is valid only in a pattern".to_owned());
            }
            Self::Guarded { .. } => {
                return Err("a `when` guard is valid only in a pattern".to_owned());
            }
        })
    }

    pub(in crate::g_syntax::parser) fn into_pattern(
        self,
        position: Position,
    ) -> Result<SyntaxPattern, String> {
        let complete = position == Position::Complete;
        Ok(match self {
            Self::Expr(expr) => expr_pattern(expr)?,
            Self::Name(name) => name_pattern(name)?,
            Self::Wildcard => SyntaxPattern::wildcard(),
            Self::Quoted(path) => quoted_path_pattern(path),
            Self::Group(term) => {
                // Views, predicates and guards fill their group, as they
                // always have; other contents keep the group.
                let fills = matches!(
                    *term,
                    Self::Apply(..) | Self::View { .. } | Self::Guarded { .. }
                );
                let pattern = term.into_pattern(Position::Complete)?;
                if fills {
                    pattern
                } else {
                    kind(SyntaxPatternKind::Group(Box::new(pattern)))
                }
            }
            Self::Complete(term) => term.into_pattern(Position::Complete)?,
            Self::Tuple(items) => tuple_pattern(patterns(items)?),
            Self::List(items) => fixed_list_pattern(patterns(items)?),
            Self::Dict(members) => dict_pattern_from(members)?,
            // A tag, `path:p`, `path: p`, `(path:) p` or `:name`, is a dict
            // pattern of one entry.
            Self::Tag(..) | Self::Pun(_) | Self::Apply(..) if tag_entry(&self) => {
                let (path, optional, pattern) = self.into_entry()?;
                dict_pattern(vec![dict_entry(path, optional, pattern)], None)
            }
            Self::OptionalTag(..) => {
                return Err(
                    "an optional entry `path?:` is valid only in a dictionary pattern".to_owned(),
                );
            }
            Self::Tag(..) | Self::Pun(_) => unreachable!("a tag is an entry"),
            Self::Apply(predicate, pattern) if complete => kind(SyntaxPatternKind::Predicate {
                predicate,
                pattern: Box::new(pattern.into_pattern(Position::Nested)?),
            }),
            Self::Apply(..) => {
                return Err(
                    "a predicate pattern must be parenthesized here; write `(Predicate Pattern)`"
                        .to_owned(),
                );
            }
            Self::Chain(first, rest) => append_pattern(*first, rest)?,
            Self::As(left, right) => kind(SyntaxPatternKind::As(
                Box::new(left.into_pattern(position)?),
                Box::new(right.into_pattern(position)?),
            )),
            Self::View { view, pattern } if complete => kind(SyntaxPatternKind::View {
                view,
                pattern: Box::new(pattern.into_pattern(position)?),
            }),
            Self::View { .. } => {
                return Err(
                    "a view pattern must be parenthesized here; write `(View -> Pattern)`"
                        .to_owned(),
                );
            }
            Self::Guarded { pattern, guards } if complete => kind(SyntaxPatternKind::Guarded {
                pattern: Box::new(pattern.into_pattern(position)?),
                guards,
            }),
            Self::Guarded { .. } => {
                return Err(
                    "a local pattern guard must be parenthesized here; write `(Pattern when Guard)`"
                        .to_owned(),
                );
            }
        })
    }
}

impl Term {
    /// A tag as a dict pattern entry: its path, whether it is optional, and
    /// its payload's pattern. `tag_entry` has accepted the term.
    fn into_entry(self) -> Result<(Vec<SyntaxKeyExpr>, bool, SyntaxPattern), String> {
        Ok(match self {
            Self::Tag(path, payload) => (path, false, payload.into_pattern(Position::Nested)?),
            Self::OptionalTag(path, payload) => {
                (path, true, payload.into_pattern(Position::Nested)?)
            }
            Self::Pun(name) => {
                let pattern = name_pattern(name)?;
                let SyntaxPatternKind::Capture(name) = &pattern.kind else {
                    unreachable!("a pun's name converts to a capture");
                };
                let path = vec![SyntaxKeyExpr::Atom(
                    name.strip_prefix('_').unwrap_or(name).to_owned(),
                )];
                (path, false, pattern)
            }
            Self::Apply(constructor, payload) => {
                let SyntaxExpr::TaggedConstructor(path) = *constructor else {
                    unreachable!("only a constructor's application is an entry");
                };
                (path, false, payload.into_pattern(Position::Nested)?)
            }
            Self::Group(term) => term.into_entry()?,
            _ => unreachable!("only a tag is an entry"),
        })
    }
}

/// Whether a term is a tag, read as one dict pattern entry: `path:p`,
/// `path?:p`, `:name`, a constructor applied, `(path:) p`, or one of these
/// parenthesized.
fn tag_entry(term: &Term) -> bool {
    match term {
        Term::Tag(..) | Term::OptionalTag(..) | Term::Pun(_) => true,
        Term::Apply(function, _) => matches!(function.as_ref(), SyntaxExpr::TaggedConstructor(_)),
        Term::Group(term) => tag_entry(term),
        _ => false,
    }
}

fn exprs(terms: Vec<Term>) -> Result<Vec<SyntaxExpr>, String> {
    terms.into_iter().map(Term::into_expr).collect()
}

fn patterns(terms: Vec<Term>) -> Result<Vec<SyntaxPattern>, String> {
    terms
        .into_iter()
        .map(|term| term.into_pattern(Position::Nested))
        .collect()
}

/// A name as an expression: a reference, or `_name` for a prior name.
fn name_expr(name: &str) -> Result<SyntaxExpr, String> {
    let Some(prior) = name.strip_prefix('_') else {
        return validate_expr_name(name).map(SyntaxExpr::Name);
    };
    if !is_glam_name(prior) {
        return Err("expected a name after `_`".to_owned());
    }
    if prior == "module_origin" {
        return Err("`module_origin` is reserved".to_owned());
    }
    validate_expr_name(prior).map(SyntaxExpr::PriorName)
}

/// A name as a pattern: a capture.
fn name_pattern(name: String) -> Result<SyntaxPattern, String> {
    if let Some(keyword) = canonical_keyword(&name) {
        return Err(reserved_keyword_message(keyword));
    }
    if !is_local_name(&name) {
        return Err(EXPECTED_PATTERN.to_owned());
    }
    Ok(SyntaxPattern::capture(name))
}

/// An expression leaf as a pattern, by its shape.
fn expr_pattern(expr: SyntaxExpr) -> Result<SyntaxPattern, String> {
    let literal = match expr {
        SyntaxExpr::Unit => SyntaxPatternLiteral::Unit,
        SyntaxExpr::Number(number) => SyntaxPatternLiteral::Number(number),
        SyntaxExpr::Text(text) => SyntaxPatternLiteral::Text(text),
        SyntaxExpr::Atom(name) => SyntaxPatternLiteral::Atom(name),
        SyntaxExpr::TaggedConstructor(_) => {
            return Err(
                "a tag's constructor `path:` is not a pattern; write `path: Pattern`".to_owned(),
            );
        }
        _ => return Err(EXPECTED_PATTERN.to_owned()),
    };
    Ok(kind(SyntaxPatternKind::Literal(literal)))
}

fn dict_pattern_from(members: Vec<Term>) -> Result<SyntaxPattern, String> {
    let count = members.len();
    let mut entries: Vec<SyntaxDictPatternEntry> = Vec::new();
    let mut remainder = None;
    for (index, member) in members.into_iter().enumerate() {
        let (path, optional, pattern) = match member {
            member if tag_entry(&member) => member.into_entry()?,
            term => {
                if remainder.is_some() {
                    return Err("dictionary pattern permits only one remainder".to_owned());
                }
                if index + 1 < count {
                    return Err("dictionary remainder must be the final pattern item".to_owned());
                }
                remainder = Some(Box::new(term.into_pattern(Position::Nested)?));
                continue;
            }
        };
        if entries.iter().any(|entry| entry.path == path) {
            return Err("dictionary pattern repeats the same path expression".to_owned());
        }
        entries.push(dict_entry(path, optional, pattern));
    }
    Ok(dict_pattern(entries, remainder))
}

/// An append chain as a list pattern: fixed lists and texts around at most
/// one variable-length segment.
fn append_pattern(first: Term, rest: Vec<(SyntaxOperator, Term)>) -> Result<SyntaxPattern, String> {
    if rest
        .iter()
        .any(|(operator, _)| *operator != SyntaxOperator::Builtin(Builtin::Append))
    {
        return Err("only `++` may join patterns".to_owned());
    }
    let mut prefix = Vec::new();
    let mut middle = None;
    let mut suffix = Vec::new();
    for part in std::iter::once(first).chain(rest.into_iter().map(|(_, operand)| operand)) {
        let target = if middle.is_some() {
            &mut suffix
        } else {
            &mut prefix
        };
        match part.into_pattern(Position::Nested)? {
            SyntaxPattern {
                kind:
                    SyntaxPatternKind::List {
                        prefix: fixed,
                        middle: None,
                        suffix: fixed_suffix,
                    },
            } => {
                target.extend(fixed);
                target.extend(fixed_suffix);
            }
            SyntaxPattern {
                kind: SyntaxPatternKind::Literal(SyntaxPatternLiteral::Text(text)),
            } => target.extend(text.bytes().map(number_pattern_from_byte)),
            pattern if middle.is_none() => middle = Some(Box::new(pattern)),
            _ => {
                return Err("list patterns permit only one variable-length segment".to_owned());
            }
        }
    }
    Ok(kind(SyntaxPatternKind::List {
        prefix,
        middle,
        suffix,
    }))
}

fn quoted_path_pattern(path: Vec<SyntaxKeyExpr>) -> SyntaxPattern {
    if path.iter().all(fixed_quoted_path_component) {
        return fixed_list_pattern(
            path.into_iter()
                .map(fixed_quoted_path_component_pattern)
                .collect(),
        );
    }
    kind(SyntaxPatternKind::QuotedPath(path))
}

fn fixed_quoted_path_component(component: &SyntaxKeyExpr) -> bool {
    match component {
        SyntaxKeyExpr::Atom(_) => true,
        SyntaxKeyExpr::Index(expr) => matches!(
            expr.as_ref(),
            SyntaxExpr::Unit | SyntaxExpr::Number(_) | SyntaxExpr::Text(_) | SyntaxExpr::Atom(_)
        ),
        SyntaxKeyExpr::PathIndex(_) => false,
    }
}

fn fixed_quoted_path_component_pattern(component: SyntaxKeyExpr) -> SyntaxPattern {
    let literal = match component {
        SyntaxKeyExpr::Atom(name) => SyntaxPatternLiteral::Atom(name),
        SyntaxKeyExpr::Index(expr) => match *expr {
            SyntaxExpr::Unit => SyntaxPatternLiteral::Unit,
            SyntaxExpr::Number(number) => SyntaxPatternLiteral::Number(number),
            SyntaxExpr::Text(text) => SyntaxPatternLiteral::Text(text),
            SyntaxExpr::Atom(name) => SyntaxPatternLiteral::Atom(name),
            _ => unreachable!("fixed quoted-path component was classified as a literal"),
        },
        SyntaxKeyExpr::PathIndex(_) => {
            unreachable!("fixed quoted-path component cannot be a path splice")
        }
    };
    kind(SyntaxPatternKind::Literal(literal))
}

fn kind(kind: SyntaxPatternKind) -> SyntaxPattern {
    SyntaxPattern { kind }
}

fn tuple_pattern(items: Vec<SyntaxPattern>) -> SyntaxPattern {
    dict_pattern(
        vec![dict_entry(
            vec![SyntaxKeyExpr::Atom("tuple".to_owned())],
            false,
            fixed_list_pattern(items),
        )],
        None,
    )
}

fn fixed_list_pattern(items: Vec<SyntaxPattern>) -> SyntaxPattern {
    kind(SyntaxPatternKind::List {
        prefix: items,
        middle: None,
        suffix: Vec::new(),
    })
}

fn dict_pattern(
    entries: Vec<SyntaxDictPatternEntry>,
    remainder: Option<Box<SyntaxPattern>>,
) -> SyntaxPattern {
    kind(SyntaxPatternKind::Dict { entries, remainder })
}

fn dict_entry(
    path: Vec<SyntaxKeyExpr>,
    optional: bool,
    pattern: SyntaxPattern,
) -> SyntaxDictPatternEntry {
    SyntaxDictPatternEntry {
        path,
        optional,
        pattern,
    }
}

fn number_pattern_from_byte(byte: u8) -> SyntaxPattern {
    kind(SyntaxPatternKind::Literal(SyntaxPatternLiteral::Number(
        Number::from_u8(byte),
    )))
}

fn is_glam_name(name: &str) -> bool {
    name.starts_with(|character: char| character.is_ascii_alphabetic())
}

fn is_local_name(name: &str) -> bool {
    is_glam_name(name) || name.strip_prefix('_').is_some_and(is_glam_name)
}
