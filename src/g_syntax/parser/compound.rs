use chumsky::prelude::*;

use super::super::{Diagnostic, ObjectExpr, SyntaxExpr, SyntaxOperator};
use super::declaration::{parse_object_body, take_header_word};
use super::expression::syntax_expr_parser;
use super::layout::{indentation_width, is_indented, local_name};

pub(super) fn syntax_binary_expr(
    operator: SyntaxOperator,
    left: SyntaxExpr,
    right: SyntaxExpr,
) -> SyntaxExpr {
    match operator {
        SyntaxOperator::Builtin(builtin) => match builtin {
            crate::core::Builtin::Append => SyntaxExpr::Append(Box::new(left), Box::new(right)),
            crate::core::Builtin::Add => SyntaxExpr::Add(Box::new(left), Box::new(right)),
            crate::core::Builtin::Subtract => SyntaxExpr::Subtract(Box::new(left), Box::new(right)),
            crate::core::Builtin::Multiply => SyntaxExpr::Multiply(Box::new(left), Box::new(right)),
            crate::core::Builtin::Divide => SyntaxExpr::Divide(Box::new(left), Box::new(right)),
            _ => SyntaxExpr::OperatorApply {
                operator,
                left: Box::new(left),
                right: Box::new(right),
            },
        },
        SyntaxOperator::BoolAnd
        | SyntaxOperator::BoolOr
        | SyntaxOperator::PipeForward
        | SyntaxOperator::PipeBackward
        | SyntaxOperator::ApplicativeForward
        | SyntaxOperator::ApplicativeBackward
        | SyntaxOperator::ComposeForward
        | SyntaxOperator::ComposeBackward
        | SyntaxOperator::EffectBind
        | SyntaxOperator::KleisliCompose
        | SyntaxOperator::EffectThen => SyntaxExpr::OperatorApply {
            operator,
            left: Box::new(left),
            right: Box::new(right),
        },
    }
}

#[cfg(test)]
pub(super) fn parse_expr_result(text: &str) -> Result<SyntaxExpr, String> {
    let mut diagnostics = Vec::new();
    parse_expr_result_with_diagnostics(text, 1, &mut diagnostics)
}

pub(in crate::g_syntax) fn parse_expr_result_with_diagnostics(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<SyntaxExpr, String> {
    let text = text.trim();
    if let Some(result) = parse_let_expr_result(text, line, diagnostics) {
        return result;
    }
    if let Some(result) = parse_where_expr_result(text, line, diagnostics) {
        return result;
    }
    if let Some(result) = parse_object_expr_result(text, line, diagnostics) {
        return result;
    }
    if let Some(result) = parse_with_expr_result(text, line, diagnostics) {
        return result;
    }

    syntax_expr_parser()
        .then_ignore(end())
        .parse(text)
        .into_result()
        .map_err(|errors| {
            errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        })
}

pub(super) fn parse_let_expr_result(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Result<SyntaxExpr, String>> {
    let rest = text.strip_prefix("let")?;
    if !rest
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_whitespace())
    {
        return None;
    }
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Some(Err("let expression requires bindings and a body".to_owned()));
    }

    let in_indices = top_level_keyword_indices(rest, "in");
    let (bindings, body_text) = match in_indices.first().copied() {
        Some(index) if rest[..index].contains('\n') => {
            return Some(Err("multi-line let expression must not use `in`".to_owned()));
        }
        Some(index) => {
            let bindings_text = rest[..index].trim();
            let body_text = rest[index + "in".len()..].trim();
            match parse_local_bindings(bindings_text, line, diagnostics) {
                Ok(bindings) => (bindings, body_text),
                Err(message) => return Some(Err(message)),
            }
        }
        None => match split_multiline_let(rest) {
            Ok((bindings, body)) => match parse_local_binding_texts(bindings, line, diagnostics) {
                Ok(bindings) => (bindings, body.trim()),
                Err(message) => return Some(Err(message)),
            },
            Err(message) => return Some(Err(message)),
        },
    };

    Some(parse_let_expr_from_bindings(
        bindings,
        body_text,
        line,
        diagnostics,
    ))
}

