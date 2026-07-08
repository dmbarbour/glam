use chumsky::prelude::*;

use std::sync::Arc;

use crate::core::{Atom, Dict, Expr as CoreExpr, Key, KeyExpr as CoreKeyExpr, Term, Value};
use crate::diagnostic::Severity;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
}

impl SourceFile {
    pub fn new(path: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            text: text.into(),
        }
    }

    pub fn parse(&self) -> ParsedSource {
        parse_source(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSource {
    pub declarations: Vec<Declaration>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub line: usize,
    pub kind: DeclarationKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationKind {
    Language(LanguageDecl),
    Import(ImportDecl),
    Abstract(Vec<String>),
    Unique(Vec<String>),
    Object,
    Extend,
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
    pub reference: String,
    pub placement: ImportPlacement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportPlacement {
    Inline,
    As(String),
    At(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionDecl {
    pub target: String,
    pub kind: DefinitionKind,
    pub body: String,
    pub expr: Option<SyntaxExpr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionKind {
    Introduce,
    Override,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxExpr {
    Number(i64),
    Text(String),
    Name(Vec<SyntaxKeyExpr>),
    SingletonDict(SyntaxKeyExpr, Box<SyntaxExpr>),
    DictUnion(Vec<SyntaxExpr>),
    List(Vec<SyntaxExpr>),
    Append(Box<SyntaxExpr>, Box<SyntaxExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxKeyExpr {
    Atom(String),
    Expr(Box<SyntaxExpr>),
    ListExpr(Box<SyntaxExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredSource {
    pub term: Option<Term>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub line: usize,
    pub message: String,
}

impl Diagnostic {
    fn warn(line: usize, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            line,
            message: message.into(),
        }
    }

    fn error(line: usize, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            line,
            message: message.into(),
        }
    }
}

pub fn parse_source(source: &SourceFile) -> ParsedSource {
    let mut diagnostics = line_ending_diagnostics(&source.text);
    let physical_lines = split_lines(&source.text);
    let mut declarations = Vec::new();
    let mut index = 0;

    while index < physical_lines.len() {
        let line = physical_lines[index];
        let trimmed = strip_comment(line.text).trim();

        if trimmed.is_empty() {
            index += 1;
            continue;
        }

        if is_indented(line.text) {
            diagnostics.push(Diagnostic::error(
                line.number,
                "continuation line without a preceding declaration",
            ));
            index += 1;
            continue;
        }

        let start_line = line.number;
        let mut text = String::from(trimmed);
        index += 1;

        while index < physical_lines.len() {
            let next = physical_lines[index];
            let next_trimmed = strip_comment(next.text).trim();

            if next_trimmed.is_empty() {
                index += 1;
                continue;
            }

            if !is_indented(next.text) && !is_dedent_closer(next_trimmed) {
                break;
            }

            text.push('\n');
            text.push_str(next_trimmed);
            index += 1;
        }

        declarations.push(Declaration {
            line: start_line,
            kind: classify_declaration(&text, start_line, &mut diagnostics),
            text,
        });
    }

    validate_language_position(&declarations, &mut diagnostics);

    ParsedSource {
        declarations,
        diagnostics,
    }
}

pub fn lower_to_core(parsed: &ParsedSource) -> LoweredSource {
    let mut root = Dict::new_sync();
    let mut atoms = std::collections::BTreeMap::new();
    let mut diagnostics = parsed.diagnostics.clone();

    for declaration in &parsed.declarations {
        let DeclarationKind::Definition(definition) = &declaration.kind else {
            continue;
        };

        match lower_definition(definition, declaration.line, &mut root, &mut atoms) {
            Ok(()) => {}
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
    }

    LoweredSource {
        term: Some(Term::Expr(CoreExpr::Value(Value::Dict(root)))),
        diagnostics,
    }
}

fn lower_definition(
    definition: &DefinitionDecl,
    line: usize,
    root: &mut Dict,
    atoms: &mut std::collections::BTreeMap<String, Atom>,
) -> Result<(), Diagnostic> {
    let Some(expr) = &definition.expr else {
        if definition.target == "asm.result" {
            return Err(Diagnostic::error(
                line,
                "`asm.result` uses an expression unsupported by the .g front end",
            ));
        }

        return Ok(());
    };

    let value = syntax_expr_to_value(expr, line, atoms)?;

    *root = match definition.kind {
        DefinitionKind::Introduce => insert_path(root, &definition.target, value, line, atoms)?,
        DefinitionKind::Override => override_path(root, &definition.target, value, line, atoms)?,
        DefinitionKind::Update => Err(Diagnostic::error(
            line,
            "update definitions are not supported by the .g spike lowering",
        ))?,
    };

    Ok(())
}

fn syntax_expr_to_value(
    expr: &SyntaxExpr,
    line: usize,
    atoms: &mut std::collections::BTreeMap<String, Atom>,
) -> Result<Value, Diagnostic> {
    match expr {
        SyntaxExpr::Number(number) => Ok(Value::Number(*number)),
        SyntaxExpr::Text(text) => Ok(Value::binary_from_text(text)),
        SyntaxExpr::Name(_)
        | SyntaxExpr::SingletonDict(_, _)
        | SyntaxExpr::DictUnion(_)
        | SyntaxExpr::List(_)
        | SyntaxExpr::Append(_, _) => Ok(Value::Expr(Arc::new(syntax_expr_to_core_expr(
            expr, line, atoms,
        )?))),
    }
}

fn syntax_expr_to_core_expr(
    expr: &SyntaxExpr,
    line: usize,
    atoms: &mut std::collections::BTreeMap<String, Atom>,
) -> Result<CoreExpr, Diagnostic> {
    Ok(match expr {
        SyntaxExpr::Number(number) => CoreExpr::Value(Value::Number(*number)),
        SyntaxExpr::Text(text) => CoreExpr::Value(Value::binary_from_text(text)),
        SyntaxExpr::SingletonDict(key, value) => builtin_apply2(
            crate::core::Builtin::Singleton,
            syntax_key_expr_to_core_expr(key, line, atoms)?,
            syntax_expr_to_core_expr(value, line, atoms)?,
        ),
        SyntaxExpr::DictUnion(items) => lower_dict_union(items, line, atoms)?,
        SyntaxExpr::Name(parts) => CoreExpr::Name(Arc::from(
            parts
                .iter()
                .map(|part| syntax_key_expr_to_core(part, line, atoms))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        SyntaxExpr::List(items) => CoreExpr::List(Arc::from(
            items
                .iter()
                .map(|expr| syntax_expr_to_core_expr(expr, line, atoms).map(Arc::new))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        SyntaxExpr::Append(left, right) => CoreExpr::Apply(
            Arc::new(CoreExpr::Apply(
                Arc::new(CoreExpr::Value(Value::Builtin(
                    crate::core::Builtin::Append,
                ))),
                Arc::new(syntax_expr_to_core_expr(left, line, atoms)?),
            )),
            Arc::new(syntax_expr_to_core_expr(right, line, atoms)?),
        ),
    })
}

fn syntax_key_expr_to_core_expr(
    key: &SyntaxKeyExpr,
    line: usize,
    atoms: &mut std::collections::BTreeMap<String, Atom>,
) -> Result<CoreExpr, Diagnostic> {
    Ok(match key {
        SyntaxKeyExpr::Atom(name) => CoreExpr::Value(Value::Atom(atom_value(name, atoms))),
        SyntaxKeyExpr::Expr(expr) => syntax_expr_to_core_expr(expr, line, atoms)?,
        SyntaxKeyExpr::ListExpr(_) => {
            return Err(Diagnostic::error(
                line,
                "list-valued path expressions are not valid dictionary keys",
            ));
        }
    })
}

fn lower_dict_union(
    items: &[SyntaxExpr],
    line: usize,
    atoms: &mut std::collections::BTreeMap<String, Atom>,
) -> Result<CoreExpr, Diagnostic> {
    let mut items = items.iter();
    let Some(first) = items.next() else {
        return Ok(CoreExpr::Value(Value::Dict(Dict::new_sync())));
    };

    let mut expr = syntax_expr_to_core_expr(first, line, atoms)?;
    for item in items {
        expr = builtin_apply2(
            crate::core::Builtin::DictUnion,
            expr,
            syntax_expr_to_core_expr(item, line, atoms)?,
        );
    }
    Ok(expr)
}

fn builtin_apply2(builtin: crate::core::Builtin, left: CoreExpr, right: CoreExpr) -> CoreExpr {
    CoreExpr::Apply(
        Arc::new(CoreExpr::Apply(
            Arc::new(CoreExpr::Value(Value::Builtin(builtin))),
            Arc::new(left),
        )),
        Arc::new(right),
    )
}

fn syntax_key_expr_to_core(
    key: &SyntaxKeyExpr,
    line: usize,
    atoms: &mut std::collections::BTreeMap<String, Atom>,
) -> Result<CoreKeyExpr, Diagnostic> {
    Ok(match key {
        SyntaxKeyExpr::Atom(name) => CoreKeyExpr::Key(atom_key(name, atoms)),
        SyntaxKeyExpr::Expr(expr) => {
            CoreKeyExpr::Expr(Arc::new(syntax_expr_to_core_expr(expr, line, atoms)?))
        }
        SyntaxKeyExpr::ListExpr(expr) => {
            CoreKeyExpr::ListExpr(Arc::new(syntax_expr_to_core_expr(expr, line, atoms)?))
        }
    })
}

fn insert_path(
    root: &Dict,
    target: &str,
    value: Value,
    line: usize,
    atoms: &mut std::collections::BTreeMap<String, Atom>,
) -> Result<Dict, Diagnostic> {
    let parts = target.split('.').collect::<Vec<_>>();
    let Some((leaf, parents)) = parts.split_last() else {
        return Err(Diagnostic::error(line, "definition target cannot be empty"));
    };

    let leaf_key = atom_key(leaf, atoms);
    let existing = get_path(root, parents, &leaf_key, atoms);
    if existing.is_some() {
        return Err(Diagnostic::error(
            line,
            format!("cannot introduce `{target}` because it is already defined"),
        ));
    }

    set_path(root, parents, leaf_key, value, line, atoms)
}

fn override_path(
    root: &Dict,
    target: &str,
    value: Value,
    line: usize,
    atoms: &mut std::collections::BTreeMap<String, Atom>,
) -> Result<Dict, Diagnostic> {
    let parts = target.split('.').collect::<Vec<_>>();
    let Some((leaf, parents)) = parts.split_last() else {
        return Err(Diagnostic::error(line, "definition target cannot be empty"));
    };

    let leaf_key = atom_key(leaf, atoms);
    let existing = get_path(root, parents, &leaf_key, atoms);
    if existing.is_none() {
        return Err(Diagnostic::error(
            line,
            format!("cannot override `{target}` because it is not defined"),
        ));
    }

    set_path(root, parents, leaf_key, value, line, atoms)
}

fn get_path<'a>(
    root: &'a Dict,
    parents: &[&str],
    leaf: &Key,
    atoms: &mut std::collections::BTreeMap<String, Atom>,
) -> Option<&'a Value> {
    let mut current = root;

    for parent in parents {
        let Value::Dict(next) = current.get(&atom_key(parent, atoms))? else {
            return None;
        };
        current = next;
    }

    current.get(leaf)
}

fn set_path(
    root: &Dict,
    parents: &[&str],
    leaf: Key,
    value: Value,
    line: usize,
    atoms: &mut std::collections::BTreeMap<String, Atom>,
) -> Result<Dict, Diagnostic> {
    let Some((parent, rest)) = parents.split_first() else {
        return Ok(root.insert(leaf, value));
    };

    let parent_key = atom_key(parent, atoms);
    let child = match root.get(&parent_key) {
        Some(Value::Dict(child)) => child.clone(),
        Some(_) => {
            return Err(Diagnostic::error(
                line,
                format!("cannot define below `{parent}` because it is not a dictionary"),
            ));
        }
        None => Dict::new_sync(),
    };
    let updated_child = set_path(&child, rest, leaf, value, line, atoms)?;
    Ok(root.insert(parent_key, Value::Dict(updated_child)))
}

fn atom_key(name: &str, atoms: &mut std::collections::BTreeMap<String, Atom>) -> Key {
    Key::Atom(atom_value(name, atoms))
}

fn atom_value(name: &str, atoms: &mut std::collections::BTreeMap<String, Atom>) -> Atom {
    atoms
        .entry(name.to_owned())
        .or_insert_with(|| Atom::from_key(&Key::binary_from_text(name)))
        .clone()
}

fn validate_language_position(declarations: &[Declaration], diagnostics: &mut Vec<Diagnostic>) {
    let Some(first) = declarations.first() else {
        diagnostics.push(Diagnostic::error(
            1,
            "empty source has no language declaration",
        ));
        return;
    };

    if !matches!(first.kind, DeclarationKind::Language(_)) {
        diagnostics.push(Diagnostic::error(
            first.line,
            "first declaration should be a language version declaration",
        ));
    }

    for declaration in declarations.iter().skip(1) {
        if matches!(declaration.kind, DeclarationKind::Language(_)) {
            diagnostics.push(Diagnostic::error(
                declaration.line,
                "language declaration must appear before all other declarations",
            ));
        }
    }
}

fn classify_declaration(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> DeclarationKind {
    match first_word(text) {
        Some("object") => return DeclarationKind::Object,
        Some("extend") | Some("extends") => return DeclarationKind::Extend,
        _ => {}
    }

    let (declaration, errors) = declaration_parser().parse(text).into_output_errors();

    for error in errors {
        diagnostics.push(Diagnostic::error(line, error.to_string()));
    }

    if let Some(declaration) = declaration {
        declaration
    } else {
        DeclarationKind::Unknown
    }
}

fn declaration_parser<'src>()
-> impl Parser<'src, &'src str, DeclarationKind, extra::Err<Rich<'src, char>>> {
    choice((
        language_decl().map(DeclarationKind::Language),
        import_decl().map(DeclarationKind::Import),
        keyword_name_list("abstract").map(DeclarationKind::Abstract),
        keyword_name_list("unique").map(DeclarationKind::Unique),
        definition_decl().map(DeclarationKind::Definition),
    ))
    .then_ignore(end())
}

fn language_decl<'src>() -> impl Parser<'src, &'src str, LanguageDecl, extra::Err<Rich<'src, char>>>
{
    just("language")
        .or(just("lang"))
        .padded()
        .ignore_then(name())
        .then(
            just("with")
                .padded()
                .ignore_then(
                    name()
                        .separated_by(just(',').padded())
                        .at_least(1)
                        .collect::<Vec<_>>(),
                )
                .or_not(),
        )
        .map(|(base, extensions)| LanguageDecl {
            base,
            extensions: extensions.unwrap_or_default(),
        })
}

fn import_decl<'src>() -> impl Parser<'src, &'src str, ImportDecl, extra::Err<Rich<'src, char>>> {
    let placement = just("as")
        .padded()
        .ignore_then(path())
        .map(ImportPlacement::As)
        .or(just("at")
            .padded()
            .ignore_then(path())
            .map(ImportPlacement::At))
        .or_not()
        .map(|placement| placement.unwrap_or(ImportPlacement::Inline));

    just("import")
        .padded()
        .ignore_then(quoted_text())
        .then(placement)
        .map(|(reference, placement)| ImportDecl {
            reference,
            placement,
        })
}

fn keyword_name_list<'src>(
    keyword: &'static str,
) -> impl Parser<'src, &'src str, Vec<String>, extra::Err<Rich<'src, char>>> {
    just(keyword).padded().ignore_then(
        path()
            .separated_by(just(',').padded())
            .at_least(1)
            .collect::<Vec<_>>(),
    )
}

fn definition_decl<'src>()
-> impl Parser<'src, &'src str, DefinitionDecl, extra::Err<Rich<'src, char>>> {
    path()
        .then_ignore(whitespace1())
        .then(definition_operator())
        .then_ignore(whitespace0())
        .then(rest_of_declaration())
        .try_map(|((target, kind), body), span| {
            if body.is_empty() {
                Err(Rich::custom(span, "definition body cannot be empty"))
            } else {
                let expr = parse_expr(body.as_str());
                Ok(DefinitionDecl {
                    target,
                    kind,
                    body,
                    expr,
                })
            }
        })
}

fn definition_operator<'src>()
-> impl Parser<'src, &'src str, DefinitionKind, extra::Err<Rich<'src, char>>> {
    choice((
        just("::=").to(DefinitionKind::Update),
        just(":=").to(DefinitionKind::Override),
        just('=').to(DefinitionKind::Introduce),
    ))
}

fn path<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    name()
        .separated_by(just('.'))
        .at_least(1)
        .collect::<Vec<_>>()
        .map(|parts| parts.join("."))
}

fn name<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    text::ascii::ident().map(ToOwned::to_owned)
}

fn quoted_text<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    none_of('"')
        .repeated()
        .to_slice()
        .map(ToOwned::to_owned)
        .delimited_by(just('"'), just('"'))
}

fn rest_of_declaration<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>>
{
    any()
        .repeated()
        .to_slice()
        .map(|text: &str| text.trim().to_owned())
}

fn parse_expr(text: &str) -> Option<SyntaxExpr> {
    syntax_expr_parser()
        .then_ignore(end())
        .parse(text)
        .into_result()
        .ok()
}

fn syntax_expr_parser<'src>()
-> impl Parser<'src, &'src str, SyntaxExpr, extra::Err<Rich<'src, char>>> {
    #[derive(Debug)]
    enum PathSuffix {
        Single(SyntaxKeyExpr),
        Expand(Vec<SyntaxKeyExpr>),
    }

    recursive(|expr| {
        let single_key_expr = || {
            choice((
                just('\'').ignore_then(glam_name()).map(SyntaxKeyExpr::Atom),
                expr.clone().map(|expr| SyntaxKeyExpr::Expr(Box::new(expr))),
            ))
        };

        let path_list_shorthand = single_key_expr()
            .padded()
            .separated_by(just(',').padded())
            .allow_leading()
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just('['), just(']'))
            .map(PathSuffix::Expand);
        let path_list_expr = expr
            .clone()
            .padded()
            .delimited_by(just('('), just(')'))
            .map(|expr| PathSuffix::Single(SyntaxKeyExpr::ListExpr(Box::new(expr))));

        // Dotted paths stay lexically tight because `.` has other roles in the
        // language surface, such as future effect sugar like `.bar`.
        let name_expr = glam_name()
            .map(SyntaxKeyExpr::Atom)
            .then(
                just('.')
                    .ignore_then(choice((
                        path_list_shorthand,
                        path_list_expr,
                        glam_name().map(SyntaxKeyExpr::Atom).map(PathSuffix::Single),
                    )))
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .map(|(first, suffixes)| {
                let mut parts = vec![first];
                for suffix in suffixes {
                    match suffix {
                        PathSuffix::Single(part) => parts.push(part),
                        PathSuffix::Expand(items) => parts.extend(items),
                    }
                }
                SyntaxExpr::Name(parts)
            });

        let number = text::digits(10).to_slice().try_map(|digits: &str, span| {
            digits
                .parse::<i64>()
                .map(SyntaxExpr::Number)
                .map_err(|err| {
                    Rich::custom(span, format!("invalid integer literal `{digits}`: {err}"))
                })
        });
        let text = quoted_text().map(SyntaxExpr::Text);

        let list = expr
            .clone()
            .padded()
            .separated_by(just(',').padded())
            .allow_leading()
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just('['), just(']'))
            .map(SyntaxExpr::List);

        let dict_item_key = choice((
            glam_name().map(SyntaxKeyExpr::Atom),
            single_key_expr()
                .padded()
                .delimited_by(just('['), just(']')),
        ));
        let dict_item = choice((
            dict_item_key
                .then_ignore(just(':').padded())
                .then(expr.clone())
                .map(|(key, value)| SyntaxExpr::SingletonDict(key, Box::new(value))),
            expr.clone(),
        ));
        let dict = dict_item
            .padded()
            .separated_by(just(',').padded())
            .allow_leading()
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just('{'), just('}'))
            .map(SyntaxExpr::DictUnion);

        let atom = choice((text, list, dict, number, name_expr)).boxed();

        atom.clone()
            .then(
                just("++")
                    .padded()
                    .ignore_then(atom)
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .map(|(first, rest)| {
                rest.into_iter().fold(first, |left, right| {
                    SyntaxExpr::Append(Box::new(left), Box::new(right))
                })
            })
    })
}

fn glam_name<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    text::ascii::ident().try_map(|name: &str, span| {
        if name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
        {
            Ok(name.to_owned())
        } else {
            Err(Rich::custom(span, "expected name"))
        }
    })
}

fn whitespace0<'src>() -> impl Parser<'src, &'src str, (), extra::Err<Rich<'src, char>>> {
    one_of(" \t\r\n").repeated().ignored()
}

fn whitespace1<'src>() -> impl Parser<'src, &'src str, (), extra::Err<Rich<'src, char>>> {
    one_of(" \t\r\n").repeated().at_least(1).ignored()
}

fn first_word(text: &str) -> Option<&str> {
    text.split(|ch: char| ch.is_whitespace()).next()
}

fn strip_comment(line: &str) -> &str {
    line.split_once('#').map_or(line, |(before, _)| before)
}

fn is_indented(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

fn is_dedent_closer(trimmed: &str) -> bool {
    !trimmed.is_empty() && trimmed.chars().all(|ch| matches!(ch, '}' | ']' | ')'))
}

fn line_ending_diagnostics(text: &str) -> Vec<Diagnostic> {
    let mut has_lf = false;
    let mut has_crlf = false;
    let mut has_cr = false;
    let bytes = text.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                has_crlf = true;
                index += 2;
            }
            b'\r' => {
                has_cr = true;
                index += 1;
            }
            b'\n' => {
                has_lf = true;
                index += 1;
            }
            _ => index += 1,
        }
    }

    let kinds = [has_lf, has_crlf, has_cr]
        .into_iter()
        .filter(|present| *present)
        .count();

    if kinds > 1 {
        vec![Diagnostic::warn(1, "source uses inconsistent line endings")]
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, Copy)]
struct PhysicalLine<'a> {
    number: usize,
    text: &'a str,
}

