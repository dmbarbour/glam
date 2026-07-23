//! Token-native declaration and nested object-body parsing.
//!
//! A declaration is always parsed from one lexer-owned range. Nested object
//! bodies reuse the same parser over layout-selected subranges, so expressions,
//! delimiter groups, multiline text, and indentation have one interpretation.

use super::super::super::{
    DeclarationKind, DefinitionDecl, DefinitionKind, Diagnostic, ObjectBodyDefinition,
    ObjectBodyDefinitionKind, ObjectDecl, ObjectExtendDecl, SyntaxExpr, SyntaxKeyExpr,
    warn_unused_locals, warn_unused_with_alias,
};
use super::super::compound::token::{
    contextual_keywords, first_top_level_line_start, is_layout_empty, local_name, parse_expression,
    split_top_level, token_is_name, trim_layout, view_between,
};
use super::super::input::TokenView;
use super::super::layout::{LayoutBase, LayoutView};
use super::super::lexical::{Delimiter, LeadingTrivia, TokenKind, lex_source};

pub(in crate::g_syntax::parser) fn parse_declaration(
    view: TokenView<'_, '_>,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> DeclarationKind {
    let view = trim_layout(view);
    let Some((_, head)) = view.first_significant() else {
        return DeclarationKind::Unknown;
    };
    match head.kind() {
        TokenKind::Name("object") => parse_object_declaration(view, line, diagnostics)
            .map_or(DeclarationKind::Unknown, DeclarationKind::Object),
        TokenKind::Name("extend") => parse_extend_declaration(view, line, diagnostics)
            .map_or(DeclarationKind::Unknown, DeclarationKind::Extend),
        _ => parse_definition(view, line, diagnostics)
            .map_or(DeclarationKind::Unknown, DeclarationKind::Definition),
    }
}

pub(in crate::g_syntax::parser) fn parse_object_body(
    view: TokenView<'_, '_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ObjectBodyDefinition> {
    if is_layout_empty(view) {
        return Vec::new();
    }
    let statements = match LayoutView::new(view).statements(LayoutBase::FirstLine) {
        Ok(statements) => statements,
        Err(error) => {
            diagnostics.push(Diagnostic::error(error.line(), error.message()));
            return Vec::new();
        }
    };

    let mut body = Vec::with_capacity(statements.len());
    for statement in statements {
        let Some(statement_view) = view.subview(statement.tokens()) else {
            continue;
        };
        let statement_view = trim_layout(statement_view);
        let line = statement.line();
        let text = declaration_preview(statement_view);
        let kind = match statement_view.first_significant() {
            Some((_, token)) if token_is_name(token, "object") => {
                parse_object_declaration(statement_view, line, diagnostics)
                    .map(ObjectBodyDefinitionKind::Object)
            }
            _ => parse_definition(statement_view, line, diagnostics)
                .map(ObjectBodyDefinitionKind::Definition),
        };
        if let Some(kind) = kind {
            body.push(ObjectBodyDefinition { line, text, kind });
        }
    }
    body
}

fn parse_definition(
    view: TokenView<'_, '_>,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<DefinitionDecl> {
    let operator = view.top_level().find_map(|indexed| {
        let kind = match indexed.token().kind() {
            TokenKind::Symbol("::=") => DefinitionKind::Update,
            TokenKind::Symbol(":=") => DefinitionKind::Override,
            TokenKind::Symbol("=") => DefinitionKind::Introduce,
            _ => return None,
        };
        Some((indexed.index(), kind))
    });
    let Some((operator_index, kind)) = operator else {
        diagnostics.push(Diagnostic::error(line, "expected a declaration"));
        return None;
    };

    let left = trim_layout(view_between(view, view.range().start(), operator_index));
    let body_view = trim_layout(view_between(view, operator_index + 1, view.range().end()));
    if is_layout_empty(body_view) {
        diagnostics.push(Diagnostic::error(line, "definition body cannot be empty"));
        return None;
    }

    let (target, parameters) = match parse_definition_left(left, line) {
        Ok(parts) => parts,
        Err(diagnostic) => {
            diagnostics.push(diagnostic);
            return None;
        }
    };
    let body = body_view.source_text().unwrap_or("").trim().to_owned();
    let parsed_body = match parse_expression(body_view) {
        Ok(expr) => expr,
        Err(errors) => {
            diagnostics.extend(errors);
            return Some(DefinitionDecl {
                target,
                parameters,
                kind,
                body,
                expr: None,
            });
        }
    };
    let expr = if parameters.is_empty() {
        parsed_body
    } else {
        SyntaxExpr::Lambda(parameters.clone(), Box::new(parsed_body))
    };
    warn_unused_locals(&expr, line, diagnostics);

    Some(DefinitionDecl {
        target,
        parameters,
        kind,
        body,
        expr: Some(expr),
    })
}

fn parse_definition_left(
    view: TokenView<'_, '_>,
    line: usize,
) -> Result<(String, Vec<String>), Diagnostic> {
    let significant = view
        .top_level()
        .filter(|indexed| !matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
        .collect::<Vec<_>>();
    let Some(first) = significant.first().copied() else {
        return Err(Diagnostic::error(line, "definition requires a target"));
    };

    let mut position;
    let mut target_end;
    match first.token().kind() {
        TokenKind::Name(_) => {
            position = 1;
            target_end = first.index() + 1;
        }
        TokenKind::Symbol(".") => {
            position = 0;
            target_end = first.index();
        }
        _ => {
            return Err(Diagnostic::error(
                line,
                "definition target must be a name or path",
            ));
        }
    }

    while position < significant.len() {
        let dot = significant[position];
        if !matches!(dot.token().kind(), TokenKind::Symbol(".")) {
            break;
        }
        if position > 0 && dot.token().leading() != LeadingTrivia::Joint {
            break;
        }
        let Some(item) = significant.get(position + 1).copied() else {
            return Err(Diagnostic::error(line, "path suffix requires a key"));
        };
        if item.token().leading() != LeadingTrivia::Joint {
            return Err(Diagnostic::error(
                line,
                "path suffix key must immediately follow `.`",
            ));
        }
        target_end = match item.token().kind() {
            TokenKind::Name(_) => item.index() + 1,
            TokenKind::Open {
                group,
                delimiter: Delimiter::Bracket | Delimiter::Parenthesis,
            } => view
                .group(*group)
                .and_then(|group| group.close_token())
                .map(|close| close + 1)
                .ok_or_else(|| Diagnostic::error(line, "unclosed definition target path"))?,
            _ => {
                return Err(Diagnostic::error(
                    line,
                    "definition path suffix requires a name, list, or parenthesized path",
                ));
            }
        };
        position += 2;
    }

    let target_view = view_between(view, first.index(), target_end);
    parse_definition_target(target_view, line)?;
    let target = target_view.source_text().unwrap_or("").trim().to_owned();

    let mut parameters = Vec::new();
    for indexed in significant.into_iter().skip(position) {
        if indexed.index() < target_end {
            continue;
        }
        let parameter_view = view_between(view, indexed.index(), indexed.index() + 1);
        let Some(parameter) = local_name(parameter_view) else {
            return Err(Diagnostic::error(
                line,
                "definition parameters must be local names",
            ));
        };
        if indexed.token().leading() == LeadingTrivia::Joint {
            return Err(Diagnostic::error(
                line,
                "definition parameters must be separated from the target",
            ));
        }
        parameters.push(parameter.to_owned());
    }

    Ok((target, parameters))
}

pub(in crate::g_syntax) fn parse_definition_target_text(
    target: &str,
    line: usize,
) -> Result<Vec<SyntaxKeyExpr>, Diagnostic> {
    let lexical = lex_source(target);
    if lexical.has_errors() {
        return Err(Diagnostic::error(
            line,
            lexical
                .diagnostics()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    let Some((_, view)) = TokenView::declarations(&lexical).next() else {
        return Err(Diagnostic::error(line, "definition requires a target"));
    };
    parse_definition_target(trim_layout(view), line)
}

fn parse_definition_target(
    view: TokenView<'_, '_>,
    line: usize,
) -> Result<Vec<SyntaxKeyExpr>, Diagnostic> {
    let mut parts = Vec::new();
    let tokens = view.top_level().collect::<Vec<_>>();
    let mut position = 0;
    if let Some(first) = tokens.first()
        && let TokenKind::Name(name) = first.token().kind()
    {
        parts.push(SyntaxKeyExpr::Atom((*name).to_owned()));
        position = 1;
    }

    while position < tokens.len() {
        let dot = tokens[position];
        if !matches!(dot.token().kind(), TokenKind::Symbol(".")) {
            return Err(Diagnostic::error(line, "invalid definition target path"));
        }
        let Some(item) = tokens.get(position + 1).copied() else {
            return Err(Diagnostic::error(line, "path suffix requires a key"));
        };
        match item.token().kind() {
            TokenKind::Name(name) => parts.push(SyntaxKeyExpr::Atom((*name).to_owned())),
            TokenKind::Open {
                group,
                delimiter: Delimiter::Bracket,
            } => {
                let contents = view
                    .group_contents(*group)
                    .ok_or_else(|| Diagnostic::error(line, "invalid path list"))?;
                for item in split_top_level(contents, ",")
                    .into_iter()
                    .map(trim_layout)
                    .filter(|item| !is_layout_empty(*item))
                {
                    let expr = parse_expression(item)
                        .map_err(|errors| combine_parse_errors(line, errors))?;
                    parts.push(match expr {
                        SyntaxExpr::Atom(name) => SyntaxKeyExpr::Atom(name),
                        expr => SyntaxKeyExpr::Index(Box::new(expr)),
                    });
                }
            }
            TokenKind::Open {
                group,
                delimiter: Delimiter::Parenthesis,
            } => {
                let contents = view
                    .group_contents(*group)
                    .ok_or_else(|| Diagnostic::error(line, "invalid computed path"))?;
                let expr = parse_expression(contents)
                    .map_err(|errors| combine_parse_errors(line, errors))?;
                parts.push(SyntaxKeyExpr::PathIndex(Box::new(expr)));
            }
            _ => return Err(Diagnostic::error(line, "invalid definition target path")),
        }
        position += 2;
    }

    if parts.is_empty() {
        Err(Diagnostic::error(
            line,
            "definition target path cannot be empty",
        ))
    } else {
        Ok(parts)
    }
}

fn combine_parse_errors(line: usize, diagnostics: Vec<Diagnostic>) -> Diagnostic {
    Diagnostic::error(
        line,
        diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn parse_object_declaration(
    view: TokenView<'_, '_>,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectDecl> {
    let (header, body) = split_header_body(view);
    let head = header.first_significant()?.0;
    let header = trim_layout(view_between(header, head + 1, header.range().end()));
    let parsed = parse_static_object_header(header, line, false, diagnostics)?;
    if body.is_some() && !parsed.has_with {
        diagnostics.push(Diagnostic::error(
            line,
            "object body requires `with` in the declaration header",
        ));
        return None;
    }
    let body = body.map_or_else(Vec::new, |body| parse_object_body(body, diagnostics));
    if let Some(alias) = &parsed.alias {
        warn_unused_with_alias(alias, &body, line, diagnostics);
    }
    Some(ObjectDecl {
        target: parsed.target,
        alias: parsed.alias,
        deps: parsed.deps,
        body,
    })
}

fn parse_extend_declaration(
    view: TokenView<'_, '_>,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectExtendDecl> {
    let (header, body) = split_header_body(view);
    let head = header.first_significant()?.0;
    let header = trim_layout(view_between(header, head + 1, header.range().end()));
    let parsed = parse_static_object_header(header, line, true, diagnostics)?;
    if !parsed.has_with {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declarations currently require `extend name (as alias)? with`",
        ));
        return None;
    }
    let Some(body_view) = body else {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declaration requires a body",
        ));
        return None;
    };
    let body = parse_object_body(body_view, diagnostics);
    if body.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declaration requires a body",
        ));
        return None;
    }
    if let Some(alias) = &parsed.alias {
        warn_unused_with_alias(alias, &body, line, diagnostics);
    }
    Some(ObjectExtendDecl {
        target: parsed.target,
        alias: parsed.alias,
        body,
    })
}

struct StaticObjectHeader {
    target: String,
    alias: Option<String>,
    deps: Vec<String>,
    has_with: bool,
}

fn parse_static_object_header(
    mut header: TokenView<'_, '_>,
    line: usize,
    extend: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<StaticObjectHeader> {
    let with_index = contextual_keywords(header, "with").into_iter().next();
    let has_with = with_index.is_some();
    if let Some(with_index) = with_index {
        if header.last_significant().map(|(index, _)| index) != Some(with_index) {
            diagnostics.push(Diagnostic::error(
                line,
                "`with` must end an object declaration header",
            ));
            return None;
        }
        header = trim_layout(view_between(header, header.range().start(), with_index));
    }

    let as_index = contextual_keywords(header, "as").into_iter().next();
    let extends_index = contextual_keywords(header, "extends").into_iter().next();
    if extend && extends_index.is_some() {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declaration does not accept `extends` dependencies",
        ));
        return None;
    }
    if as_index
        .is_some_and(|as_index| extends_index.is_some_and(|extends_index| as_index > extends_index))
    {
        diagnostics.push(Diagnostic::error(
            line,
            "object declaration `as` must precede `extends`",
        ));
        return None;
    }

    let target_end = [as_index, extends_index]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(header.range().end());
    let target_view = trim_layout(view_between(header, header.range().start(), target_end));
    let target = match static_path(target_view) {
        Some(target) if target != "_" => target,
        Some(_) => {
            diagnostics.push(Diagnostic::error(
                line,
                "anonymous object declarations are not supported by the current spike",
            ));
            return None;
        }
        None => {
            diagnostics.push(Diagnostic::error(
                line,
                if extend {
                    "extend declaration requires a path name"
                } else {
                    "object declaration requires a path name"
                },
            ));
            return None;
        }
    };

    let alias = if let Some(as_index) = as_index {
        let end = extends_index.unwrap_or(header.range().end());
        let alias_view = trim_layout(view_between(header, as_index + 1, end));
        match local_name(alias_view) {
            Some(alias) => Some(alias.to_owned()),
            None => {
                diagnostics.push(Diagnostic::error(
                    line,
                    "`as` requires a valid object alias name",
                ));
                return None;
            }
        }
    } else {
        None
    };

    let deps = if let Some(extends_index) = extends_index {
        let deps_view = trim_layout(view_between(
            header,
            extends_index + 1,
            header.range().end(),
        ));
        let dep_views = split_top_level(deps_view, ",")
            .into_iter()
            .map(trim_layout)
            .filter(|view| !is_layout_empty(*view))
            .collect::<Vec<_>>();
        if dep_views.is_empty() {
            diagnostics.push(Diagnostic::error(
                line,
                "object `extends` requires at least one dependency",
            ));
            return None;
        }
        let mut deps = Vec::with_capacity(dep_views.len());
        for dep in dep_views {
            let Some(dep) = static_path(dep) else {
                diagnostics.push(Diagnostic::error(
                    line,
                    "object dependency is not a path name",
                ));
                return None;
            };
            deps.push(dep);
        }
        deps
    } else {
        Vec::new()
    };

    Some(StaticObjectHeader {
        target,
        alias,
        deps,
        has_with,
    })
}

fn static_path(view: TokenView<'_, '_>) -> Option<String> {
    let significant = view
        .top_level()
        .filter(|indexed| !matches!(indexed.token().kind(), TokenKind::LineStart { .. }))
        .collect::<Vec<_>>();
    let TokenKind::Name(_) = significant.first()?.token().kind() else {
        return None;
    };
    for (position, indexed) in significant.iter().enumerate() {
        match (position % 2, indexed.token().kind()) {
            (0, TokenKind::Name(_)) => {
                if position > 0 && indexed.token().leading() != LeadingTrivia::Joint {
                    return None;
                }
            }
            (1, TokenKind::Symbol(".")) if indexed.token().leading() == LeadingTrivia::Joint => {}
            _ => return None,
        }
    }
    (significant.len() % 2 == 1).then(|| view.source_text().unwrap_or("").trim().to_owned())
}

fn split_header_body<'lex, 'source>(
    view: TokenView<'lex, 'source>,
) -> (TokenView<'lex, 'source>, Option<TokenView<'lex, 'source>>) {
    let Some(header_end) = first_top_level_line_start(view) else {
        return (view, None);
    };
    (
        view_between(view, view.range().start(), header_end),
        Some(view_between(view, header_end, view.range().end())),
    )
}

fn declaration_preview(view: TokenView<'_, '_>) -> String {
    view.source_text()
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned()
}
