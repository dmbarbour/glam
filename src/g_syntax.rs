use chumsky::prelude::*;

use std::collections::BTreeMap;

use crate::core::{Key, Term, Value};
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
    pub expr: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionKind {
    Introduce,
    Override,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Text(String),
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
    let mut root = BTreeMap::new();
    let mut diagnostics = parsed.diagnostics.clone();

    for declaration in &parsed.declarations {
        let DeclarationKind::Definition(definition) = &declaration.kind else {
            continue;
        };

        match lower_definition(definition, declaration.line, &mut root) {
            Ok(()) => {}
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
    }

    LoweredSource {
        term: Some(Term::Data(Value::Dict(root))),
        diagnostics,
    }
}

fn lower_definition(
    definition: &DefinitionDecl,
    line: usize,
    root: &mut BTreeMap<Key, Value>,
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

    let value = match expr_to_core(expr) {
        Term::Data(value) => value,
    };

    match definition.kind {
        DefinitionKind::Introduce => insert_path(root, &definition.target, value, line),
        DefinitionKind::Override => override_path(root, &definition.target, value, line),
        DefinitionKind::Update => Err(Diagnostic::error(
            line,
            "update definitions are not supported by the .g spike lowering",
        )),
    }
}

fn expr_to_core(expr: &Expr) -> Term {
    match expr {
        Expr::Text(text) => Term::Data(Value::Text(text.clone())),
    }
}

fn insert_path(
    root: &mut BTreeMap<Key, Value>,
    target: &str,
    value: Value,
    line: usize,
) -> Result<(), Diagnostic> {
    let parts = target.split('.').collect::<Vec<_>>();
    let Some((leaf, parents)) = parts.split_last() else {
        return Err(Diagnostic::error(line, "definition target cannot be empty"));
    };

    let parent = ensure_parent_dict(root, parents, line)?;
    let leaf_key = Key::name(*leaf);
    if parent.contains_key(&leaf_key) {
        return Err(Diagnostic::error(
            line,
            format!("cannot introduce `{target}` because it is already defined"),
        ));
    }

    parent.insert(leaf_key, value);
    Ok(())
}

fn override_path(
    root: &mut BTreeMap<Key, Value>,
    target: &str,
    value: Value,
    line: usize,
) -> Result<(), Diagnostic> {
    let parts = target.split('.').collect::<Vec<_>>();
    let Some((leaf, parents)) = parts.split_last() else {
        return Err(Diagnostic::error(line, "definition target cannot be empty"));
    };

    let parent = ensure_parent_dict(root, parents, line)?;
    let leaf_key = Key::name(*leaf);
    if !parent.contains_key(&leaf_key) {
        return Err(Diagnostic::error(
            line,
            format!("cannot override `{target}` because it is not defined"),
        ));
    }

    parent.insert(leaf_key, value);
    Ok(())
}

fn ensure_parent_dict<'a>(
    root: &'a mut BTreeMap<Key, Value>,
    parents: &[&str],
    line: usize,
) -> Result<&'a mut BTreeMap<Key, Value>, Diagnostic> {
    let mut current = root;

    for parent in parents {
        let entry = current
            .entry(Key::name(*parent))
            .or_insert_with(|| Value::Dict(BTreeMap::new()));

        let Value::Dict(next) = entry else {
            return Err(Diagnostic::error(
                line,
                format!("cannot define below `{parent}` because it is not a dictionary"),
            ));
        };

        current = next;
    }

    Ok(current)
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
    let (declaration, errors) = declaration_parser().parse(text).into_output_errors();

    for error in errors {
        diagnostics.push(Diagnostic::error(line, error.to_string()));
    }

    if let Some(declaration) = declaration {
        declaration
    } else {
        match first_word(text) {
            Some("object") => DeclarationKind::Object,
            Some("extend") | Some("extends") => DeclarationKind::Extend,
            _ => DeclarationKind::Unknown,
        }
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
                let expr = expr_parser().parse(body.as_str()).into_result().ok();
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

fn expr_parser<'src>() -> impl Parser<'src, &'src str, Expr, extra::Err<Rich<'src, char>>> {
    inline_text_literal()
        .map(Expr::Text)
        .padded()
        .then_ignore(end())
}

fn inline_text_literal<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>>
{
    none_of('"')
        .repeated()
        .to_slice()
        .map(ToOwned::to_owned)
        .delimited_by(just('"'), just('"'))
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
                expr: None,
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
                expr: None,
            })
        );
        assert_eq!(
            definition_decl().parse("foo := 1").into_result(),
            Ok(DefinitionDecl {
                target: "foo".to_owned(),
                kind: DefinitionKind::Override,
                body: "1".to_owned(),
                expr: None,
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
                expr: Some(Expr::Text("Hello, World!".to_owned())),
            })
        );
    }

    #[test]
    fn lowers_text_literals_to_core_terms() {
        let parsed = parse("language g0\nasm.result = \"Hello, World!\"\n");
        let lowered = lower_to_core(&parsed);

        assert_eq!(lowered.diagnostics, []);
        assert_eq!(
            lowered.term.as_ref().and_then(|term| match term {
                crate::core::Term::Data(value) => value.get_name_path(&["asm", "result"]),
            }),
            Some(&Value::Text("Hello, World!".to_owned()))
        );
    }
}
