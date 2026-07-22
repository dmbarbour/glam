use chumsky::prelude::*;

use super::super::{Diagnostic, DoExpr, DoStep, DoStepKind, SyntaxExpr};
use super::compound::{
    parse_expr_result_with_diagnostics, split_top_level_binding_equals, top_level_token_indices,
};
use super::layout::{dedent_layout_block, is_glam_whitespace, local_name, split_layout_statements};

enum ParsedDoStatement {
    Bind { name: String, operation: SyntaxExpr },
    ValueBind { name: String, value: SyntaxExpr },
    Expr(SyntaxExpr),
}

pub(super) fn parse_do_expr_result(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Result<SyntaxExpr, String>> {
    let (header, body) = split_first_source_line(text);
    let header = header.trim_end();
    let (prefix, direct) = if header == "do" {
        ("", true)
    } else {
        let prefix = header.strip_suffix("do")?;
        if !prefix.chars().next_back().is_some_and(is_glam_whitespace) {
            return None;
        }
        (prefix.trim_end(), false)
    };

    let Some(body) = body else {
        return Some(Err(
            "layout do expression requires a newline-delimited block".to_owned(),
        ));
    };

    enum Prefix {
        Direct,
        Apply(SyntaxExpr),
        Lambda(Vec<String>, Option<SyntaxExpr>),
    }

    let prefix = if direct {
        Prefix::Direct
    } else if let Some((parameters, body_prefix)) = parse_trailing_lambda_prefix(prefix) {
        let body_prefix = if body_prefix.is_empty() {
            None
        } else {
            let mut prefix_diagnostics = Vec::new();
            let Ok(body_prefix) =
                parse_expr_result_with_diagnostics(body_prefix, line, &mut prefix_diagnostics)
            else {
                return None;
            };
            diagnostics.extend(prefix_diagnostics);
            Some(body_prefix)
        };
        Prefix::Lambda(parameters, body_prefix)
    } else {
        let mut prefix_diagnostics = Vec::new();
        let Ok(function) =
            parse_expr_result_with_diagnostics(prefix, line, &mut prefix_diagnostics)
        else {
            return None;
        };
        diagnostics.extend(prefix_diagnostics);
        Prefix::Apply(function)
    };

    let do_expr = match parse_do_block(body, line, diagnostics) {
        Ok(expr) => expr,
        Err(message) => return Some(Err(message)),
    };
    Some(Ok(match prefix {
        Prefix::Direct => do_expr,
        Prefix::Apply(function) => SyntaxExpr::Apply(Box::new(function), Box::new(do_expr)),
        Prefix::Lambda(parameters, body_prefix) => {
            let body = match body_prefix {
                Some(function) => SyntaxExpr::Apply(Box::new(function), Box::new(do_expr)),
                None => do_expr,
            };
            SyntaxExpr::Lambda(parameters, Box::new(body))
        }
    }))
}

fn split_first_source_line(text: &str) -> (&str, Option<&str>) {
    let Some(index) = text.find(['\r', '\n']) else {
        return (text, None);
    };
    let after = if text[index..].starts_with("\r\n") {
        index + 2
    } else {
        index + 1
    };
    (&text[..index], Some(&text[after..]))
}

fn parse_trailing_lambda_prefix(text: &str) -> Option<(Vec<String>, &str)> {
    let lambda = text.trim_start().strip_prefix('\\')?;
    let (parameters, body) = lambda.split_once("->")?;
    let parameters = local_name()
        .padded()
        .repeated()
        .at_least(1)
        .collect::<Vec<_>>()
        .then_ignore(end())
        .parse(parameters)
        .into_result()
        .ok()?;
    Some((parameters, body.trim()))
}

fn parse_do_block(
    body: &str,
    do_line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<SyntaxExpr, String> {
    let body = dedent_layout_block(body)?;
    let mut statements = split_layout_statements(&body)?;
    let Some(result_statement) = statements.pop() else {
        return Err("layout do expression requires at least one statement".to_owned());
    };

    let mut steps = Vec::with_capacity(statements.len());
    for statement in statements {
        let line = do_line + 1 + statement.line_offset;
        let kind = match parse_do_statement(statement.text.trim(), line, diagnostics)? {
            ParsedDoStatement::Bind { name, operation } => DoStepKind::Bind { name, operation },
            ParsedDoStatement::ValueBind { name, value } => DoStepKind::ValueBind { name, value },
            ParsedDoStatement::Expr(expr) => DoStepKind::Then(expr),
        };
        steps.push(DoStep { line, kind });
    }

    let result_line = do_line + 1 + result_statement.line_offset;
    let result = match parse_do_statement(result_statement.text.trim(), result_line, diagnostics)? {
        ParsedDoStatement::Expr(expr) => expr,
        ParsedDoStatement::Bind { .. } | ParsedDoStatement::ValueBind { .. } => {
            return Err("do block cannot end with a binding; add a final expression".to_owned());
        }
    };
    Ok(SyntaxExpr::Do(DoExpr {
        steps,
        result_line,
        result: Box::new(result),
    }))
}

fn parse_do_statement(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<ParsedDoStatement, String> {
    if let Some(index) = top_level_token_indices(text, "<-").into_iter().next() {
        let name = text[..index].trim();
        let operation = text[index + 2..].trim();
        let Some(name) = parse_exact_local_name(name) else {
            return Err(
                "patterns are not yet supported in do bindings; expected a local name before `<-`"
                    .to_owned(),
            );
        };
        if operation.is_empty() {
            return Err(format!(
                "do binding `{name}` requires an operation after `<-`"
            ));
        }
        return Ok(ParsedDoStatement::Bind {
            name,
            operation: parse_expr_result_with_diagnostics(operation, line, diagnostics)?,
        });
    }

    let forward_arrows = top_level_token_indices(text, "->");
    if let Some(index) = forward_arrows.first().copied()
        && text[..index].trim().is_empty()
    {
        return Err("a do forward binding requires an operation before `->`".to_owned());
    }
    for index in forward_arrows.into_iter().rev() {
        let operation = text[..index].trim();
        if operation.is_empty() {
            continue;
        }
        let mut operation_diagnostics = Vec::new();
        let Ok(operation) =
            parse_expr_result_with_diagnostics(operation, line, &mut operation_diagnostics)
        else {
            continue;
        };
        let name = text[index + 2..].trim();
        let Some(name) = parse_exact_local_name(name) else {
            return Err(
                "a do forward binding requires exactly one local name after `->`".to_owned(),
            );
        };
        diagnostics.extend(operation_diagnostics);
        return Ok(ParsedDoStatement::Bind { name, operation });
    }

    if let Some((name, value)) = split_top_level_binding_equals(text) {
        let name = name.trim();
        let Some(name) = parse_exact_local_name(name) else {
            return Err("patterns are not yet supported in do value bindings; expected a local name before `=`".to_owned());
        };
        let value = value.trim();
        if value.is_empty() {
            return Err(format!(
                "do value binding `{name}` requires a value after `=`"
            ));
        }
        return Ok(ParsedDoStatement::ValueBind {
            name,
            value: parse_expr_result_with_diagnostics(value, line, diagnostics)?,
        });
    }

    parse_expr_result_with_diagnostics(text, line, diagnostics).map(ParsedDoStatement::Expr)
}

fn parse_exact_local_name(text: &str) -> Option<String> {
    local_name()
        .then_ignore(end())
        .parse(text)
        .into_result()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::g_syntax::parser::compound::parse_expr_result;

    #[test]
    fn parses_do_statement_kinds_and_preserves_lines() {
        let expr = parse_expr_result(
            "do\n.read 'left -> left\nright <- .read 'right\ntotal = left + right\n.write total\n.r total",
        )
        .unwrap();
        let SyntaxExpr::Do(do_expr) = expr else {
            panic!("expected a do expression");
        };

        assert_eq!(
            do_expr
                .steps
                .iter()
                .map(|step| step.line)
                .collect::<Vec<_>>(),
            [2, 3, 4, 5]
        );
        assert!(matches!(
            &do_expr.steps[0].kind,
            DoStepKind::Bind { name, .. } if name == "left"
        ));
        assert!(matches!(
            &do_expr.steps[1].kind,
            DoStepKind::Bind { name, .. } if name == "right"
        ));
        assert!(matches!(
            &do_expr.steps[2].kind,
            DoStepKind::ValueBind { name, .. } if name == "total"
        ));
        assert!(matches!(
            &do_expr.steps[3].kind,
            DoStepKind::Then(SyntaxExpr::Apply(_, _))
        ));
        assert_eq!(do_expr.result_line, 6);
        assert!(matches!(do_expr.result.as_ref(), SyntaxExpr::Apply(_, _)));
    }

    #[test]
    fn parses_nested_do_and_multiline_operations() {
        let expr = parse_expr_result(
            "do\nvalue <- do\n  input <- source\n  .r input\nwritten <- write\n  value\n.r written",
        )
        .unwrap();
        let SyntaxExpr::Do(do_expr) = expr else {
            panic!("expected an outer do expression");
        };

        assert!(matches!(
            &do_expr.steps[0].kind,
            DoStepKind::Bind {
                operation: SyntaxExpr::Do(_),
                ..
            }
        ));
        assert!(matches!(
            &do_expr.steps[1].kind,
            DoStepKind::Bind {
                operation: SyntaxExpr::Apply(_, _),
                ..
            }
        ));
        assert_eq!(do_expr.steps[1].line, 5);
        assert_eq!(do_expr.result_line, 7);
    }

    #[test]
    fn trailing_do_integrates_with_lambdas_and_applications() {
        assert!(matches!(
            parse_expr_result("interaction_net do\n.bind -> ports\n.r ports"),
            Ok(SyntaxExpr::Apply(_, argument)) if matches!(argument.as_ref(), SyntaxExpr::Do(_))
        ));
        assert!(matches!(
            parse_expr_result("\\api -> do\n.r api"),
            Ok(SyntaxExpr::Lambda(parameters, body))
                if parameters == ["api"] && matches!(body.as_ref(), SyntaxExpr::Do(_))
        ));
        assert!(matches!(
            parse_expr_result("\\api -> interaction_net do\n.bind -> ports\n.r ports"),
            Ok(SyntaxExpr::Lambda(_, body))
                if matches!(body.as_ref(), SyntaxExpr::Apply(_, argument)
                    if matches!(argument.as_ref(), SyntaxExpr::Do(_)))
        ));
    }

    #[test]
    fn lambda_arrows_are_not_do_bindings() {
        assert!(matches!(
            parse_expr_result("do\n\\value -> value"),
            Ok(SyntaxExpr::Do(DoExpr { steps, result, .. }))
                if steps.is_empty() && matches!(result.as_ref(), SyntaxExpr::Lambda(_, _))
        ));
        assert!(matches!(
            parse_expr_result("do\n(\\value -> value) -> function\n.r function"),
            Ok(SyntaxExpr::Do(DoExpr { steps, .. }))
                if matches!(&steps[0].kind, DoStepKind::Bind { name, operation }
                    if name == "function" && matches!(operation, SyntaxExpr::Lambda(_, _)))
        ));
    }

    #[test]
    fn arrows_inside_inline_and_multiline_text_are_not_bindings() {
        let expr = parse_expr_result(
            "do\n.write \"inline -> text\"\n.write \"\"\"\n  \" -> multiline text\n\"\"\"\n.r ()",
        )
        .unwrap();
        let SyntaxExpr::Do(do_expr) = expr else {
            panic!("expected a do expression");
        };

        assert!(
            do_expr
                .steps
                .iter()
                .all(|step| matches!(step.kind, DoStepKind::Then(_)))
        );
    }

    #[test]
    fn rejects_malformed_or_incomplete_do_blocks() {
        let cases = [
            ("do", "newline-delimited block"),
            ("do\nvalue <- .read", "cannot end with a binding"),
            (
                "do\n[first] <- .read\n.r first",
                "patterns are not yet supported",
            ),
            (
                "do\n.read -> first second\n.r ()",
                "requires exactly one local name",
            ),
            ("do\n-> result\n.r result", "requires an operation"),
            (
                "do\n  .first\n .second",
                "indented less than the first statement",
            ),
        ];

        for (source, expected) in cases {
            let error = parse_expr_result(source).unwrap_err();
            assert!(
                error.contains(expected),
                "`{source}` reported `{error}` instead of `{expected}`"
            );
        }
        assert!(parse_expr_result("do value").is_err());
    }
}
