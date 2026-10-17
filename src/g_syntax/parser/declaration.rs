//! Source and nested declaration grammar.
//!
//! A declaration is always parsed from one lexer-owned range. Nested object
//! bodies reuse the same parser over layout-selected subranges, so expressions,
//! delimiter groups, multiline text, and indentation have one interpretation.

use super::super::keywords::{g0_keyword, reserved_keyword_message};
use super::super::{
    DeclarationKind, DefinitionDecl, DefinitionKind, Diagnostic, ObjectBodyDefinition,
    ObjectBodyDefinitionKind, ObjectDecl, ObjectExtendDecl, ObjectRealization, Severity,
    SyntaxExpr, SyntaxKeyExpr, warn_unused_body_lets, warn_unused_locals, warn_unused_with_alias,
};
use super::expression_context::{ExpressionContext, validate_expression_floor};
use super::input::{ParseSession, TokenView};
use super::layout::LayoutView;
use super::lexical::{Delimiter, LeadingTrivia, TokenKind};
use super::structural::{
    braced_contents, contextual_keywords, is_layout_empty, local_name, object_alias_name,
    parse_body, parse_expression_in_context, parse_object_parents, single_reserved_keyword,
    split_braced_members, split_compound_header_body, split_top_level, trim_layout, view_between,
};

mod simple;

use simple::parse_name_list_member;
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
        TokenKind::Name("let") => parse_let_member(view, line, context, diagnostics)
            .map_or(DeclarationKind::Unknown, DeclarationKind::Let),
        // `alias = …` defines a reserved name, which the definition reports.
        TokenKind::Name("alias")
            if !view.top_level().any(|indexed| {
                matches!(
                    indexed.token().kind(),
                    TokenKind::Symbol("=" | ":=" | "::=")
                )
            }) =>
        {
            diagnostics.push(Diagnostic::error(
                line,
                "`alias` may only be the first member of a `with` body",
            ));
            DeclarationKind::Unknown
        }
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

/// A `with` body: the alias its first member declares, if any, and its
/// members.
#[derive(Default)]
pub(in crate::g_syntax::parser) struct ObjectBody {
    pub(in crate::g_syntax::parser) alias: Option<String>,
    pub(in crate::g_syntax::parser) members: Vec<ObjectBodyDefinition>,
}

pub(in crate::g_syntax::parser) fn parse_object_body(
    view: TokenView<'_, '_>,
    parent_context: ExpressionContext,
    diagnostics: &mut Vec<Diagnostic>,
) -> ObjectBody {
    if is_layout_empty(view) {
        return ObjectBody::default();
    }
    let Some(statement_views) = block_members(view, "`with` body", diagnostics) else {
        return ObjectBody::default();
    };

    let mut body = Vec::with_capacity(statement_views.len());
    let mut alias = None;
    for (position, (line, statement_view)) in statement_views.into_iter().enumerate() {
        let statement_view = trim_layout(statement_view);
        let head = statement_view
            .first_significant()
            .map(|(_, token)| token.kind());
        // `alias Name` may only be a body's first member.
        if let Some(TokenKind::Name("alias")) = head {
            if position > 0 {
                diagnostics.push(Diagnostic::error(
                    line,
                    "`alias` may only be the first member of its body",
                ));
                continue;
            }
            match parse_alias_member(statement_view) {
                Ok(name) => alias = Some(name),
                Err(message) => diagnostics.push(Diagnostic::error(line, message)),
            }
            continue;
        }
        if let Some(TokenKind::Name("let")) = head {
            let context = parent_context.child_owner(statement_view);
            if let Some(bindings) = parse_let_member(statement_view, line, context, diagnostics) {
                body.push(ObjectBodyDefinition {
                    line,
                    kind: ObjectBodyDefinitionKind::Let(bindings),
                });
            }
            continue;
        }
        if let Some(TokenKind::Name(head @ ("declare" | "assume"))) = head {
            let declare = *head == "declare";
            let keyword = if declare { "declare" } else { "assume" };
            let mut session = ParseSession::new(statement_view.source());
            let names = parse_name_list_member(statement_view, line, keyword, &mut session);
            diagnostics.extend(session.into_diagnostics());
            if let Some(names) = names {
                let kind = if declare {
                    ObjectBodyDefinitionKind::Declare(names)
                } else {
                    ObjectBodyDefinitionKind::Assume(names)
                };
                body.push(ObjectBodyDefinition { line, kind });
            }
            continue;
        }
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
    warn_unused_body_lets(&body, diagnostics);
    ObjectBody {
        alias,
        members: body,
    }
}

/// The members of a braced or layout block, each with its line, or `None`
/// once a malformed block is reported.
fn block_members<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    label: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<(usize, TokenView<'lex, 'source>)>> {
    match split_braced_members(view, label) {
        Some(Ok(members)) => Some(
            members
                .into_iter()
                .map(|member| {
                    let line = member
                        .first_significant()
                        .and_then(|(_, token)| member.line_at_span(token.span()))
                        .unwrap_or(1);
                    (line, member)
                })
                .collect(),
        ),
        Some(Err(mut errors)) => {
            diagnostics.append(&mut errors);
            None
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
                return None;
            }
            Some(
                block
                    .into_statements()
                    .into_iter()
                    .filter_map(|statement| {
                        view.subview(statement.tokens())
                            .map(|statement_view| (statement.line(), statement_view))
                    })
                    .collect(),
            )
        }
    }
}

