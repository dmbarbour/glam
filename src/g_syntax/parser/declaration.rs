use chumsky::prelude::*;

use super::super::{
    Declaration, DeclarationKind, DefinitionDecl, DefinitionKind, Diagnostic, ImportDecl,
    ImportPlacement, ImportReference, LanguageDecl, ObjectBodyDefinition, ObjectBodyDefinitionKind,
    ObjectDecl, ObjectExtendDecl, PathSuffix, SyntaxKeyExpr, flatten_path_suffixes,
    warn_unused_locals, warn_unused_with_alias,
};
use super::layout::{
    first_word, glam_name, indentation_width, is_indented, local_name, strip_indent_width,
    whitespace0, whitespace1,
};
use super::{parse_expr_result_with_diagnostics, syntax_expr_parser};

pub(super) fn validate_language_position(
    declarations: &[Declaration],
    diagnostics: &mut Vec<Diagnostic>,
) {
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

pub(super) fn classify_declaration(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> DeclarationKind {
    match first_word(text) {
        Some("object") => return classify_object_declaration(text, line, diagnostics),
        Some("extend") => return classify_extend_declaration(text, line, diagnostics),
        _ => {}
    }

    let (declaration, errors) = declaration_parser().parse(text).into_output_errors();

    for error in errors {
        diagnostics.push(Diagnostic::error(line, error.to_string()));
    }

    if let Some(declaration) = declaration {
        match declaration {
            DeclarationKind::Definition(definition) => {
                DeclarationKind::Definition(finalize_definition_expr(definition, line, diagnostics))
            }
            other => other,
        }
    } else {
        DeclarationKind::Unknown
    }
}

fn classify_object_declaration(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> DeclarationKind {
    match parse_object_declaration(text, line, diagnostics) {
        Some(object) => DeclarationKind::Object(object),
        None => DeclarationKind::Unknown,
    }
}

fn parse_object_declaration(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectDecl> {
    let mut lines = text.lines();
    let header = lines.next()?.trim();
    let body_lines = lines.collect::<Vec<_>>();
    let header = header.strip_prefix("object")?.trim();

    let (target, rest) = take_header_word(header).unwrap_or(("", ""));
    if target.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "object declaration requires a name",
        ));
        return None;
    }
    if target == "_" {
        diagnostics.push(Diagnostic::error(
            line,
            "anonymous object declarations are not supported by the current spike",
        ));
        return None;
    }
    if !path().parse(target).into_result().is_ok() {
        diagnostics.push(Diagnostic::error(
            line,
            "object declaration requires a path name",
        ));
        return None;
    }

    let header_tail = parse_object_header_tail(rest.trim(), line, diagnostics)?;
    if !body_lines.is_empty() && !header_tail.has_with {
        diagnostics.push(Diagnostic::error(
            line,
            "object body requires `with` in the declaration header",
        ));
        return None;
    }

    let body = parse_object_body(&body_lines, line + 1, diagnostics);
    if let Some(alias) = &header_tail.alias {
        warn_unused_with_alias(alias, &body, line, diagnostics);
    }

    Some(ObjectDecl {
        target: target.to_owned(),
        alias: header_tail.alias,
        deps: header_tail.deps,
        body,
    })
}

struct ObjectHeaderTail {
    alias: Option<String>,
    deps: Vec<String>,
    has_with: bool,
}

fn parse_object_header_tail(
    rest: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectHeaderTail> {
    let (alias, rest) = parse_optional_object_alias(rest, line, diagnostics)?;
    if rest.is_empty() {
        return Some(ObjectHeaderTail {
            alias,
            deps: Vec::new(),
            has_with: false,
        });
    }
    if rest == "with" {
        return Some(ObjectHeaderTail {
            alias,
            deps: Vec::new(),
            has_with: true,
        });
    }

    let Some(after_extends) = rest.strip_prefix("extends").map(str::trim) else {
        diagnostics.push(Diagnostic::error(
            line,
            "object declarations currently support only `extends ...` and `with` after the name",
        ));
        return None;
    };
    if after_extends.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "object `extends` requires at least one dependency",
        ));
        return None;
    }

    let (deps_text, has_with) = match after_extends.strip_suffix(" with") {
        Some(deps) => (deps.trim(), true),
        None if after_extends == "with" => {
            diagnostics.push(Diagnostic::error(
                line,
                "object `extends` requires at least one dependency",
            ));
            return None;
        }
        None => (after_extends, false),
    };
    let deps = deps_text
        .split(',')
        .map(str::trim)
        .filter(|dep| !dep.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if deps.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "object `extends` requires at least one dependency",
        ));
        return None;
    }
    for dep in &deps {
        if !path().parse(dep.as_str()).into_result().is_ok() {
            diagnostics.push(Diagnostic::error(
                line,
                format!("object dependency `{dep}` is not a path name"),
            ));
            return None;
        }
    }

    Some(ObjectHeaderTail {
        alias,
        deps,
        has_with,
    })
}

