//! G-syntax expression parsing, including Chumsky combinators and the small
//! layout-aware scanners used by compound expression forms.

use chumsky::prelude::*;

use crate::number::Number;

use super::{
    Diagnostic, ObjectExpr, PathSuffix, SyntaxExpr, SyntaxKeyExpr, SyntaxOperator,
    flatten_path_suffixes, glam_name, indentation_width, is_comparison_operator, is_indented,
    local_name, parse_object_body, quoted_text, take_header_word, whitespace1,
};

fn syntax_binary_expr(operator: SyntaxOperator, left: SyntaxExpr, right: SyntaxExpr) -> SyntaxExpr {
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
fn parse_expr_result(text: &str) -> Result<SyntaxExpr, String> {
    let mut diagnostics = Vec::new();
    parse_expr_result_with_diagnostics(text, 1, &mut diagnostics)
}

pub(super) fn parse_expr_result_with_diagnostics(
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

fn parse_let_expr_result(
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

fn parse_where_expr_result(
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

fn parse_let_expr_from_parts(
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

fn parse_let_expr_from_bindings(
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

fn parse_local_bindings(
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

fn parse_local_binding_texts(
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

fn parse_local_binding(
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

fn split_multiline_let(text: &str) -> Result<(Vec<&str>, &str), String> {
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

fn parse_multiline_binding_texts(text: &str) -> Result<Vec<&str>, String> {
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

fn split_top_level_keyword<'a>(
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

fn top_level_keyword_indices(text: &str, keyword: &str) -> Vec<usize> {
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

fn keyword_starts_at(text: &str, index: usize, keyword: &str) -> bool {
    if !text[index..].starts_with(keyword) {
        return false;
    }
    let before = text[..index].chars().next_back();
    let after = text[index + keyword.len()..].chars().next();
    !before.is_some_and(is_name_char) && !after.is_some_and(is_name_char)
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn split_top_level_semicolons(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for index in top_level_char_indices(text, ';') {
        parts.push(&text[start..index]);
        start = index + 1;
    }
    parts.push(&text[start..]);
    parts
}

fn split_top_level_binding_equals(text: &str) -> Option<(&str, &str)> {
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

fn top_level_char_indices(text: &str, needle: char) -> Vec<usize> {
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

fn parse_object_expr_result(
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

fn split_before_object_expr_keyword(header: &str) -> (&str, &str) {
    let as_index = header.find(" as ");
    let extends_index = header.find(" extends ");
    let split = match (as_index, extends_index) {
        (Some(left), Some(right)) => left.min(right),
        (Some(index), None) | (None, Some(index)) => index,
        (None, None) => return (header, ""),
    };
    (&header[..split], header[split..].trim_start())
}

fn parse_optional_object_expr_alias(rest: &str) -> Result<(Option<String>, &str), String> {
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

fn parse_with_expr_result(
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

fn parse_optional_with_alias(text: &str) -> (&str, Option<String>) {
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

#[cfg(test)]
pub(super) fn parse_expr(text: &str) -> Option<SyntaxExpr> {
    parse_expr_result(text).ok()
}

pub(super) fn syntax_expr_parser<'src>()
-> impl Parser<'src, &'src str, SyntaxExpr, extra::Err<Rich<'src, char>>> {
    #[allow(dead_code)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Associativity {
        Left,
        Right,
        None,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum OperatorRelation {
        Stronger,
        Weaker,
        Same(Associativity),
        Unrelated,
    }

    enum PartialExpr {
        Expr(SyntaxExpr),
        ComparisonChain {
            first: Box<SyntaxExpr>,
            rest: Vec<(SyntaxOperator, SyntaxExpr)>,
        },
    }

    impl PartialExpr {
        fn into_expr(self) -> SyntaxExpr {
            match self {
                PartialExpr::Expr(expr) => expr,
                PartialExpr::ComparisonChain { first, mut rest } if rest.len() == 1 => {
                    let (operator, right) = rest
                        .pop()
                        .expect("single-item comparison chain should contain one comparison");
                    syntax_binary_expr(operator, *first, right)
                }
                PartialExpr::ComparisonChain { first, rest } => {
                    SyntaxExpr::ComparisonChain { first, rest }
                }
            }
        }
    }

    fn resolve_infix_chain(
        first: SyntaxExpr,
        rest: Vec<(SyntaxOperator, SyntaxExpr)>,
    ) -> Result<SyntaxExpr, String> {
        let mut exprs = vec![PartialExpr::Expr(first)];
        let mut ops = Vec::new();

        for (next_op, next_expr) in rest {
            while let Some(previous_op) = ops.last().copied() {
                match infix_operator_relation(previous_op, next_op) {
                    OperatorRelation::Stronger | OperatorRelation::Same(Associativity::Left) => {
                        reduce_top_operator(&mut exprs, &mut ops)?
                    }
                    OperatorRelation::Weaker | OperatorRelation::Same(Associativity::Right) => {
                        break;
                    }
                    OperatorRelation::Same(Associativity::None)
                        if is_comparison_operator(previous_op)
                            && is_comparison_operator(next_op) =>
                    {
                        reduce_top_operator(&mut exprs, &mut ops)?
                    }
                    OperatorRelation::Same(Associativity::None) => {
                        return Err(format!(
                            "operator `{}` is non-associative; parenthesize this chain",
                            infix_operator_symbol(next_op)
                        ));
                    }
                    OperatorRelation::Unrelated => {
                        return Err(format!(
                            "operators `{}` and `{}` have no precedence relationship; parenthesize to disambiguate",
                            infix_operator_symbol(previous_op),
                            infix_operator_symbol(next_op)
                        ));
                    }
                }
            }

            ops.push(next_op);
            exprs.push(PartialExpr::Expr(next_expr));
        }

        while !ops.is_empty() {
            reduce_top_operator(&mut exprs, &mut ops)?;
        }

        exprs
            .pop()
            .map(PartialExpr::into_expr)
            .ok_or_else(|| "operator chain did not produce an expression".to_owned())
    }

    fn reduce_top_operator(
        exprs: &mut Vec<PartialExpr>,
        ops: &mut Vec<SyntaxOperator>,
    ) -> Result<(), String> {
        let right = exprs
            .pop()
            .map(PartialExpr::into_expr)
            .ok_or_else(|| "missing right operand in operator chain".to_owned())?;
        let left = exprs
            .pop()
            .ok_or_else(|| "missing left operand in operator chain".to_owned())?;
        let op = ops
            .pop()
            .ok_or_else(|| "missing operator in operator chain".to_owned())?;
        if is_comparison_operator(op) {
            match left {
                PartialExpr::Expr(left) => exprs.push(PartialExpr::ComparisonChain {
                    first: Box::new(left),
                    rest: vec![(op, right)],
                }),
                PartialExpr::ComparisonChain { first, mut rest } => {
                    rest.push((op, right));
                    exprs.push(PartialExpr::ComparisonChain { first, rest });
                }
            }
        } else {
            exprs.push(PartialExpr::Expr(syntax_binary_expr(
                op,
                left.into_expr(),
                right,
            )));
        }
        Ok(())
    }

    fn infix_operator_relation(left: SyntaxOperator, right: SyntaxOperator) -> OperatorRelation {
        use crate::core::Builtin::{
            Add, Append, Divide, Equal, Greater, GreaterEqual, Less, LessEqual, Multiply, NotEqual,
            Subtract,
        };
        use SyntaxOperator::{
            BoolAnd, BoolOr, Builtin, ComposeBackward, ComposeForward, EffectBind, EffectThen,
            KleisliCompose, PipeBackward, PipeForward,
        };

        match (left, right) {
            (BoolOr, BoolOr) | (BoolAnd, BoolAnd) => OperatorRelation::Same(Associativity::Left),
            (BoolOr, BoolAnd) => OperatorRelation::Weaker,
            (BoolAnd, BoolOr) => OperatorRelation::Stronger,
            (EffectBind, EffectBind)
            | (EffectBind, EffectThen)
            | (EffectThen, EffectBind)
            | (EffectThen, EffectThen) => OperatorRelation::Same(Associativity::Left),
            (KleisliCompose, KleisliCompose) => OperatorRelation::Same(Associativity::Right),
            (PipeForward, PipeForward) => OperatorRelation::Same(Associativity::Left),
            (PipeBackward, PipeBackward) => OperatorRelation::Same(Associativity::Right),
            (PipeForward, PipeBackward) | (PipeBackward, PipeForward) => {
                OperatorRelation::Unrelated
            }
            (ComposeForward, ComposeForward) => OperatorRelation::Same(Associativity::Left),
            (ComposeBackward, ComposeBackward) => OperatorRelation::Same(Associativity::Right),
            (ComposeForward, ComposeBackward) | (ComposeBackward, ComposeForward) => {
                OperatorRelation::Unrelated
            }
            (Builtin(Append), Builtin(Append)) => OperatorRelation::Same(Associativity::Left),
            (Builtin(Add), Builtin(Add)) => OperatorRelation::Same(Associativity::Left),
            (Builtin(Add), Builtin(Subtract)) | (Builtin(Subtract), Builtin(Add)) => {
                OperatorRelation::Unrelated
            }
            (Builtin(Subtract), Builtin(Subtract)) => OperatorRelation::Same(Associativity::None),
            (
                Builtin(Greater | GreaterEqual | Equal | NotEqual | LessEqual | Less),
                Builtin(Greater | GreaterEqual | Equal | NotEqual | LessEqual | Less),
            ) => OperatorRelation::Same(Associativity::None),
            (Builtin(Multiply), Builtin(Multiply))
            | (Builtin(Multiply), Builtin(Divide))
            | (Builtin(Divide), Builtin(Multiply)) => OperatorRelation::Same(Associativity::Left),
            (Builtin(Divide), Builtin(Divide)) => OperatorRelation::Same(Associativity::None),
            _ => match operator_precedence(left).cmp(&operator_precedence(right)) {
                std::cmp::Ordering::Greater => OperatorRelation::Stronger,
                std::cmp::Ordering::Less => OperatorRelation::Weaker,
                std::cmp::Ordering::Equal => OperatorRelation::Unrelated,
            },
        }
    }

    fn operator_precedence(operator: SyntaxOperator) -> u8 {
        use crate::core::Builtin::{
            Add, Append, Divide, Equal, Greater, GreaterEqual, Less, LessEqual, Multiply, NotEqual,
            Subtract,
        };
        use SyntaxOperator::{
            BoolAnd, BoolOr, Builtin, ComposeBackward, ComposeForward, EffectBind, EffectThen,
            KleisliCompose, PipeBackward, PipeForward,
        };

        match operator {
            BoolOr => 0,
            BoolAnd => 1,
            EffectBind | EffectThen => 2,
            PipeForward | PipeBackward => 3,
            ComposeForward | ComposeBackward | KleisliCompose => 4,
            Builtin(Greater | GreaterEqual | Equal | NotEqual | LessEqual | Less) => 5,
            Builtin(Append) => 6,
            Builtin(Add | Subtract) => 7,
            Builtin(Multiply | Divide) => 8,
            Builtin(_) => 9,
        }
    }

    fn infix_operator_symbol(operator: SyntaxOperator) -> &'static str {
        match operator {
            SyntaxOperator::BoolAnd => "and",
            SyntaxOperator::BoolOr => "or",
            SyntaxOperator::Builtin(crate::core::Builtin::Append) => "++",
            SyntaxOperator::Builtin(crate::core::Builtin::Add) => "+",
            SyntaxOperator::Builtin(crate::core::Builtin::Subtract) => "-",
            SyntaxOperator::Builtin(crate::core::Builtin::Multiply) => "*",
            SyntaxOperator::Builtin(crate::core::Builtin::Divide) => "/",
            SyntaxOperator::Builtin(crate::core::Builtin::Greater) => ">",
            SyntaxOperator::Builtin(crate::core::Builtin::GreaterEqual) => ">=",
            SyntaxOperator::Builtin(crate::core::Builtin::Equal) => "==",
            SyntaxOperator::Builtin(crate::core::Builtin::NotEqual) => "<>",
            SyntaxOperator::Builtin(crate::core::Builtin::LessEqual) => "=<",
            SyntaxOperator::Builtin(crate::core::Builtin::Less) => "<",
            SyntaxOperator::PipeForward => "|>",
            SyntaxOperator::PipeBackward => "<|",
            SyntaxOperator::ComposeForward => ">>",
            SyntaxOperator::ComposeBackward => "<<",
            SyntaxOperator::EffectBind => ">>=",
            SyntaxOperator::KleisliCompose => ">=>",
            SyntaxOperator::EffectThen => "=>>",
            SyntaxOperator::Builtin(crate::core::Builtin::Fixpoint) => "fixpoint",
            SyntaxOperator::Builtin(crate::core::Builtin::Anno) => "anno",
            SyntaxOperator::Builtin(crate::core::Builtin::MergeDuplicate) => "merge_duplicate",
            SyntaxOperator::Builtin(crate::core::Builtin::Floor) => "floor",
            SyntaxOperator::Builtin(crate::core::Builtin::Mod) => "mod",
            SyntaxOperator::Builtin(crate::core::Builtin::Slice) => "slice",
            SyntaxOperator::Builtin(crate::core::Builtin::Map) => "map",
            SyntaxOperator::Builtin(crate::core::Builtin::ListLen) => "list.len",
            SyntaxOperator::Builtin(crate::core::Builtin::ListSplit) => "list.split",
            SyntaxOperator::Builtin(crate::core::Builtin::ListSplitEnd) => "list.split_end",
            SyntaxOperator::Builtin(crate::core::Builtin::ListHead) => "list.head",
            SyntaxOperator::Builtin(crate::core::Builtin::ListTail) => "list.tail",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffect) => "list.pure",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffectReturn) => "list.pure.r",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffectSeq) => "list.pure.seq",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffectAlt) => "list.pure.alt",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffectCut) => "list.pure.cut",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffectFix) => "list.pure.fix",
            SyntaxOperator::Builtin(crate::core::Builtin::DictSingleton) => ":",
            SyntaxOperator::Builtin(crate::core::Builtin::DictUnion) => "{,}",
            SyntaxOperator::Builtin(crate::core::Builtin::DictUpdate) => "dict_update",
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectSpec) => "object_spec",
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectLocalName) => "object_local_name",
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectInstanceFromParts) => {
                "object_instance_from_parts"
            }
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectInstance) => "object_instance",
            SyntaxOperator::Builtin(crate::core::Builtin::EffectApply) => "effect_apply",
            SyntaxOperator::Builtin(crate::core::Builtin::EffectCall) => "effect_call",
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectDefaultDefs) => {
                "object_default_defs"
            }
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectDictDefs) => "object_dict_defs",
        }
    }

    fn access_if_path(base: SyntaxExpr, suffixes: Vec<PathSuffix>) -> SyntaxExpr {
        match flatten_path_suffixes(suffixes) {
            parts if parts.is_empty() => base,
            parts => SyntaxExpr::Access(Box::new(base), parts),
        }
    }

    recursive(|expr| {
        let name = glam_name().boxed();
        let expr_name = glam_name()
            .try_map(|name, span| match name.as_str() {
                "and" | "or" => Err(Rich::custom(span, format!("`{name}` is a keyword"))),
                _ => Ok(name),
            })
            .boxed();
        let local = local_name().boxed();

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
            .clone()
            .padded()
            .delimited_by(just('('), just(')'))
            .map(|expr| PathSuffix::Single(SyntaxKeyExpr::PathIndex(Box::new(expr))));

        // Dotted paths stay lexically tight because `.` has other roles in the
        // language surface, such as future effect sugar like `.bar`.
        let path_suffix = just('.')
            .ignore_then(choice((
                path_list_shorthand,
                path_list_expr,
                expr_name
                    .clone()
                    .map(SyntaxKeyExpr::Atom)
                    .map(PathSuffix::Single),
            )))
            .repeated()
            .collect::<Vec<_>>();

        let prior_name = just('_')
            .ignore_then(expr_name.clone())
            .then(path_suffix.clone())
            .map(|(name, suffixes)| access_if_path(SyntaxExpr::PriorName(name), suffixes))
            .boxed();
        let escaped_expr = just('^')
            .repeated()
            .at_least(1)
            .collect::<Vec<_>>()
            .then(choice((
                expr.clone().padded().delimited_by(just('('), just(')')),
                expr_name
                    .clone()
                    .then(path_suffix.clone())
                    .map(|(name, suffixes)| access_if_path(SyntaxExpr::Name(name), suffixes)),
            )))
            .then(path_suffix.clone())
            .map(|((carets, escaped), suffixes)| {
                access_if_path(
                    SyntaxExpr::Escape(carets.len(), Box::new(escaped)),
                    suffixes,
                )
            })
            .boxed();
        let name_expr = expr_name
            .clone()
            .then(path_suffix.clone())
            .map(|(name, suffixes)| access_if_path(SyntaxExpr::Name(name), suffixes))
            .boxed();
        let effect_expr = just('.')
            .ignore_then(name.clone())
            .map(SyntaxExpr::Effect)
            .boxed();

        let number_literal = choice((
            just('_').then(one_of("0123456789")).ignored(),
            one_of("0123456789").ignored(),
        ))
        .then(one_of("0123456789_.xXbBeEaAcCdDfF").repeated().to_slice())
        .to_slice();
        let number = number_literal.try_map(|text: &str, span| {
            Number::parse(text).map(SyntaxExpr::Number).map_err(|err| {
                Rich::custom(span, format!("invalid number literal `{text}`: {err}"))
            })
        });
        let text = quoted_text().map(SyntaxExpr::Text);
        let atom_literal = just('\'').ignore_then(name.clone()).map(SyntaxExpr::Atom);
        let unit = just("()").map(|_| SyntaxExpr::Unit);

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
            name.clone().map(SyntaxKeyExpr::Atom),
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

        let infix_operator = choice((
            text::keyword("and").to(SyntaxOperator::BoolAnd),
            text::keyword("or").to(SyntaxOperator::BoolOr),
            just(">>=").to(SyntaxOperator::EffectBind),
            just(">=>").to(SyntaxOperator::KleisliCompose),
            just("=>>").to(SyntaxOperator::EffectThen),
            just(">=").to(SyntaxOperator::Builtin(crate::core::Builtin::GreaterEqual)),
            just("==").to(SyntaxOperator::Builtin(crate::core::Builtin::Equal)),
            just("<>").to(SyntaxOperator::Builtin(crate::core::Builtin::NotEqual)),
            just("=<").to(SyntaxOperator::Builtin(crate::core::Builtin::LessEqual)),
            just(">>")
                .then_ignore(just('=').not())
                .to(SyntaxOperator::ComposeForward),
            just("<<").to(SyntaxOperator::ComposeBackward),
            just("|>").to(SyntaxOperator::PipeForward),
            just("<|").to(SyntaxOperator::PipeBackward),
            just('>').to(SyntaxOperator::Builtin(crate::core::Builtin::Greater)),
            just('<').to(SyntaxOperator::Builtin(crate::core::Builtin::Less)),
            just("++").to(SyntaxOperator::Builtin(crate::core::Builtin::Append)),
            just('*').to(SyntaxOperator::Builtin(crate::core::Builtin::Multiply)),
            just('/').to(SyntaxOperator::Builtin(crate::core::Builtin::Divide)),
            just('+')
                .then_ignore(just('+').not())
                .to(SyntaxOperator::Builtin(crate::core::Builtin::Add)),
            just('-').to(SyntaxOperator::Builtin(crate::core::Builtin::Subtract)),
        ));
        let prefix_operator_section = infix_operator
            .clone()
            .padded()
            .then(expr.clone())
            .delimited_by(just('('), just(')'))
            .map(|(operator, right)| SyntaxExpr::OperatorSection {
                operator,
                left: None,
                right: Some(Box::new(right)),
            });
        let postfix_operator_section = expr
            .clone()
            .then(infix_operator.clone().padded())
            .delimited_by(just('('), just(')'))
            .map(|(left, operator)| SyntaxExpr::OperatorSection {
                operator,
                left: Some(Box::new(left)),
                right: None,
            });
        let bare_operator_section = infix_operator
            .clone()
            .padded()
            .delimited_by(just('('), just(')'))
            .map(|operator| SyntaxExpr::OperatorSection {
                operator,
                left: None,
                right: None,
            });
        let parenthesized = expr.clone().padded().delimited_by(just('('), just(')'));
        let lambda = just('\\')
            .padded()
            .ignore_then(
                local
                    .clone()
                    .padded()
                    .repeated()
                    .at_least(1)
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just("->").padded())
            .then(expr.clone())
            .map(|(params, body)| SyntaxExpr::Lambda(params, Box::new(body)));

        let literal_atom = choice((
            unit,
            text,
            atom_literal,
            list,
            dict,
            number,
            prefix_operator_section,
            postfix_operator_section,
            bare_operator_section,
            parenthesized,
        ))
        .boxed();
        let literal_expr = literal_atom
            .then(path_suffix.clone())
            .map(|(base, suffixes)| access_if_path(base, suffixes))
            .boxed();
        let atom = choice((
            literal_expr,
            escaped_expr,
            effect_expr,
            prior_name,
            name_expr,
        ))
        .boxed();
        let application = atom
            .clone()
            .then(
                whitespace1()
                    .ignore_then(atom.clone())
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .map(|(function, arguments)| {
                arguments.into_iter().fold(function, |function, argument| {
                    SyntaxExpr::Apply(Box::new(function), Box::new(argument))
                })
            })
            .boxed();
        choice((
            lambda,
            application
                .clone()
                .then(
                    infix_operator
                        .padded()
                        .then(application)
                        .repeated()
                        .collect::<Vec<_>>(),
                )
                .try_map(|(first, rest), span| {
                    resolve_infix_chain(first, rest).map_err(|message| Rich::custom(span, message))
                }),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Builtin;

    fn n(value: i64) -> Number {
        value.into()
    }

    #[test]
    fn parses_escaped_object_scope_names() {
        assert_eq!(
            parse_expr("^prefix.value"),
            Some(SyntaxExpr::Escape(
                1,
                Box::new(SyntaxExpr::Access(
                    Box::new(SyntaxExpr::Name("prefix".to_owned())),
                    vec![SyntaxKeyExpr::Atom("value".to_owned())],
                )),
            ))
        );
        assert_eq!(
            parse_expr("^^prefix"),
            Some(SyntaxExpr::Escape(
                2,
                Box::new(SyntaxExpr::Name("prefix".to_owned())),
            ))
        );
        assert_eq!(
            parse_expr("^(prefix ++ suffix).tail"),
            Some(SyntaxExpr::Access(
                Box::new(SyntaxExpr::Escape(
                    1,
                    Box::new(SyntaxExpr::Append(
                        Box::new(SyntaxExpr::Name("prefix".to_owned())),
                        Box::new(SyntaxExpr::Name("suffix".to_owned())),
                    )),
                )),
                vec![SyntaxKeyExpr::Atom("tail".to_owned())],
            ))
        );
    }

    #[test]
    fn parses_effect_shorthand_expressions() {
        assert_eq!(parse_expr("()"), Some(SyntaxExpr::Unit));
        assert_eq!(
            parse_expr(".emit"),
            Some(SyntaxExpr::Effect("emit".to_owned()))
        );
        assert_eq!(
            parse_expr(".emit 'eax 42"),
            Some(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Apply(
                    Box::new(SyntaxExpr::Effect("emit".to_owned())),
                    Box::new(SyntaxExpr::Atom("eax".to_owned())),
                )),
                Box::new(SyntaxExpr::Number(n(42))),
            ))
        );
    }

    #[test]
    fn parses_operator_sections() {
        assert_eq!(
            parse_expr("(+ 42)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::Builtin(Builtin::Add),
                left: None,
                right: Some(Box::new(SyntaxExpr::Number(n(42)))),
            })
        );
        assert_eq!(
            parse_expr("(42 -)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::Builtin(Builtin::Subtract),
                left: Some(Box::new(SyntaxExpr::Number(n(42)))),
                right: None,
            })
        );
        assert_eq!(
            parse_expr("(++ suffix)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::Builtin(Builtin::Append),
                left: None,
                right: Some(Box::new(SyntaxExpr::Name("suffix".to_owned()))),
            })
        );
        assert_eq!(
            parse_expr("(+)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::Builtin(Builtin::Add),
                left: None,
                right: None,
            })
        );
    }

    #[test]
    fn parses_pipe_and_composition_operators() {
        assert_eq!(
            parse_expr("value |> f"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::PipeForward,
                left: Box::new(SyntaxExpr::Name("value".to_owned())),
                right: Box::new(SyntaxExpr::Name("f".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("f <| value"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::PipeBackward,
                left: Box::new(SyntaxExpr::Name("f".to_owned())),
                right: Box::new(SyntaxExpr::Name("value".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("f >> g"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::ComposeForward,
                left: Box::new(SyntaxExpr::Name("f".to_owned())),
                right: Box::new(SyntaxExpr::Name("g".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("g << f"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::ComposeBackward,
                left: Box::new(SyntaxExpr::Name("g".to_owned())),
                right: Box::new(SyntaxExpr::Name("f".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("op >>= k"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::EffectBind,
                left: Box::new(SyntaxExpr::Name("op".to_owned())),
                right: Box::new(SyntaxExpr::Name("k".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("k1 >=> k2"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::KleisliCompose,
                left: Box::new(SyntaxExpr::Name("k1".to_owned())),
                right: Box::new(SyntaxExpr::Name("k2".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("op =>> next"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::EffectThen,
                left: Box::new(SyntaxExpr::Name("op".to_owned())),
                right: Box::new(SyntaxExpr::Name("next".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("(|> f)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::PipeForward,
                left: None,
                right: Some(Box::new(SyntaxExpr::Name("f".to_owned()))),
            })
        );
        assert_eq!(
            parse_expr("(>>)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::ComposeForward,
                left: None,
                right: None,
            })
        );
        assert_eq!(
            parse_expr("(>>= k)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::EffectBind,
                left: None,
                right: Some(Box::new(SyntaxExpr::Name("k".to_owned()))),
            })
        );
    }

    #[test]
    fn parses_comparison_and_boolean_operators() {
        assert_eq!(
            parse_expr("x < y"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::Builtin(Builtin::Less),
                left: Box::new(SyntaxExpr::Name("x".to_owned())),
                right: Box::new(SyntaxExpr::Name("y".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("x >= y"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::Builtin(Builtin::GreaterEqual),
                left: Box::new(SyntaxExpr::Name("x".to_owned())),
                right: Box::new(SyntaxExpr::Name("y".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("x and y or z"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::BoolOr,
                left: Box::new(SyntaxExpr::OperatorApply {
                    operator: SyntaxOperator::BoolAnd,
                    left: Box::new(SyntaxExpr::Name("x".to_owned())),
                    right: Box::new(SyntaxExpr::Name("y".to_owned())),
                }),
                right: Box::new(SyntaxExpr::Name("z".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("x < y =< z"),
            Some(SyntaxExpr::ComparisonChain {
                first: Box::new(SyntaxExpr::Name("x".to_owned())),
                rest: vec![
                    (
                        SyntaxOperator::Builtin(Builtin::Less),
                        SyntaxExpr::Name("y".to_owned()),
                    ),
                    (
                        SyntaxOperator::Builtin(Builtin::LessEqual),
                        SyntaxExpr::Name("z".to_owned()),
                    ),
                ],
            })
        );
        assert_eq!(parse_expr("and"), None);
        assert_eq!(
            parse_expr("android"),
            Some(SyntaxExpr::Name("android".to_owned()))
        );
        assert_eq!(parse_expr("'and"), Some(SyntaxExpr::Atom("and".to_owned())));
    }

    #[test]
    fn parses_let_and_where_expressions() {
        assert_eq!(
            parse_expr("let x = 1 in x + x"),
            Some(SyntaxExpr::Let {
                bindings: vec![("x".to_owned(), SyntaxExpr::Number(n(1)))],
                body: Box::new(SyntaxExpr::Add(
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                )),
            })
        );
        assert_eq!(
            parse_expr("let x = 1; _y = 2 in x"),
            Some(SyntaxExpr::Let {
                bindings: vec![
                    ("x".to_owned(), SyntaxExpr::Number(n(1))),
                    ("_y".to_owned(), SyntaxExpr::Number(n(2))),
                ],
                body: Box::new(SyntaxExpr::Name("x".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("let x = 1\n    y = 2\nx + y"),
            Some(SyntaxExpr::Let {
                bindings: vec![
                    ("x".to_owned(), SyntaxExpr::Number(n(1))),
                    ("y".to_owned(), SyntaxExpr::Number(n(2))),
                ],
                body: Box::new(SyntaxExpr::Add(
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                    Box::new(SyntaxExpr::Name("y".to_owned())),
                )),
            })
        );
        assert_eq!(
            parse_expr("x + y where x = 1; y = 2"),
            Some(SyntaxExpr::Let {
                bindings: vec![
                    ("x".to_owned(), SyntaxExpr::Number(n(1))),
                    ("y".to_owned(), SyntaxExpr::Number(n(2))),
                ],
                body: Box::new(SyntaxExpr::Add(
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                    Box::new(SyntaxExpr::Name("y".to_owned())),
                )),
            })
        );
    }

    #[test]
    fn dotted_paths_require_tight_dots() {
        assert!(matches!(
            parse_expr("foo.[  42  ].bar"),
            Some(SyntaxExpr::Access(_, _))
        ));
        assert!(matches!(
            parse_expr("foo.([1,2] ++ [3]).bar"),
            Some(SyntaxExpr::Access(_, _))
        ));

        assert_eq!(
            parse_expr("foo  .[42].bar"),
            None,
            "whitespace before `.` should be rejected"
        );
        assert_eq!(
            parse_expr("foo .bar"),
            Some(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Name("foo".to_owned())),
                Box::new(SyntaxExpr::Effect("bar".to_owned())),
            )),
            "whitespace before `.` should parse `.bar` as a separate effect expression"
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
    fn parses_dotted_paths_on_literal_expressions() {
        assert_eq!(
            parse_expr("{ hello:\"Hello\" }.hello"),
            Some(SyntaxExpr::Access(
                Box::new(SyntaxExpr::DictUnion(vec![SyntaxExpr::SingletonDict(
                    SyntaxKeyExpr::Atom("hello".to_owned()),
                    Box::new(SyntaxExpr::Text("Hello".to_owned())),
                )])),
                vec![SyntaxKeyExpr::Atom("hello".to_owned())],
            ))
        );
        assert_eq!(
            parse_expr("[\"Hello\"].[0]"),
            Some(SyntaxExpr::Access(
                Box::new(SyntaxExpr::List(vec![SyntaxExpr::Text("Hello".to_owned())])),
                vec![SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(0))))],
            ))
        );
        assert_eq!(
            parse_expr("(foo).bar"),
            Some(SyntaxExpr::Access(
                Box::new(SyntaxExpr::Name("foo".to_owned())),
                vec![SyntaxKeyExpr::Atom("bar".to_owned())],
            ))
        );
    }

    #[test]
    fn parses_dictionary_with_expressions() {
        assert!(matches!(
        parse_expr("{ x:1 } with\nx := 2\ny = x + 1"),
        Some(SyntaxExpr::With {
        alias: None,
        body,
        ..
        }) if body.len() == 2
        ));
        assert!(matches!(
        parse_expr("d as prior with\nx := _prior.x + 1"),
        Some(SyntaxExpr::With {
        alias: Some(alias),
        body,
        ..
        }) if alias == "prior" && body.len() == 1
        ));
        assert!(matches!(
        parse_expr("d as _prior with\nx := _prior.x + 1"),
        Some(SyntaxExpr::With {
        alias: Some(alias),
        body,
        ..
        }) if alias == "_prior" && body.len() == 1
        ));
        assert!(matches!(
        parse_expr("d as _ with\nx := 1"),
        Some(SyntaxExpr::With {
        alias: None,
        body,
        ..
        }) if body.len() == 1
        ));
    }

    #[test]
    fn parentheses_disambiguate_division_chains() {
        assert_eq!(parse_expr("3/4/5"), None);
        assert_eq!(parse_expr("3/4 / 5"), None);
        assert_eq!(
            parse_expr("(3/4) / 5"),
            Some(SyntaxExpr::Divide(
                Box::new(SyntaxExpr::Divide(
                    Box::new(SyntaxExpr::Number(n(3))),
                    Box::new(SyntaxExpr::Number(n(4))),
                )),
                Box::new(SyntaxExpr::Number(n(5))),
            ))
        );
        assert_eq!(
            parse_expr("3 / (4/5)"),
            Some(SyntaxExpr::Divide(
                Box::new(SyntaxExpr::Number(n(3))),
                Box::new(SyntaxExpr::Divide(
                    Box::new(SyntaxExpr::Number(n(4))),
                    Box::new(SyntaxExpr::Number(n(5))),
                )),
            ))
        );
        assert_eq!(
            parse_expr("2 * 3 / 4"),
            Some(SyntaxExpr::Divide(
                Box::new(SyntaxExpr::Multiply(
                    Box::new(SyntaxExpr::Number(n(2))),
                    Box::new(SyntaxExpr::Number(n(3))),
                )),
                Box::new(SyntaxExpr::Number(n(4))),
            ))
        );
        assert_eq!(parse_expr("3 - 4 - 5"), None);
        assert_eq!(
            parse_expr("(3 - 4) - 5"),
            Some(SyntaxExpr::Subtract(
                Box::new(SyntaxExpr::Subtract(
                    Box::new(SyntaxExpr::Number(n(3))),
                    Box::new(SyntaxExpr::Number(n(4))),
                )),
                Box::new(SyntaxExpr::Number(n(5))),
            ))
        );
        assert_eq!(
            parse_expr("3 - (4 - 5)"),
            Some(SyntaxExpr::Subtract(
                Box::new(SyntaxExpr::Number(n(3))),
                Box::new(SyntaxExpr::Subtract(
                    Box::new(SyntaxExpr::Number(n(4))),
                    Box::new(SyntaxExpr::Number(n(5))),
                )),
            ))
        );
        assert_eq!(parse_expr("3 + 4 - 5"), None);
        assert_eq!(
            parse_expr("(3 + 4) - 5"),
            Some(SyntaxExpr::Subtract(
                Box::new(SyntaxExpr::Add(
                    Box::new(SyntaxExpr::Number(n(3))),
                    Box::new(SyntaxExpr::Number(n(4))),
                )),
                Box::new(SyntaxExpr::Number(n(5))),
            ))
        );
        assert_eq!(
            parse_expr("3 + (4 - 5)"),
            Some(SyntaxExpr::Add(
                Box::new(SyntaxExpr::Number(n(3))),
                Box::new(SyntaxExpr::Subtract(
                    Box::new(SyntaxExpr::Number(n(4))),
                    Box::new(SyntaxExpr::Number(n(5))),
                )),
            ))
        );
    }
}