/// A `let` group, a body's member or a file's declaration: one binding
/// after `let`, a hanging layout block of them whose first sets the column,
/// or a braced block.
fn parse_let_member(
    view: TokenView<'_, '_>,
    line: usize,
    context: ExpressionContext,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<(String, SyntaxExpr)>> {
    let (let_index, _) = view
        .first_significant()
        .expect("a `let` member begins with its keyword");
    let group = trim_layout(view_between(view, let_index + 1, view.range().end()));
    let members = if is_layout_empty(group) {
        Vec::new()
    } else {
        block_members(group, "`let` bindings", diagnostics)?
    };
    if members.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "a `let` member requires at least one binding",
        ));
        return None;
    }
    let mut bindings = Vec::with_capacity(members.len());
    let mut failed = false;
    for (line, member) in members {
        let member = trim_layout(member);
        match parse_let_binding(member, line, context.child_owner(member), diagnostics) {
            Some(binding) => bindings.push(binding),
            None => failed = true,
        }
    }
    (!failed).then_some(bindings)
}

/// One binding of a `let` member, `name params = value`, read as a `let`
/// expression's: parameters are lambda sugar.
fn parse_let_binding(
    view: TokenView<'_, '_>,
    line: usize,
    context: ExpressionContext,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(String, SyntaxExpr)> {
    let text = |view: TokenView<'_, '_>| view.source_text().unwrap_or("").trim().to_owned();
    let mut fail = |message: String| {
        diagnostics.push(Diagnostic::error(line, message));
        None
    };
    let operator = view.top_level().find(|indexed| {
        matches!(
            indexed.token().kind(),
            TokenKind::Symbol("=" | ":=" | "::=")
        )
    });
    let Some(equal) =
        operator.filter(|indexed| matches!(indexed.token().kind(), TokenKind::Symbol("=")))
    else {
        return fail(format!("local binding `{}` must use `=`", text(view)));
    };
    let head = trim_layout(view_between(view, view.range().start(), equal.index()));
    let value = trim_layout(view_between(view, equal.index() + 1, view.range().end()));
    let one = |index: usize| view_between(view, index, index + 1);
    let mut names = head
        .top_level()
        .filter(|indexed| !matches!(indexed.token().kind(), TokenKind::LineStart { .. }));
    let Some(first) = names.next() else {
        return fail(format!("invalid local binding name `{}`", text(head)));
    };
    let Some(name) = local_name(one(first.index())) else {
        return fail(match single_reserved_keyword(one(first.index())) {
            Some(keyword) => reserved_keyword_message(keyword),
            None => format!("invalid local binding name `{}`", text(head)),
        });
    };
    let mut parameters = Vec::new();
    for indexed in names {
        let Some(parameter) = local_name(one(indexed.index())) else {
            return fail(match single_reserved_keyword(one(indexed.index())) {
                Some(keyword) => reserved_keyword_message(keyword),
                None => format!("local binding `{name}` parameters must be local names"),
            });
        };
        if indexed.token().leading() == LeadingTrivia::Joint {
            return fail(format!(
                "local binding `{name}` parameters must be separated from its name"
            ));
        }
        parameters.push(parameter.to_owned());
    }
    if is_layout_empty(value) {
        return fail(format!("local binding `{name}` requires a value"));
    }
    let value = match parse_body(value, context) {
        Ok(value) => value,
        Err(errors) => {
            diagnostics.extend(errors);
            return None;
        }
    };
    let value = if parameters.is_empty() {
        value
    } else {
        SyntaxExpr::Lambda(parameters, Box::new(value))
    };
    warn_unused_locals(&value, line, diagnostics);
    Some((name.to_owned(), value))
}

pub(in crate::g_syntax::parser) fn parse_nonempty_object_body(
    view: TokenView<'_, '_>,
    line: usize,
    empty_message: &str,
    parent_context: ExpressionContext,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectBody> {
    let diagnostic_start = diagnostics.len();
    let body = parse_object_body(view, parent_context, diagnostics);
    if body.members.is_empty() && body.alias.is_none() {
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

/// The name an `alias Name` member gives its body: a local name, `self`,
/// or `_` for none.
fn parse_alias_member(view: TokenView<'_, '_>) -> Result<String, String> {
    let (alias_index, _) = view
        .first_significant()
        .expect("an `alias` member begins with its keyword");
    let name_view = trim_layout(view_between(view, alias_index + 1, view.range().end()));
    if is_layout_empty(name_view) {
        return Err("`alias` requires a name, `self`, or `_`".to_owned());
    }
    match object_alias_name(name_view) {
        Some(name) => Ok(name.to_owned()),
        None => Err(match single_reserved_keyword(name_view) {
            Some(keyword) => reserved_keyword_message(keyword),
            None => "`alias` requires a name, `self`, or `_`".to_owned(),
        }),
    }
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
    let parsed_body = match parse_body(body_view, context) {
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
                    parts.push(SyntaxKeyExpr::from_item(expr));
                }
            }
            TokenKind::Open {
                group,
                delimiter: Delimiter::Parenthesis,
            } => {
                let contents = view
                    .group_contents(*group)
                    .ok_or_else(|| Diagnostic::error(line, "invalid computed path"))?;
                let expr = parse_body(contents, context.complete())
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
        ObjectBody::default()
    };
    if let Some(alias) = &body.alias {
        warn_unused_with_alias(alias, &body.members, line, diagnostics);
    }
    Some(ObjectDecl {
        realization,
        target: parsed.target,
        alias: body.alias,
        deps: parsed.deps,
        body: body.members,
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
            "extend declarations currently require `extend name with`",
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
    if let Some(alias) = &body.alias {
        warn_unused_with_alias(alias, &body.members, line, diagnostics);
    }
    Some(ObjectExtendDecl {
        realization,
        target: parsed.target,
        alias: body.alias,
        body: body.members,
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
    deps: Vec<SyntaxExpr>,
    has_with: bool,
}

/// The error for an `as Name` alias in a header, which moved into the body.
pub(in crate::g_syntax::parser) const MOVED_ALIAS: &str =
    "an alias is its body's first member: write `with { alias Name; … }`, not `as Name`";

fn parse_static_object_header(
    mut header: TokenView<'_, '_>,
    line: usize,
    extend: bool,
    context: ExpressionContext,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<StaticObjectHeader> {
    let with_index = contextual_keywords(header, "with").next();
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

    let as_index = contextual_keywords(header, "as").next();
    let extends_index = contextual_keywords(header, "extends").next();
    if extend && extends_index.is_some() {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declaration does not accept `extends` dependencies",
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

    if as_index.is_some() {
        diagnostics.push(Diagnostic::error(line, MOVED_ALIAS));
        return None;
    }

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