fn parse_optional_object_alias<'a>(
    rest: &'a str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(Option<String>, &'a str)> {
    let rest = rest.trim();
    let Some((first, tail)) = take_header_word(rest) else {
        return Some((None, ""));
    };
    if first != "as" {
        return Some((None, rest));
    }

    let Some((alias, tail)) = take_header_word(tail) else {
        diagnostics.push(Diagnostic::error(
            line,
            "`as` requires an object alias name",
        ));
        return None;
    };
    if !local_name().parse(alias).into_result().is_ok() {
        diagnostics.push(Diagnostic::error(
            line,
            format!("object alias `{alias}` is not a valid local name"),
        ));
        return None;
    }
    Some((Some(alias.to_owned()), tail.trim()))
}

pub(super) fn parse_object_body(
    lines: &[&str],
    first_line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ObjectBodyDefinition> {
    let mut body = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            index += 1;
            continue;
        }

        let line_number = first_line + index;
        if is_indented(line) {
            diagnostics.push(Diagnostic::error(
                line_number,
                "object body continuation line without a preceding nested declaration",
            ));
            index += 1;
            continue;
        }

        let mut text = trimmed.to_owned();
        index += 1;
        let mut continuation_indent = None;
        while index < lines.len() {
            let next = lines[index];
            let next_trimmed = next.trim();
            if next_trimmed.is_empty() {
                index += 1;
                continue;
            }
            if !is_indented(next) {
                break;
            }
            if continuation_indent.is_none() {
                continuation_indent = Some(indentation_width(next));
            }
            let next_text = continuation_indent
                .map(|indent| strip_indent_width(next.trim_end(), indent))
                .unwrap_or(next_trimmed);
            text.push('\n');
            text.push_str(next_text.trim_end());
            index += 1;
        }

        if let Some(definition) = parse_object_body_definition(&text, line_number, diagnostics) {
            body.push(definition);
        }
    }

    body
}

fn parse_object_body_definition(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectBodyDefinition> {
    if text.trim_start().starts_with("object ") {
        let object = parse_object_declaration(text, line, diagnostics)?;
        return Some(ObjectBodyDefinition {
            line,
            text: text.to_owned(),
            kind: ObjectBodyDefinitionKind::Object(object),
        });
    }

    let (declaration, errors) = definition_decl().parse(text).into_output_errors();
    for error in errors {
        diagnostics.push(Diagnostic::error(line, error.to_string()));
    }

    let definition = declaration?;
    Some(ObjectBodyDefinition {
        line,
        text: text.to_owned(),
        kind: ObjectBodyDefinitionKind::Definition(finalize_definition_expr(
            definition,
            line,
            diagnostics,
        )),
    })
}

fn classify_extend_declaration(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> DeclarationKind {
    match parse_extend_declaration(text, line, diagnostics) {
        Some(extend) => DeclarationKind::Extend(extend),
        None => DeclarationKind::Unknown,
    }
}

fn parse_extend_declaration(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectExtendDecl> {
    let mut lines = text.lines();
    let header = lines.next()?.trim();
    let body_lines = lines.collect::<Vec<_>>();
    let header = header.strip_prefix("extend")?.trim();

    let (target, rest) = take_header_word(header).unwrap_or(("", ""));
    if target.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declaration requires a name",
        ));
        return None;
    }
    let (alias, rest) = parse_optional_object_alias(rest, line, diagnostics)?;
    if rest != "with" {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declarations currently require `extend name (as alias)? with`",
        ));
        return None;
    }
    if !path().parse(target).into_result().is_ok() {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declaration requires a path name",
        ));
        return None;
    }
    if body_lines.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declaration requires a body",
        ));
        return None;
    }

    let body = parse_object_body(&body_lines, line + 1, diagnostics);
    if let Some(alias) = &alias {
        warn_unused_with_alias(alias, &body, line, diagnostics);
    }

    Some(ObjectExtendDecl {
        target: target.to_owned(),
        alias,
        body,
    })
}