fn split_lines(text: &str) -> Vec<PhysicalLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut number = 1;
    let bytes = text.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                lines.push(PhysicalLine {
                    number,
                    text: &text[start..index],
                });
                index += 2;
                start = index;
                number += 1;
            }
            b'\r' | b'\n' => {
                lines.push(PhysicalLine {
                    number,
                    text: &text[start..index],
                });
                index += 1;
                start = index;
                number += 1;
            }
            _ => index += 1,
        }
    }

    if start < text.len() || text.is_empty() {
        lines.push(PhysicalLine {
            number,
            text: &text[start..],
        });
    }

    lines
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::core::{Builtin, Expr as CoreExpr, Key, KeyExpr as CoreKeyExpr, Value};

    fn core_append(left: CoreExpr, right: CoreExpr) -> CoreExpr {
        core_builtin2(Builtin::Append, left, right)
    }

    fn core_singleton(key: CoreExpr, value: CoreExpr) -> CoreExpr {
        core_builtin2(Builtin::Singleton, key, value)
    }

    fn core_dict_union(left: CoreExpr, right: CoreExpr) -> CoreExpr {
        core_builtin2(Builtin::DictUnion, left, right)
    }

    fn core_builtin2(builtin: Builtin, left: CoreExpr, right: CoreExpr) -> CoreExpr {
        CoreExpr::Apply(
            Arc::new(CoreExpr::Apply(
                Arc::new(CoreExpr::Value(Value::Builtin(builtin))),
                Arc::new(left),
            )),
            Arc::new(right),
        )
    }

    use super::*;
    use crate::diagnostic::Severity;

    fn parse(text: &str) -> ParsedSource {
        SourceFile::new("test.g", text).parse()
    }

    #[test]
    fn parses_language_declaration_with_extensions() {
        let parsed = parse("language g0 with utf8, demo\nanswer = 42\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[0].kind,
            DeclarationKind::Language(LanguageDecl {
                base: "g0".to_owned(),
                extensions: vec!["utf8".to_owned(), "demo".to_owned()],
            })
        );
    }

    #[test]
    fn groups_indented_continuation_lines() {
        let parsed = parse("language g0\nfoo = do\n  .bar\n  .baz\nqux := 1\n");

        assert_eq!(parsed.declarations.len(), 3);
        assert_eq!(parsed.declarations[1].text, "foo = do\n.bar\n.baz");
        assert_eq!(
            parsed.declarations[2].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "qux".to_owned(),
                kind: DefinitionKind::Override,
                body: "1".to_owned(),
                expr: Some(SyntaxExpr::Number(1)),
            })
        );
    }

    #[test]
    fn parses_local_imports() {
        let parsed = parse("language g0\nimport \"minimal.g\" as conf\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Import(ImportDecl {
                reference: "minimal.g".to_owned(),
                placement: ImportPlacement::As("conf".to_owned()),
            })
        );
    }

    #[test]
    fn reports_missing_language_declaration() {
        let parsed = parse("foo = 1\n");

        assert_eq!(parsed.diagnostics.len(), 1);
        assert_eq!(parsed.diagnostics[0].severity, Severity::Error);
    }

    #[test]
    fn warns_on_inconsistent_line_endings() {
        let parsed = parse("language g0\r\nfoo = 1\n");

        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diag| diag.message.contains("inconsistent line endings"))
        );
    }

    #[test]
    fn parses_definition_forms() {
        assert_eq!(
            definition_decl().parse("foo = 1").into_result(),
            Ok(DefinitionDecl {
                target: "foo".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "1".to_owned(),
                expr: Some(SyntaxExpr::Number(1)),
            })
        );
        assert_eq!(
            definition_decl().parse("foo := 1").into_result(),
            Ok(DefinitionDecl {
                target: "foo".to_owned(),
                kind: DefinitionKind::Override,
                body: "1".to_owned(),
                expr: Some(SyntaxExpr::Number(1)),
            })
        );
        assert_eq!(
            definition_decl().parse("foo ::= f").into_result(),
            Ok(DefinitionDecl {
                target: "foo".to_owned(),
                kind: DefinitionKind::Update,
                body: "f".to_owned(),
                expr: Some(SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom("f".to_owned())])),
            })
        );
    }

    #[test]
    fn parses_inline_text_literal_expressions() {
        let parsed = parse("language g0\nasm.result = \"Hello, World!\"\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "\"Hello, World!\"".to_owned(),
                expr: Some(SyntaxExpr::Text("Hello, World!".to_owned())),
            })
        );
    }

    #[test]
    fn parses_integer_literals() {
        let parsed = parse("language g0\nanswer = 42\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "answer".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "42".to_owned(),
                expr: Some(SyntaxExpr::Number(42)),
            })
        );
    }

    #[test]
    fn parses_list_and_append_expressions() {
        let parsed = parse("language g0\nbytes = [1, 2] ++ [3, 4]\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "bytes".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "[1, 2] ++ [3, 4]".to_owned(),
                expr: Some(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::List(vec![
                        SyntaxExpr::Number(1),
                        SyntaxExpr::Number(2),
                    ])),
                    Box::new(SyntaxExpr::List(vec![
                        SyntaxExpr::Number(3),
                        SyntaxExpr::Number(4),
                    ])),
                )),
            })
        );
    }

    #[test]
    fn parses_name_and_append_expressions() {
        let parsed = parse("language g0\nasm.result = hello ++ \", \" ++ world ++ \"!\"\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "hello ++ \", \" ++ world ++ \"!\"".to_owned(),
                expr: Some(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::Append(
                        Box::new(SyntaxExpr::Append(
                            Box::new(SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom(
                                "hello".to_owned(),
                            )])),
                            Box::new(SyntaxExpr::Text(", ".to_owned())),
                        )),
                        Box::new(SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom(
                            "world".to_owned(),
                        )])),
                    )),
                    Box::new(SyntaxExpr::Text("!".to_owned())),
                )),
            })
        );
    }

    #[test]
    fn parses_dictionary_literals() {
        let parsed = parse("language g0\nd = { hello:\"Hello\", world:\"World\" }\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "d".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "{ hello:\"Hello\", world:\"World\" }".to_owned(),
                expr: Some(SyntaxExpr::DictUnion(vec![
                    SyntaxExpr::SingletonDict(
                        SyntaxKeyExpr::Atom("hello".to_owned()),
                        Box::new(SyntaxExpr::Text("Hello".to_owned())),
                    ),
                    SyntaxExpr::SingletonDict(
                        SyntaxKeyExpr::Atom("world".to_owned()),
                        Box::new(SyntaxExpr::Text("World".to_owned())),
                    ),
                ])),
            })
        );
    }

    #[test]
    fn parses_dictionary_unions() {
        let parsed = parse("language g0\nd = { left, right, hello:\"Hello\" }\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "d".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "{ left, right, hello:\"Hello\" }".to_owned(),
                expr: Some(SyntaxExpr::DictUnion(vec![
                    SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom("left".to_owned())]),
                    SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom("right".to_owned())]),
                    SyntaxExpr::SingletonDict(
                        SyntaxKeyExpr::Atom("hello".to_owned()),
                        Box::new(SyntaxExpr::Text("Hello".to_owned())),
                    ),
                ])),
            })
        );
    }

    #[test]
    fn parses_multiline_literals_with_leading_commas() {
        let parsed = parse(
            "language g0\nnums = [\n  , 1\n  , 2\n  ]\nd = {\n  , hello:\"Hello\"\n  , world:\"World\"\n  }\n",
        );

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "nums".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "[\n, 1\n, 2\n]".to_owned(),
                expr: Some(SyntaxExpr::List(vec![
                    SyntaxExpr::Number(1),
                    SyntaxExpr::Number(2),
                ])),
            })
        );
        assert_eq!(
            parsed.declarations[2].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "d".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "{\n, hello:\"Hello\"\n, world:\"World\"\n}".to_owned(),
                expr: Some(SyntaxExpr::DictUnion(vec![
                    SyntaxExpr::SingletonDict(
                        SyntaxKeyExpr::Atom("hello".to_owned()),
                        Box::new(SyntaxExpr::Text("Hello".to_owned())),
                    ),
                    SyntaxExpr::SingletonDict(
                        SyntaxKeyExpr::Atom("world".to_owned()),
                        Box::new(SyntaxExpr::Text("World".to_owned())),
                    ),
                ])),
            })
        );
    }

    #[test]
    fn parses_expression_indexed_names_and_keys() {
        let parsed =
            parse("language g0\nd = { [42]:\"World\" }\nasm.result = d.[42] ++ d.['tail]\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "d".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "{ [42]:\"World\" }".to_owned(),
                expr: Some(SyntaxExpr::DictUnion(vec![SyntaxExpr::SingletonDict(
                    SyntaxKeyExpr::Expr(Box::new(SyntaxExpr::Number(42))),
                    Box::new(SyntaxExpr::Text("World".to_owned())),
                )])),
            })
        );
        assert_eq!(
            parsed.declarations[2].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "d.[42] ++ d.['tail]".to_owned(),
                expr: Some(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::Name(vec![
                        SyntaxKeyExpr::Atom("d".to_owned()),
                        SyntaxKeyExpr::Expr(Box::new(SyntaxExpr::Number(42))),
                    ])),
                    Box::new(SyntaxExpr::Name(vec![
                        SyntaxKeyExpr::Atom("d".to_owned()),
                        SyntaxKeyExpr::Atom("tail".to_owned()),
                    ])),
                )),
            })
        );
    }

    #[test]
    fn parses_path_list_shorthand_and_general_list_path_exprs() {
        let parsed = parse("language g0\nasm.result = foo.[1,2,3] ++ foo.([1,2] ++ [3])\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "foo.[1,2,3] ++ foo.([1,2] ++ [3])".to_owned(),
                expr: Some(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::Name(vec![
                        SyntaxKeyExpr::Atom("foo".to_owned()),
                        SyntaxKeyExpr::Expr(Box::new(SyntaxExpr::Number(1))),
                        SyntaxKeyExpr::Expr(Box::new(SyntaxExpr::Number(2))),
                        SyntaxKeyExpr::Expr(Box::new(SyntaxExpr::Number(3))),
                    ])),
                    Box::new(SyntaxExpr::Name(vec![
                        SyntaxKeyExpr::Atom("foo".to_owned()),
                        SyntaxKeyExpr::ListExpr(Box::new(SyntaxExpr::Append(
                            Box::new(SyntaxExpr::List(vec![
                                SyntaxExpr::Number(1),
                                SyntaxExpr::Number(2),
                            ])),
                            Box::new(SyntaxExpr::List(vec![SyntaxExpr::Number(3)])),
                        ))),
                    ])),
                )),
            })
        );
    }

    #[test]
    fn dotted_paths_require_tight_dots() {
        assert!(matches!(
            parse_expr("foo.[  42  ].bar"),
            Some(SyntaxExpr::Name(_))
        ));
        assert!(matches!(
            parse_expr("foo.([1,2] ++ [3]).bar"),
            Some(SyntaxExpr::Name(_))
        ));

        assert_eq!(
            parse_expr("foo  .[42].bar"),
            None,
            "whitespace before `.` should be rejected"
        );
        assert_eq!(
            parse_expr("foo .bar"),
            None,
            "whitespace before `.` should prevent dotted-path parsing"
        );
        assert_eq!(
            parse_expr("foo.[42].  bar"),
            None,
            "whitespace after `.` should be rejected"
        );
        assert_eq!(
            parse_expr("foo. bar"),
            None,
            "whitespace after `.` should prevent dotted-path parsing"
        );
        assert_eq!(
            parse_expr("foo. [42].bar"),
            None,
            "whitespace between `.` and `[` should be rejected"
        );
        assert_eq!(
            parse_expr("foo. ([1,2] ++ [3]).bar"),
            None,
            "whitespace between `.` and `(` should be rejected"
        );
    }

    #[test]
    fn lowers_list_expressions_to_core_terms() {
        let parsed = parse("language g0\nasm.result = [72, 101] ++ [108, 108, 111]\n");
        let lowered = lower_to_core(&parsed);

        assert_eq!(lowered.diagnostics, []);
        assert_eq!(
            lowered.term.as_ref().and_then(|term| match term {
                crate::core::Term::Expr(CoreExpr::Value(value)) => value.get_atom_path(&[
                    Atom::from_key(&Key::binary_from_text("asm")),
                    Atom::from_key(&Key::binary_from_text("result")),
                ]),
                _ => None,
            }),
            Some(&Value::Expr(Arc::new(core_append(
                CoreExpr::List(Arc::from([
                    Arc::new(CoreExpr::Value(Value::Number(72))),
                    Arc::new(CoreExpr::Value(Value::Number(101))),
                ])),
                CoreExpr::List(Arc::from([
                    Arc::new(CoreExpr::Value(Value::Number(108))),
                    Arc::new(CoreExpr::Value(Value::Number(108))),
                    Arc::new(CoreExpr::Value(Value::Number(111))),
                ])),
            ))))
        );
    }

    #[test]
    fn lowers_name_expressions_to_core_terms() {
        let parsed = parse(
            "language g0\nasm.result = hello ++ \", \" ++ world ++ \"!\"\nhello = \"Hello\"\nworld = \"World\"\n",
        );
        let lowered = lower_to_core(&parsed);

        assert_eq!(lowered.diagnostics, []);
        assert_eq!(
            lowered.term.as_ref().and_then(|term| match term {
                crate::core::Term::Expr(CoreExpr::Value(value)) => value.get_atom_path(&[
                    Atom::from_key(&Key::binary_from_text("asm")),
                    Atom::from_key(&Key::binary_from_text("result")),
                ]),
                _ => None,
            }),
            Some(&Value::Expr(Arc::new(core_append(
                core_append(
                    core_append(
                        CoreExpr::Name(Arc::from([CoreKeyExpr::Key(
                            Key::atom_from_text("hello",)
                        )])),
                        CoreExpr::Value(Value::binary_from_text(", ")),
                    ),
                    CoreExpr::Name(Arc::from([CoreKeyExpr::Key(Key::atom_from_text("world",))])),
                ),
                CoreExpr::Value(Value::binary_from_text("!")),
            ))))
        );
    }

    #[test]
    fn lowers_dictionary_literals_to_lazy_values() {
        let parsed = parse(
            "language g0\nd = { hello:\"Hello\", world:other ++ \"!\" }\nother = \"World\"\n",
        );
        let lowered = lower_to_core(&parsed);

        assert_eq!(lowered.diagnostics, []);
        assert_eq!(
            lowered.term.as_ref().and_then(|term| match term {
                crate::core::Term::Expr(CoreExpr::Value(value)) => {
                    value.get_atom_path(&[Atom::from_key(&Key::binary_from_text("d"))])
                }
                _ => None,
            }),
            Some(&Value::Expr(Arc::new(core_dict_union(
                core_singleton(
                    CoreExpr::Value(Value::Atom(Atom::from_key(&Key::binary_from_text("hello")))),
                    CoreExpr::Value(Value::binary_from_text("Hello")),
                ),
                core_singleton(
                    CoreExpr::Value(Value::Atom(Atom::from_key(&Key::binary_from_text("world")))),
                    core_append(
                        CoreExpr::Name(Arc::from([CoreKeyExpr::Key(
                            Key::atom_from_text("other",)
                        )])),
                        CoreExpr::Value(Value::binary_from_text("!")),
                    ),
                ),
            ))))
        );
    }
}