pub(super) fn parse_where_expr_result(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Result<SyntaxExpr, String>> {
    let (body_text, bindings_text) = split_top_level_keyword(text, "where", true)?;
    let body_text = body_text.trim();
    let bindings_text = bindings_text.trim();
    if body_text.is_empty() {
        return Some(Err("where expression requires a body".to_owned()));
    }
    Some(parse_let_expr_from_parts(
        bindings_text,
        body_text,
        line,
        diagnostics,
    ))
}

pub(super) fn parse_let_expr_from_parts(
    bindings_text: &str,
    body_text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<SyntaxExpr, String> {
    if body_text.is_empty() {
        return Err("let expression requires a body".to_owned());
    }
    let bindings = parse_local_bindings(bindings_text, line, diagnostics)?;
    parse_let_expr_from_bindings(bindings, body_text, line, diagnostics)
}

pub(super) fn parse_let_expr_from_bindings(
    bindings: Vec<(String, SyntaxExpr)>,
    body_text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<SyntaxExpr, String> {
    if body_text.is_empty() {
        return Err("let expression requires a body".to_owned());
    }
    if bindings.is_empty() {
        return Err("let expression requires at least one binding".to_owned());
    }
    let body = parse_expr_result_with_diagnostics(body_text, line, diagnostics)?;
    Ok(SyntaxExpr::Let {
        bindings,
        body: Box::new(body),
    })
}

pub(super) fn parse_local_bindings(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<(String, SyntaxExpr)>, String> {
    let binding_texts = if text.contains('\n') {
        parse_multiline_binding_texts(text)?
    } else {
        split_top_level_semicolons(text)
    };

    parse_local_binding_texts(binding_texts, line, diagnostics)
}

pub(super) fn parse_local_binding_texts(
    binding_texts: Vec<&str>,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<(String, SyntaxExpr)>, String> {
    binding_texts
        .into_iter()
        .filter(|binding| !binding.trim().is_empty())
        .map(|binding| parse_local_binding(binding.trim(), line, diagnostics))
        .collect()
}

pub(super) fn parse_local_binding(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(String, SyntaxExpr), String> {
    let Some((name, value)) = split_top_level_binding_equals(text) else {
        return Err(format!("local binding `{text}` must use `=`"));
    };
    let name = name.trim();
    if local_name().parse(name).into_result().is_err() || name.contains(char::is_whitespace) {
        return Err(format!("invalid local binding name `{name}`"));
    }
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("local binding `{name}` requires a value"));
    }
    Ok((
        name.to_owned(),
        parse_expr_result_with_diagnostics(value, line, diagnostics)?,
    ))
}

pub(super) fn split_multiline_let(text: &str) -> Result<(Vec<&str>, &str), String> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() < 2 {
        return Err("multi-line let expression requires a body or `in`".to_owned());
    }

    let first = lines[0].trim();
    if split_top_level_binding_equals(first).is_none() {
        return Err("let expression requires at least one binding".to_owned());
    }

    let binding_indent = "let ".len();
    let mut starts = vec![0usize];
    let mut body_start = None;
    let mut offset = lines[0].len() + 1;

    for line in lines.iter().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            offset += line.len() + 1;
            continue;
        }

        let indent = indentation_width(line);
        if indent == 0 {
            if split_top_level_binding_equals(trimmed).is_some() {
                return Err(
                    "multi-line let binding names must align under the first binding".to_owned(),
                );
            }
            body_start = Some(offset);
            break;
        }

        if split_top_level_binding_equals(trimmed).is_some() {
            if indent != binding_indent {
                return Err(
                    "multi-line let binding names must align under the first binding".to_owned(),
                );
            }
            starts.push(offset + indent);
        }

        offset += line.len() + 1;
    }

    let Some(body_start) = body_start else {
        return Err("multi-line let expression requires a body".to_owned());
    };

    starts.push(body_start);
    let bindings = starts
        .windows(2)
        .map(|pair| text[pair[0]..pair[1].saturating_sub(1)].trim())
        .collect::<Vec<_>>();
    Ok((bindings, &text[body_start..]))
}

pub(super) fn parse_multiline_binding_texts(text: &str) -> Result<Vec<&str>, String> {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !is_indented(line)
            && split_top_level_binding_equals(trimmed).is_some()
        {
            starts.push(offset);
        }
        offset += line.len() + 1;
    }

    if starts.is_empty() {
        return Err("local binding block requires at least one binding".to_owned());
    }
    starts.push(text.len() + 1);

    let mut bindings = Vec::new();
    for pair in starts.windows(2) {
        let start = pair[0];
        let end = pair[1].saturating_sub(1).min(text.len());
        bindings.push(text[start..end].trim());
    }
    Ok(bindings)
}

