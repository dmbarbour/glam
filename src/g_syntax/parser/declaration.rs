//! Source and nested declaration grammar.
//!
//! A declaration is always parsed from one lexer-owned range. Nested object
//! bodies reuse the same parser over layout-selected subranges, so expressions,
//! delimiter groups, multiline text, and indentation have one interpretation.

use super::super::keywords::{g0_keyword, reserved_keyword_message};
use super::super::{
    DeclarationKind, DefinitionDecl, DefinitionKind, Diagnostic, ObjectBodyDefinition,
    ObjectBodyDefinitionKind, ObjectDecl, ObjectExtendDecl, Severity, SyntaxExpr, SyntaxKeyExpr,
    warn_unused_locals, warn_unused_with_alias,
};
use super::input::{TokenRange, TokenView};
use super::layout::{LayoutBase, LayoutView};
use super::lexical::{Delimiter, LeadingTrivia, TokenKind};
use super::structural::{
    braced_contents, contextual_keywords, is_layout_empty, local_name, object_alias_name,
    parse_expression, parse_object_parents, single_reserved_keyword, split_braced_members,
    split_compound_header_body, split_top_level, token_is_name, trim_layout, view_between,
};

mod simple;

pub(super) use simple::{SimpleDeclaration, parse_simple_declaration};