pub(super) fn take_header_word(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start();
    if text.is_empty() {
        return None;
    }
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    Some((&text[..end], &text[end..]))
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
    let reference = choice((
        quoted_text().map(ImportReference::Local),
        just('\'')
            .ignore_then(glam_name())
            .map(ImportReference::Builtin),
    ));
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

    let binary = just("binary")
        .padded()
        .to(true)
        .or_not()
        .map(|v| v.unwrap_or(false));

    just("import")
        .padded()
        .ignore_then(reference)
        .then(binary)
        .then(placement)
        .map(|((reference, binary), placement)| ImportDecl {
            reference,
            binary,
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

pub(in crate::g_syntax) fn definition_decl<'src>()
-> impl Parser<'src, &'src str, DefinitionDecl, extra::Err<Rich<'src, char>>> {
    definition_target()
        .then(
            whitespace1().ignore_then(
                local_name()
                    .then_ignore(whitespace1())
                    .repeated()
                    .collect::<Vec<_>>(),
            ),
        )
        .then(definition_operator())
        .then_ignore(whitespace0())
        .then(rest_of_declaration())
        .try_map(|(((target, params), kind), body), span| {
            if body.is_empty() {
                Err(Rich::custom(span, "definition body cannot be empty"))
            } else {
                Ok(DefinitionDecl {
                    target,
                    kind,
                    body: desugar_definition_body(kind, &params, body),
                    expr: None,
                })
            }
        })
}

fn definition_target<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    definition_target_path().to_slice().map(ToOwned::to_owned)
}

pub(in crate::g_syntax) fn definition_target_path<'src>()
-> impl Parser<'src, &'src str, Vec<SyntaxKeyExpr>, extra::Err<Rich<'src, char>>> {
    let name = glam_name().boxed();
    let expr = syntax_expr_parser().boxed();
    let single_key_expr = || {
        choice((
            just('\'')
                .ignore_then(name.clone())
                .map(SyntaxKeyExpr::Atom),
            expr.clone()
                .map(|expr| SyntaxKeyExpr::Index(Box::new(expr))),
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
        .padded()
        .delimited_by(just('('), just(')'))
        .map(|expr| PathSuffix::Single(SyntaxKeyExpr::PathIndex(Box::new(expr))));
    let path_suffix_item = just('.').ignore_then(choice((
        path_list_shorthand,
        path_list_expr,
        name.clone()
            .map(SyntaxKeyExpr::Atom)
            .map(PathSuffix::Single),
    )));
    let path_suffix = path_suffix_item.clone().repeated().collect::<Vec<_>>();

    choice((
        name.clone()
            .map(SyntaxKeyExpr::Atom)
            .then(path_suffix.clone())
            .map(|(name, suffixes)| {
                let mut parts = vec![name];
                parts.extend(flatten_path_suffixes(suffixes));
                parts
            }),
        path_suffix_item
            .repeated()
            .at_least(1)
            .collect::<Vec<_>>()
            .map(flatten_path_suffixes),
    ))
}

pub(in crate::g_syntax) fn definition_target_parts(
    target: &str,
    line: usize,
) -> Result<Vec<SyntaxKeyExpr>, Diagnostic> {
    definition_target_path()
        .then_ignore(end())
        .parse(target)
        .into_result()
        .map_err(|errors| {
            Diagnostic::error(
                line,
                errors
                    .into_iter()
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
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

pub(super) fn quoted_text<'src>()
-> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
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

fn desugar_definition_body(kind: DefinitionKind, params: &[String], body: String) -> String {
    let _ = kind;
    if params.is_empty() {
        body
    } else {
        format!("\\ {} -> {}", params.join(" "), body)
    }
}

fn finalize_definition_expr(
    mut definition: DefinitionDecl,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> DefinitionDecl {
    match parse_expr_result_with_diagnostics(definition.body.as_str(), line, diagnostics) {
        Ok(expr) => {
            warn_unused_locals(&expr, line, diagnostics);
            definition.expr = Some(expr);
        }
        Err(message) => diagnostics.push(Diagnostic::error(line, message)),
    }
    definition
}