pub(super) fn split_top_level_keyword<'a>(
    text: &'a str,
    keyword: &str,
    from_end: bool,
) -> Option<(&'a str, &'a str)> {
    let matches = top_level_keyword_indices(text, keyword);
    let index = if from_end {
        matches.into_iter().last()?
    } else {
        matches.into_iter().next()?
    };
    Some((&text[..index], &text[index + keyword.len()..]))
}

pub(super) fn top_level_keyword_indices(text: &str, keyword: &str) -> Vec<usize> {
    let mut indices = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;

    for (index, ch) in text.char_indices() {
        if in_string {
            if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 && keyword_starts_at(text, index, keyword) => indices.push(index),
            _ => {}
        }
    }

    indices
}

pub(super) fn keyword_starts_at(text: &str, index: usize, keyword: &str) -> bool {
    if !text[index..].starts_with(keyword) {
        return false;
    }
    let before = text[..index].chars().next_back();
    let after = text[index + keyword.len()..].chars().next();
    !before.is_some_and(is_name_char) && !after.is_some_and(is_name_char)
}

pub(super) fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

pub(super) fn split_top_level_semicolons(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for index in top_level_char_indices(text, ';') {
        parts.push(&text[start..index]);
        start = index + 1;
    }
    parts.push(&text[start..]);
    parts
}

pub(super) fn split_top_level_binding_equals(text: &str) -> Option<(&str, &str)> {
    top_level_char_indices(text, '=')
        .into_iter()
        .find(|index| {
            let before = text[..*index].chars().next_back();
            let after = text[index + 1..].chars().next();
            !matches!(before, Some(':') | Some('<') | Some('>') | Some('='))
                && !matches!(after, Some('=') | Some('>') | Some('<'))
        })
        .map(|index| (&text[..index], &text[index + 1..]))
}

pub(super) fn top_level_char_indices(text: &str, needle: char) -> Vec<usize> {
    let mut indices = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;

    for (index, ch) in text.char_indices() {
        if in_string {
            if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 && ch == needle => indices.push(index),
            _ => {}
        }
    }

    indices
}

pub(super) fn parse_object_expr_result(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Result<SyntaxExpr, String>> {
    let mut lines = text.lines();
    let header = lines.next()?.trim();
    let body_lines = lines.collect::<Vec<_>>();
    let header = header.strip_prefix("object")?.trim();
    if header.is_empty() {
        return Some(Err(
            "object expression requires a name expression or `_`".to_owned()
        ));
    }

    let (header, has_with) = match header.strip_suffix(" with") {
        Some(header) => (header.trim(), true),
        None => (header, false),
    };
    if !body_lines.is_empty() && !has_with {
        return Some(Err(
            "object expression body requires `with` in the expression header".to_owned(),
        ));
    }

    let ParsedObjectExprHeader {
        name: name_text,
        alias,
        deps: dep_texts,
    } = match parse_object_expr_header(header) {
        Ok(parsed) => parsed,
        Err(message) => return Some(Err(message)),
    };
    let name = match name_text {
        Some(name_text) => match parse_expr_result_with_diagnostics(name_text, line, diagnostics) {
            Ok(name) => Some(Box::new(name)),
            Err(message) => return Some(Err(message)),
        },
        None => None,
    };
    let deps = match dep_texts
        .iter()
        .map(|dep| parse_expr_result_with_diagnostics(dep, line, diagnostics))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(deps) => deps,
        Err(message) => return Some(Err(message)),
    };

    let body = parse_object_body(&body_lines, line + 1, diagnostics);

    Some(Ok(SyntaxExpr::Object(ObjectExpr {
        name,
        alias,
        deps,
        body,
    })))
}

