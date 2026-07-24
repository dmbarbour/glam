//! Source and nested declaration grammar.
//!
//! A declaration is always parsed from one lexer-owned range. Nested object
//! bodies reuse the same parser over layout-selected subranges, so expressions,
//! delimiter groups, multiline text, and indentation have one interpretation.

use super::super::keywords::{g0_keyword, reserved_keyword_message};
use super::super::{
    DeclarationKind, DefinitionDecl, DefinitionKind, Diagnostic, ObjectBodyDefinition,
    ObjectBodyDefinitionKind, ObjectDecl, ObjectExtendDecl, ObjectRealization, Severity,
    SyntaxExpr, SyntaxKeyExpr, warn_unused_locals, warn_unused_with_alias,
};
use super::expression_context::{ExpressionContext, validate_expression_floor};
use super::input::TokenView;
use super::layout::LayoutView;
use super::lexical::{Delimiter, LeadingTrivia, TokenKind};
use super::structural::{
    braced_contents, contextual_keywords, is_layout_empty, local_name, object_alias_name,
    parse_expression_in_context, parse_object_parents, single_reserved_keyword,
    split_braced_members, split_compound_header_body, split_top_level, trim_layout, view_between,
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
    let context = ExpressionContext::for_owner(view);
    parse_declaration_in_context(view, line, context, diagnostics)
}

fn parse_declaration_in_context(
    view: TokenView<'_, '_>,
    line: usize,
    context: ExpressionContext,
    diagnostics: &mut Vec<Diagnostic>,
) -> DeclarationKind {
    let (context, mut floor_diagnostics) = validate_expression_floor(view, context);
    diagnostics.append(&mut floor_diagnostics);
    let Some((_, head)) = view.first_significant() else {
        return DeclarationKind::Unknown;
    };
    match head.kind() {
        TokenKind::Name("object") => parse_object_declaration(view, line, context, diagnostics)
            .map_or(DeclarationKind::Unknown, DeclarationKind::Object),
        TokenKind::Name("abstract") if is_abstract_object_declaration(view) => {
            parse_object_declaration(view, line, context, diagnostics)
                .map_or(DeclarationKind::Unknown, DeclarationKind::Object)
        }
        TokenKind::Name("extend") => parse_extend_declaration(view, line, context, diagnostics)
            .map_or(DeclarationKind::Unknown, DeclarationKind::Extend),
        _ => parse_definition(view, line, context, diagnostics)
            .map_or(DeclarationKind::Unknown, DeclarationKind::Definition),
    }
}

pub(super) fn is_abstract_object_declaration(view: TokenView<'_, '_>) -> bool {
    matches!(
        object_declaration_head(view),
        Some((ObjectRealization::Abstract, _))
    )
}

pub(in crate::g_syntax::parser) fn parse_object_body(
    view: TokenView<'_, '_>,
    parent_context: ExpressionContext,
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
            let block = LayoutView::new(view).block();
            if let Some(boundary) = block.boundary() {
                diagnostics.push(Diagnostic::error(
                    boundary.line(),
                    format!(
                        "layout line is indented {} spaces; expected at least {}",
                        boundary.indentation(),
                        block.anchor()
                    ),
                ));
                return Vec::new();
            }
            block
                .into_statements()
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
        let statement_view = trim_layout(statement_view);
        let context = parent_context.child_owner(statement_view);
        let kind = match parse_declaration_in_context(statement_view, line, context, diagnostics) {
            DeclarationKind::Object(object) => Some(ObjectBodyDefinitionKind::Object(object)),
            DeclarationKind::Extend(extend) => Some(ObjectBodyDefinitionKind::Extend(extend)),
            DeclarationKind::Definition(definition) => {
                Some(ObjectBodyDefinitionKind::Definition(definition))
            }
            _ => None,
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
    parent_context: ExpressionContext,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<ObjectBodyDefinition>> {
    let diagnostic_start = diagnostics.len();
    let body = parse_object_body(view, parent_context, diagnostics);
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
    context: ExpressionContext,
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

    let (target, parameters) = match parse_definition_left(left, line, context) {
        Ok(parts) => parts,
        Err(diagnostic) => {
            diagnostics.push(diagnostic);
            return None;
        }
    };
    let parsed_body = match parse_expression_in_context(body_view, context) {
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
    context: ExpressionContext,
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
    let target = parse_definition_target(target_view, line, context)?;

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
    context: ExpressionContext,
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
                    let expr = parse_expression_in_context(item, context.complete())
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
                let expr = parse_expression_in_context(contents, context.complete())
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
    context: ExpressionContext,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectDecl> {
    let (header, body) = split_compound_header_body(view);
    let (realization, head) = object_declaration_head(header)?;
    let header = trim_layout(view_between(header, head + 1, header.range().end()));
    let parsed = parse_static_object_header(header, line, false, context, diagnostics)?;
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
            parse_object_body(body, context, diagnostics)
        } else {
            parse_nonempty_object_body(
                body,
                line,
                "object `with` body cannot be empty; use `with {}` for an explicit empty body",
                context,
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
        realization,
        target: parsed.target,
        alias: parsed.alias,
        deps: parsed.deps,
        body,
    })
}

fn parse_extend_declaration(
    view: TokenView<'_, '_>,
    line: usize,
    context: ExpressionContext,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectExtendDecl> {
    let (header, body) = split_compound_header_body(view);
    let head = header.first_significant()?.0;
    let mut header = trim_layout(view_between(header, head + 1, header.range().end()));
    let realization = if header
        .first_significant()
        .is_some_and(|(_, token)| matches!(token.kind(), TokenKind::Name("abstract")))
    {
        let abstract_index = header
            .first_significant()
            .map(|(index, _)| index)
            .expect("nonempty extend header has a first token");
        header = trim_layout(view_between(
            header,
            abstract_index + 1,
            header.range().end(),
        ));
        ObjectRealization::Abstract
    } else {
        ObjectRealization::Instance
    };
    let parsed = parse_static_object_header(header, line, true, context, diagnostics)?;
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
        parse_object_body(body_view, context, diagnostics)
    } else {
        parse_nonempty_object_body(
            body_view,
            line,
            "extend declaration body cannot be empty; use `with {}` for an explicit empty body",
            context,
            diagnostics,
        )?
    };
    if let Some(alias) = &parsed.alias {
        warn_unused_with_alias(alias, &body, line, diagnostics);
    }
    Some(ObjectExtendDecl {
        realization,
        target: parsed.target,
        alias: parsed.alias,
        body,
    })
}

fn object_declaration_head(view: TokenView<'_, '_>) -> Option<(ObjectRealization, usize)> {
    let (head_index, head) = view.first_significant()?;
    match head.kind() {
        TokenKind::Name("object") => Some((ObjectRealization::Instance, head_index)),
        TokenKind::Name("abstract") => {
            let mut significant = view.top_level().filter(|indexed| {
                indexed.index() > head_index
                    && !matches!(indexed.token().kind(), TokenKind::LineStart { .. })
            });
            let object = significant.next()?;
            matches!(object.token().kind(), TokenKind::Name("object"))
                .then_some((ObjectRealization::Abstract, object.index()))
        }
        _ => None,
    }
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
    context: ExpressionContext,
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
        match parse_object_parents(deps_view, line, context.complete()) {
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
