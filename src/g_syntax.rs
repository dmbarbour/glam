use chumsky::prelude::*;

use std::sync::Arc;

use crate::core::{Atom, Dict, Expr as CoreExpr, Key, Term, Value};
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
    List(Vec<SyntaxExpr>),
    Append(Box<SyntaxExpr>, Box<SyntaxExpr>),
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

    let value = syntax_expr_to_value(expr, line)?;

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

fn syntax_expr_to_value(expr: &SyntaxExpr, line: usize) -> Result<Value, Diagnostic> {
    match expr {
        SyntaxExpr::Number(number) => Ok(Value::Number(*number)),
        SyntaxExpr::Text(text) => Ok(Value::binary_from_text(text)),
        SyntaxExpr::List(_) | SyntaxExpr::Append(_, _) => {
            Ok(Value::Expr(Arc::new(syntax_expr_to_core_expr(expr, line)?)))
        }
    }
}

fn syntax_expr_to_core_expr(expr: &SyntaxExpr, line: usize) -> Result<CoreExpr, Diagnostic> {
    Ok(match expr {
        SyntaxExpr::Number(number) => CoreExpr::Value(Value::Number(*number)),
        SyntaxExpr::Text(text) => CoreExpr::Value(Value::binary_from_text(text)),
        SyntaxExpr::List(items) => CoreExpr::List(Arc::from(
            items
                .iter()
                .map(|expr| syntax_expr_to_core_expr(expr, line).map(Arc::new))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        SyntaxExpr::Append(left, right) => CoreExpr::Append(
            Arc::new(syntax_expr_to_core_expr(left, line)?),
            Arc::new(syntax_expr_to_core_expr(right, line)?),
        ),
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
    Key::Atom(
        atoms
            .entry(name.to_owned())
            .or_insert_with(|| Atom::from_key(&Key::text(name)))
            .clone(),
    )
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
    let (expr, rest) = parse_append(text).ok()?;
    if rest.trim().is_empty() {
        Some(expr)
    } else {
        None
    }
}

fn parse_append(text: &str) -> Result<(SyntaxExpr, &str), String> {
    let (mut expr, mut rest) = parse_atom(text)?;

    loop {
        let trimmed = rest.trim_start();
        if let Some(after_op) = trimmed.strip_prefix("++") {
            let (rhs, new_rest) = parse_atom(after_op)?;
            expr = SyntaxExpr::Append(Box::new(expr), Box::new(rhs));
            rest = new_rest;
        } else {
            return Ok((expr, rest));
        }
    }
}

fn parse_atom(text: &str) -> Result<(SyntaxExpr, &str), String> {
    let text = text.trim_start();

    if let Some(rest) = text.strip_prefix('"') {
        return parse_text_literal(rest);
    }

    if let Some(rest) = text.strip_prefix('[') {
        return parse_list_literal(rest);
    }

    if text.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        return parse_integer_literal(text);
    }

    Err("unsupported expression".to_owned())
}

fn parse_text_literal(text: &str) -> Result<(SyntaxExpr, &str), String> {
    for (index, ch) in text.char_indices() {
        if ch == '"' {
            let value = &text[..index];
            let rest = &text[index + 1..];
            return Ok((SyntaxExpr::Text(value.to_owned()), rest));
        }
    }

    Err("unterminated text literal".to_owned())
}

fn parse_integer_literal(text: &str) -> Result<(SyntaxExpr, &str), String> {
    let digit_count = text.chars().take_while(|ch| ch.is_ascii_digit()).count();

    let digits = &text[..digit_count];
    let rest = &text[digit_count..];

    let number = digits
        .parse::<i64>()
        .map_err(|err| format!("invalid integer literal `{digits}`: {err}"))?;

    Ok((SyntaxExpr::Number(number), rest))
}

fn parse_list_literal(text: &str) -> Result<(SyntaxExpr, &str), String> {
    let mut rest = text;
    let mut items = Vec::new();

    loop {
        rest = rest.trim_start();
        if let Some(after_close) = rest.strip_prefix(']') {
            return Ok((SyntaxExpr::List(items), after_close));
        }

        let (item, new_rest) = parse_append(rest)?;
        items.push(item);
        rest = new_rest.trim_start();

        if let Some(after_comma) = rest.strip_prefix(',') {
            rest = after_comma;
            continue;
        }

        if let Some(after_close) = rest.strip_prefix(']') {
            return Ok((SyntaxExpr::List(items), after_close));
        }

        return Err("expected `,` or `]` in list literal".to_owned());
    }
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
    use super::*;
    use crate::core::Value;
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
                expr: None,
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
    fn lowers_list_expressions_to_core_terms() {
        let parsed = parse("language g0\nasm.result = [72, 101] ++ [108, 108, 111]\n");
        let lowered = lower_to_core(&parsed);

        assert_eq!(lowered.diagnostics, []);
        assert_eq!(
            lowered.term.as_ref().and_then(|term| match term {
                crate::core::Term::Expr(CoreExpr::Value(value)) => value.get_atom_path(&[
                    Atom::from_key(&Key::text("asm")),
                    Atom::from_key(&Key::text("result")),
                ]),
                _ => None,
            }),
            Some(&Value::Expr(Arc::new(CoreExpr::Append(
                Arc::new(CoreExpr::List(Arc::from([
                    Arc::new(CoreExpr::Value(Value::Number(72))),
                    Arc::new(CoreExpr::Value(Value::Number(101))),
                ]))),
                Arc::new(CoreExpr::List(Arc::from([
                    Arc::new(CoreExpr::Value(Value::Number(108))),
                    Arc::new(CoreExpr::Value(Value::Number(108))),
                    Arc::new(CoreExpr::Value(Value::Number(111))),
                ]))),
            ))))
        );
    }
}