struct ParsedObjectExprHeader<'a> {
    name: Option<&'a str>,
    alias: Option<String>,
    deps: Vec<&'a str>,
}

fn parse_object_expr_header(header: &str) -> Result<ParsedObjectExprHeader<'_>, String> {
    let (name_text, rest) = split_before_object_expr_keyword(header);
    let name_text = name_text.trim();
    if name_text.is_empty() {
        return Err("object expression requires a name expression or `_`".to_owned());
    }
    let name = if name_text == "_" {
        None
    } else {
        Some(name_text)
    };

    let (alias, rest) = parse_optional_object_expr_alias(rest)?;
    let deps = if rest.is_empty() {
        Vec::new()
    } else {
        let Some(deps) = rest.strip_prefix("extends").map(str::trim) else {
            return Err(
                "object expressions currently support only `as ...`, `extends ...`, and `with` after the name"
                    .to_owned(),
            );
        };
        if deps.is_empty() {
            return Err("object expression `extends` requires at least one dependency".to_owned());
        }
        deps.split(',')
            .map(str::trim)
            .filter(|dep| !dep.is_empty())
            .collect::<Vec<_>>()
    };

    Ok(ParsedObjectExprHeader { name, alias, deps })
}

pub(super) fn split_before_object_expr_keyword(header: &str) -> (&str, &str) {
    let as_index = header.find(" as ");
    let extends_index = header.find(" extends ");
    let split = match (as_index, extends_index) {
        (Some(left), Some(right)) => left.min(right),
        (Some(index), None) | (None, Some(index)) => index,
        (None, None) => return (header, ""),
    };
    (&header[..split], header[split..].trim_start())
}

pub(super) fn parse_optional_object_expr_alias(
    rest: &str,
) -> Result<(Option<String>, &str), String> {
    let Some(after_as) = rest.strip_prefix("as").map(str::trim_start) else {
        return Ok((None, rest));
    };
    let Some((alias, rest)) = take_header_word(after_as) else {
        return Err("`as` requires an object alias name".to_owned());
    };
    if !local_name().parse(alias).into_result().is_ok() {
        return Err(format!("object alias `{alias}` is not a valid local name"));
    }
    Ok((Some(alias.to_owned()), rest.trim()))
}

pub(super) fn parse_with_expr_result(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Result<SyntaxExpr, String>> {
    let mut lines = text.lines();
    let header = lines.next()?.trim();
    let body_lines = lines.collect::<Vec<_>>();
    if body_lines.is_empty() {
        return None;
    }

    let base_and_alias = header.strip_suffix(" with")?.trim();
    let (base_text, alias) = parse_optional_with_alias(base_and_alias);
    if base_text.is_empty() {
        return Some(Err("with expression requires a base expression".to_owned()));
    }
    let base = match parse_expr_result_with_diagnostics(base_text, line, diagnostics) {
        Ok(base) => base,
        Err(message) => return Some(Err(message)),
    };

    let body = parse_object_body(&body_lines, line + 1, diagnostics);

    Some(Ok(SyntaxExpr::With {
        base: Box::new(base),
        alias,
        body,
    }))
}

pub(super) fn parse_optional_with_alias(text: &str) -> (&str, Option<String>) {
    let Some((base, alias)) = text.rsplit_once(" as ") else {
        return (text, None);
    };
    if alias == "_" {
        return (base.trim(), None);
    }
    if local_name().parse(alias).into_result().is_ok() {
        (base.trim(), Some(alias.to_owned()))
    } else {
        (text, None)
    }
}
