use std::fmt;

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
    Import,
    Abstract,
    Unique,
    Object,
    Extend,
    Definition(DefinitionKind),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageDecl {
    pub base: String,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionKind {
    Introduce,
    Override,
    Update,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Warning => f.write_str("warning"),
            Severity::Error => f.write_str("error"),
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
    let head = first_word(text);

    match head {
        Some("language") | Some("lang") => parse_language_decl(text, line, diagnostics)
            .map(DeclarationKind::Language)
            .unwrap_or(DeclarationKind::Unknown),
        Some("import") => DeclarationKind::Import,
        Some("abstract") => DeclarationKind::Abstract,
        Some("unique") => DeclarationKind::Unique,
        Some("object") => DeclarationKind::Object,
        Some("extend") | Some("extends") => DeclarationKind::Extend,
        _ => {
            classify_definition(text).map_or(DeclarationKind::Unknown, DeclarationKind::Definition)
        }
    }
}

fn parse_language_decl(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<LanguageDecl> {
    let mut words = text.split_whitespace();
    let keyword = words.next()?;
    debug_assert!(keyword == "language" || keyword == "lang");

    let Some(base) = words.next() else {
        diagnostics.push(Diagnostic::error(
            line,
            "language declaration is missing a base version",
        ));
        return None;
    };

    let mut extensions = Vec::new();
    match words.next() {
        None => {}
        Some("with") => {
            let rest = words.collect::<Vec<_>>().join(" ");
            extensions = rest
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(ToOwned::to_owned)
                .collect();

            if extensions.is_empty() {
                diagnostics.push(Diagnostic::error(
                    line,
                    "language declaration has `with` but no extensions",
                ));
            }
        }
        Some(other) => diagnostics.push(Diagnostic::error(
            line,
            format!("unexpected language declaration token `{other}`"),
        )),
    }

    Some(LanguageDecl {
        base: base.to_owned(),
        extensions,
    })
}

fn classify_definition(text: &str) -> Option<DefinitionKind> {
    let introduce = text.find('=');
    let update = text.find("::=");
    let override_ = text.find(":=");

    match (introduce, override_, update) {
        (_, _, Some(update_at))
            if introduce == Some(update_at + 2) && override_ == Some(update_at + 1) =>
        {
            Some(DefinitionKind::Update)
        }
        (_, Some(override_at), _) if introduce == Some(override_at + 1) => {
            Some(DefinitionKind::Override)
        }
        (Some(_), _, _) => Some(DefinitionKind::Introduce),
        _ => None,
    }
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
            DeclarationKind::Definition(DefinitionKind::Override)
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
    fn classifies_definition_forms() {
        assert_eq!(
            classify_definition("foo = 1"),
            Some(DefinitionKind::Introduce)
        );
        assert_eq!(
            classify_definition("foo := 1"),
            Some(DefinitionKind::Override)
        );
        assert_eq!(
            classify_definition("foo ::= f"),
            Some(DefinitionKind::Update)
        );
    }
}