pub(super) fn validate_language_position(
    declarations: &[super::super::Declaration],
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

pub(in crate::g_syntax::parser) fn validate_declaration_continuations(
    view: TokenView<'_, '_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some((first_index, first_token)) = view.first_significant() else {
        return;
    };
    let first_line = view.line_at_span(first_token.span()).unwrap_or(1);
    let declaration_indentation = view.line_indentation_at(first_index).unwrap_or(0);
    let line_starts = view
        .tokens()
        .iter()
        .enumerate()
        .filter_map(|(relative, token)| match token.kind() {
            TokenKind::LineStart { indentation } => Some((
                view.range().start() + relative,
                *indentation,
                view.line_at_span(token.span()).unwrap_or(first_line),
            )),
            _ => None,
        })
        .filter(|(_, _, line)| *line > first_line)
        .collect::<Vec<_>>();

    for (position, (line_start, indentation, line)) in line_starts.iter().copied().enumerate() {
        if indentation > declaration_indentation {
            continue;
        }
        let line_end = line_starts
            .get(position + 1)
            .map_or(view.range().end(), |(next, _, _)| *next);
        let line_view = view
            .subview(
                TokenRange::new(line_start + 1, line_end)
                    .expect("a physical source line has ordered token boundaries"),
            )
            .expect("a declaration line remains within its declaration");
        let mut line_tokens = line_view
            .tokens()
            .iter()
            .filter(|token| !matches!(token.kind(), TokenKind::LineStart { .. }));
        let Some(first) = line_tokens.next() else {
            continue;
        };
        let starts_with_closer = matches!(first.kind(), TokenKind::Close { .. });
        let closer_only = starts_with_closer
            && line_tokens.all(|token| matches!(token.kind(), TokenKind::Close { .. }));

        if indentation == declaration_indentation && closer_only {
            if position + 1 < line_starts.len() {
                push_continuation_diagnostic(
                    diagnostics,
                    line,
                    "expression continues after a boundary-aligned closing delimiter; indent the closing delimiter to continue the expression",
                );
            }
        } else if starts_with_closer {
            push_continuation_diagnostic(
                diagnostics,
                line,
                "expression continues after a boundary-aligned closing delimiter; indent this line or end the declaration after the delimiter",
            );
        } else {
            push_continuation_diagnostic(
                diagnostics,
                line,
                format!(
                    "declaration continuation is indented {indentation} spaces; expected at least {}",
                    declaration_indentation + 1
                ),
            );
        }
    }
}

fn push_continuation_diagnostic(
    diagnostics: &mut Vec<Diagnostic>,
    line: usize,
    message: impl Into<String>,
) {
    let already_reported = diagnostics.iter().any(|diagnostic| {
        diagnostic.line == line
            && (diagnostic
                .message
                .contains("boundary-aligned closing delimiter")
                || diagnostic
                    .message
                    .starts_with("declaration continuation is indented"))
    });
    if !already_reported {
        diagnostics.push(Diagnostic::error(line, message));
    }
}

pub(in crate::g_syntax::parser) fn parse_object_body(
    view: TokenView<'_, '_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ObjectBodyDefinition> {
    if is_layout_empty(view) {
        return Vec::new();
    }
    let statement_views = match split_braced_members(view, "`with` body") {
        Some(Ok(members)) => members
            .into_iter()
            .map(|member| {
                let line = member
                    .first_significant()
                    .and_then(|(_, token)| member.line_at_span(token.span()))
                    .unwrap_or(1);
                (line, member)
            })
            .collect::<Vec<_>>(),
        Some(Err(mut errors)) => {
            diagnostics.append(&mut errors);
            return Vec::new();
        }
        None => {
            let statements = match LayoutView::new(view).statements(LayoutBase::FirstLine) {
                Ok(statements) => statements,
                Err(error) => {
                    diagnostics.push(Diagnostic::error(error.line(), error.message()));
                    return Vec::new();
                }
            };
            statements
                .into_iter()
                .filter_map(|statement| {
                    view.subview(statement.tokens())
                        .map(|statement_view| (statement.line(), statement_view))
                })
                .collect()
        }
    };

    let mut body = Vec::with_capacity(statement_views.len());
    for (line, statement_view) in statement_views {
        validate_declaration_continuations(statement_view, diagnostics);
        let statement_view = trim_layout(statement_view);
        let kind = match statement_view.first_significant() {
            Some((_, token)) if token_is_name(token, "object") => {
                parse_object_declaration(statement_view, line, diagnostics)
                    .map(ObjectBodyDefinitionKind::Object)
            }
            Some((_, token)) if token_is_name(token, "extend") => {
                parse_extend_declaration(statement_view, line, diagnostics)
                    .map(ObjectBodyDefinitionKind::Extend)
            }
            _ => parse_definition(statement_view, line, diagnostics)
                .map(ObjectBodyDefinitionKind::Definition),
        };
        if let Some(kind) = kind {
            body.push(ObjectBodyDefinition { line, kind });
        }
    }
    body
}

pub(in crate::g_syntax::parser) fn parse_nonempty_object_body(
    view: TokenView<'_, '_>,
    line: usize,
    empty_message: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<ObjectBodyDefinition>> {
    let diagnostic_start = diagnostics.len();
    let body = parse_object_body(view, diagnostics);
    if body.is_empty() {
        if !diagnostics[diagnostic_start..]
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            diagnostics.push(Diagnostic::error(line, empty_message));
        }
        return None;
    }
    Some(body)
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
    let parsed_body = match parse_expression(body_view) {
        Ok(expr) => expr,
        Err(errors) => {
            diagnostics.extend(errors);
            return Some(DefinitionDecl {
                target,
                parameters,
                kind,
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
        expr: Some(expr),
    })
}

fn parse_definition_left(
    view: TokenView<'_, '_>,
    line: usize,
) -> Result<(Vec<SyntaxKeyExpr>, Vec<String>), Diagnostic> {
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
    let target = parse_definition_target(target_view, line)?;

    let mut parameters = Vec::new();
    for indexed in significant.into_iter().skip(position) {
        if indexed.index() < target_end {
            continue;
        }
        let parameter_view = view_between(view, indexed.index(), indexed.index() + 1);
        let Some(parameter) = local_name(parameter_view) else {
            if let Some(keyword) = single_reserved_keyword(parameter_view) {
                return Err(Diagnostic::error(line, reserved_keyword_message(keyword)));
            }
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
        if let Some(keyword) = g0_keyword(name) {
            return Err(Diagnostic::error(line, reserved_keyword_message(keyword)));
        }
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
            TokenKind::Name(name) => {
                if parts.is_empty()
                    && let Some(keyword) = g0_keyword(name)
                {
                    return Err(Diagnostic::error(line, reserved_keyword_message(keyword)));
                }
                parts.push(SyntaxKeyExpr::Atom((*name).to_owned()));
            }
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
    let (header, body) = split_compound_header_body(view);
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
    if parsed.alias.is_some() && !parsed.has_with {
        diagnostics.push(Diagnostic::error(
            line,
            "object declaration `as` requires a `with` body",
        ));
        return None;
    }
    for parent in &parsed.deps {
        warn_unused_locals(parent, line, diagnostics);
    }
    let body = if parsed.has_with {
        let Some(body) = body else {
            diagnostics.push(Diagnostic::error(
                line,
                "object `with` requires a body; use `with {}` for an explicit empty body",
            ));
            return None;
        };
        if braced_contents(body).is_some() {
            parse_object_body(body, diagnostics)
        } else {
            parse_nonempty_object_body(
                body,
                line,
                "object `with` body cannot be empty; use `with {}` for an explicit empty body",
                diagnostics,
            )?
        }
    } else {
        Vec::new()
    };
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
    let (header, body) = split_compound_header_body(view);
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
    let body = if braced_contents(body_view).is_some() {
        parse_object_body(body_view, diagnostics)
    } else {
        parse_nonempty_object_body(
            body_view,
            line,
            "extend declaration body cannot be empty; use `with {}` for an explicit empty body",
            diagnostics,
        )?
    };
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
    deps: Vec<SyntaxExpr>,
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
    if let Some(keyword) =
        target_view
            .top_level()
            .next()
            .and_then(|indexed| match indexed.token().kind() {
                TokenKind::Name(name) => g0_keyword(name),
                _ => None,
            })
    {
        diagnostics.push(Diagnostic::error(line, reserved_keyword_message(keyword)));
        return None;
    }
    let target = match static_path(target_view) {
        Some(target) if target != "_" => target,
        Some(_) => {
            diagnostics.push(Diagnostic::error(
                line,
                "object declarations require a named target; use an object expression for an anonymous object",
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
        match object_alias_name(alias_view) {
            Some(alias) => Some(alias.to_owned()),
            None => {
                if let Some(keyword) = single_reserved_keyword(alias_view) {
                    diagnostics.push(Diagnostic::error(line, reserved_keyword_message(keyword)));
                    return None;
                }
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
        match parse_object_parents(deps_view, line) {
            Ok(deps) => deps,
            Err(mut errors) => {
                diagnostics.append(&mut errors);
                return None;
            }
        }
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
