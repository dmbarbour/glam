#[cfg(test)]
use chumsky::prelude::*;

#[cfg(test)]
use super::super::{
    Diagnostic, DoExpr, DoStep, DoStepKind, ObjectBodyDefinition, ObjectBodyDefinitionKind,
    SyntaxExpr, SyntaxKeyExpr,
};
#[cfg(test)]
use super::compound::{
    keyword_starts_at, matching_closing_delimiter, parse_expr_result_with_diagnostics,
    split_top_level_binding_equals, top_level_char_indices, top_level_token_indices,
};
#[cfg(test)]
use super::layout::{
    legacy_dedent_layout_block, legacy_is_glam_whitespace, legacy_local_name,
    legacy_split_layout_statements,
};

pub(super) mod token;

#[cfg(test)]
pub(in crate::g_syntax::parser) use legacy::{parse_braced_do_atoms_result, parse_do_expr_result};

#[cfg(test)]
mod legacy {
    use super::*;

    enum ParsedDoStatement {
        Abstract(Vec<String>),
        Bind { name: String, operation: SyntaxExpr },
        ValueBind { name: String, value: SyntaxExpr },
        Expr(SyntaxExpr),
    }

    #[derive(Clone, Copy)]
    struct DoStatementSource<'a> {
        text: &'a str,
        line: usize,
    }

    #[derive(Clone, Copy)]
    struct BracedDoRange {
        start: usize,
        open: usize,
        close: usize,
    }

    pub(in crate::g_syntax::parser) fn parse_braced_do_atoms_result(
        text: &str,
        line: usize,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Result<SyntaxExpr, String>> {
        let ranges = match find_braced_do_ranges(text) {
            Ok(ranges) if ranges.is_empty() => return None,
            Ok(ranges) => ranges,
            Err(message) => return Some(Err(message)),
        };

        let mut rewritten = String::with_capacity(text.len());
        let mut replacements = Vec::with_capacity(ranges.len());
        let mut copied_through = 0;

        for (index, range) in ranges.into_iter().enumerate() {
            rewritten.push_str(&text[copied_through..range.start]);
            let placeholder = fresh_placeholder(text, index);
            rewritten.push_str(&placeholder);

            let body = &text[range.open + 1..range.close];
            let body_line = line + line_break_count(&text[..range.open + 1]);
            let do_expr = match parse_braced_do_block(body, body_line, diagnostics) {
                Ok(do_expr) => do_expr,
                Err(message) => return Some(Err(message)),
            };
            replacements.push((placeholder, Some(do_expr)));
            copied_through = range.close + 1;
        }
        rewritten.push_str(&text[copied_through..]);

        let mut expr = match parse_expr_result_with_diagnostics(&rewritten, line, diagnostics) {
            Ok(expr) => expr,
            Err(message) => return Some(Err(message)),
        };
        replace_braced_do_placeholders(&mut expr, &mut replacements);
        debug_assert!(replacements.iter().all(|(_, expr)| expr.is_none()));
        Some(Ok(expr))
    }

    pub(in crate::g_syntax::parser) fn parse_do_expr_result(
        text: &str,
        line: usize,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Result<SyntaxExpr, String>> {
        if !text.contains(['\r', '\n'])
            && let Some(body) = text.strip_prefix("do")
            && body.chars().next().is_some_and(legacy_is_glam_whitespace)
        {
            let body = body.trim_start();
            if body.starts_with('{') {
                return None;
            }
            if body.is_empty() {
                return Some(Err("do expression requires a statement".to_owned()));
            }
            return Some(parse_do_statements(
                vec![DoStatementSource { text: body, line }],
                diagnostics,
            ));
        }

        let (header, body) = split_first_source_line(text);
        let header = header.trim_end();
        let (prefix, direct) = if header == "do" {
            ("", true)
        } else {
            let prefix = header.strip_suffix("do")?;
            if !prefix
                .chars()
                .next_back()
                .is_some_and(legacy_is_glam_whitespace)
            {
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

    fn find_braced_do_ranges(text: &str) -> Result<Vec<BracedDoRange>, String> {
        let mut ranges = Vec::new();
        let mut inline_text = false;
        let mut multiline_text = false;
        let mut index = 0;

        while index < text.len() {
            if multiline_text {
                if text[index..].starts_with("\"\"\"")
                    && text[line_start(text, index)..index]
                        .chars()
                        .all(|ch| ch == ' ')
                {
                    multiline_text = false;
                    index += 3;
                } else {
                    index += text[index..]
                        .chars()
                        .next()
                        .expect("index should remain on a character boundary")
                        .len_utf8();
                }
                continue;
            }
            if inline_text {
                let ch = text[index..]
                    .chars()
                    .next()
                    .expect("index should remain on a character boundary");
                inline_text = ch != '"';
                index += ch.len_utf8();
                continue;
            }
            if text[index..].starts_with("\"\"\"") {
                multiline_text = true;
                index += 3;
                continue;
            }

            let ch = text[index..]
                .chars()
                .next()
                .expect("index should remain on a character boundary");
            if ch == '"' {
                inline_text = true;
                index += ch.len_utf8();
                continue;
            }
            if ch == 'd'
                && keyword_starts_at(text, index, "do")
                && !text[..index]
                    .chars()
                    .next_back()
                    .is_some_and(|before| matches!(before, '\'' | '.'))
            {
                let after_keyword = index + "do".len();
                let mut open = after_keyword;
                while open < text.len()
                    && text[open..]
                        .chars()
                        .next()
                        .is_some_and(legacy_is_glam_whitespace)
                {
                    open += text[open..]
                        .chars()
                        .next()
                        .expect("checked whitespace character")
                        .len_utf8();
                }
                if open > after_keyword && text[open..].starts_with('{') {
                    let Some(close) = matching_closing_delimiter(text, open) else {
                        return Err(
                            "braced do expression has an unmatched or mismatched `}`".to_owned()
                        );
                    };
                    ranges.push(BracedDoRange {
                        start: index,
                        open,
                        close,
                    });
                    index = close + 1;
                    continue;
                }
            }
            index += ch.len_utf8();
        }

        Ok(ranges)
    }

    fn fresh_placeholder(text: &str, index: usize) -> String {
        let mut salt = 0;
        loop {
            let candidate = format!("GlamBracedDoInternal{index}x{salt}");
            if !text.contains(&candidate) {
                return candidate;
            }
            salt += 1;
        }
    }

    fn line_start(text: &str, index: usize) -> usize {
        text[..index]
            .rfind(['\r', '\n'])
            .map_or(0, |line_ending| line_ending + 1)
    }

    fn line_break_count(text: &str) -> usize {
        let bytes = text.as_bytes();
        let mut count = 0;
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                    count += 1;
                    index += 2;
                }
                b'\r' | b'\n' => {
                    count += 1;
                    index += 1;
                }
                _ => index += 1,
            }
        }
        count
    }

    fn parse_trailing_lambda_prefix(text: &str) -> Option<(Vec<String>, &str)> {
        let lambda = text.trim_start().strip_prefix('\\')?;
        let (parameters, body) = lambda.split_once("->")?;
        let parameters = legacy_local_name()
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
        let body = legacy_dedent_layout_block(body)?;
        let statements = legacy_split_layout_statements(&body)?;
        if statements.is_empty() {
            return Err("layout do expression requires at least one statement".to_owned());
        }
        parse_do_statements(
            statements
                .into_iter()
                .map(|statement| DoStatementSource {
                    text: statement.text.trim(),
                    line: do_line + 1 + statement.line_offset,
                })
                .collect(),
            diagnostics,
        )
    }

    fn parse_braced_do_block(
        body: &str,
        body_line: usize,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<SyntaxExpr, String> {
        if body.trim().is_empty() {
            return Ok(empty_do_expr(body_line));
        }

        let mut starts = Vec::new();
        let mut start = 0;
        for separator in top_level_char_indices(body, ';') {
            starts.push((start, separator));
            start = separator + 1;
        }
        starts.push((start, body.len()));

        if starts
            .first()
            .is_some_and(|&(start, end)| body[start..end].trim().is_empty())
        {
            starts.remove(0);
        }
        if starts
            .last()
            .is_some_and(|&(start, end)| body[start..end].trim().is_empty())
        {
            starts.pop();
        }
        if starts.is_empty() {
            return Err("`do {;}` is invalid; a semicolon is not an empty computation".to_owned());
        }

        let mut statements = Vec::with_capacity(starts.len());
        for (start, end) in starts {
            let segment = &body[start..end];
            let trimmed = segment.trim();
            if trimmed.is_empty() {
                return Err(
                    "braced do block contains an empty statement between semicolons".to_owned(),
                );
            }
            let leading = segment.len() - segment.trim_start().len();
            statements.push(DoStatementSource {
                text: trimmed,
                line: body_line + line_break_count(&body[..start + leading]),
            });
        }
        parse_do_statements(statements, diagnostics)
    }

    fn parse_do_statements(
        statements: Vec<DoStatementSource<'_>>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<SyntaxExpr, String> {
        let statements = statements
            .into_iter()
            .map(|statement| {
                parse_do_statement(statement.text, statement.line, diagnostics)
                    .map(|statement_kind| (statement.line, statement_kind))
            })
            .collect::<Result<Vec<_>, _>>()?;
        finish_do_statements(statements)
    }

    fn finish_do_statements(
        mut statements: Vec<(usize, ParsedDoStatement)>,
    ) -> Result<SyntaxExpr, String> {
        let (result_line, result_statement) = statements
            .pop()
            .expect("do statement sequence should be checked as non-empty");

        let mut steps = Vec::with_capacity(statements.len());
        for (line, statement) in statements {
            let kind = match statement {
                ParsedDoStatement::Abstract(names) => DoStepKind::Abstract(names),
                ParsedDoStatement::Bind { name, operation } => DoStepKind::Bind { name, operation },
                ParsedDoStatement::ValueBind { name, value } => {
                    DoStepKind::ValueBind { name, value }
                }
                ParsedDoStatement::Expr(expr) => DoStepKind::Then(expr),
            };
            steps.push(DoStep { line, kind });
        }

        let result = match result_statement {
            ParsedDoStatement::Expr(expr) => expr,
            ParsedDoStatement::Abstract(_) => {
                return Err(
                "do block cannot end with an abstract declaration; add its fulfillment and a final expression"
                    .to_owned(),
            );
            }
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

    fn empty_do_expr(line: usize) -> SyntaxExpr {
        SyntaxExpr::Do(DoExpr {
            steps: Vec::new(),
            result_line: line,
            result: Box::new(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Effect(vec!["r".to_owned()])),
                Box::new(SyntaxExpr::Unit),
            )),
        })
    }

    fn replace_braced_do_placeholders(
        expr: &mut SyntaxExpr,
        replacements: &mut [(String, Option<SyntaxExpr>)],
    ) {
        if let SyntaxExpr::Name(name) = expr
            && let Some((_, replacement)) = replacements
                .iter_mut()
                .find(|(placeholder, _)| placeholder == name)
        {
            *expr = replacement
                .take()
                .expect("each braced do placeholder should occur exactly once");
            return;
        }

        match expr {
            SyntaxExpr::Unit
            | SyntaxExpr::Number(_)
            | SyntaxExpr::Text(_)
            | SyntaxExpr::Atom(_)
            | SyntaxExpr::Effect(_)
            | SyntaxExpr::Name(_)
            | SyntaxExpr::PriorName(_) => {}
            SyntaxExpr::Escape(_, escaped) => {
                replace_braced_do_placeholders(escaped, replacements);
            }
            SyntaxExpr::Access(base, path) => {
                replace_braced_do_placeholders(base, replacements);
                replace_key_placeholders(path, replacements);
            }
            SyntaxExpr::Object(object) => {
                if let Some(name) = &mut object.name {
                    replace_braced_do_placeholders(name, replacements);
                }
                for dependency in &mut object.deps {
                    replace_braced_do_placeholders(dependency, replacements);
                }
                replace_object_body_placeholders(&mut object.body, replacements);
            }
            SyntaxExpr::With { base, body, .. } => {
                replace_braced_do_placeholders(base, replacements);
                replace_object_body_placeholders(body, replacements);
            }
            SyntaxExpr::PathDict(path, value) => {
                replace_key_placeholders(path, replacements);
                replace_braced_do_placeholders(value, replacements);
            }
            SyntaxExpr::TaggedConstructor(path) => {
                replace_key_placeholders(path, replacements);
            }
            SyntaxExpr::DictUnion(items) | SyntaxExpr::List(items) | SyntaxExpr::Tuple(items) => {
                for item in items {
                    replace_braced_do_placeholders(item, replacements);
                }
            }
            SyntaxExpr::Lambda(_, body) => replace_braced_do_placeholders(body, replacements),
            SyntaxExpr::Do(do_expr) => {
                for step in &mut do_expr.steps {
                    match &mut step.kind {
                        DoStepKind::Abstract(_) => {}
                        DoStepKind::Bind { operation, .. } => {
                            replace_braced_do_placeholders(operation, replacements);
                        }
                        DoStepKind::ValueBind { value, .. } => {
                            replace_braced_do_placeholders(value, replacements);
                        }
                        DoStepKind::Then(operation) => {
                            replace_braced_do_placeholders(operation, replacements);
                        }
                    }
                }
                replace_braced_do_placeholders(&mut do_expr.result, replacements);
            }
            SyntaxExpr::Let { bindings, body } => {
                for (_, value) in bindings {
                    replace_braced_do_placeholders(value, replacements);
                }
                replace_braced_do_placeholders(body, replacements);
            }
            SyntaxExpr::Apply(function, argument)
            | SyntaxExpr::Multiply(function, argument)
            | SyntaxExpr::Divide(function, argument)
            | SyntaxExpr::Add(function, argument)
            | SyntaxExpr::Subtract(function, argument)
            | SyntaxExpr::Append(function, argument) => {
                replace_braced_do_placeholders(function, replacements);
                replace_braced_do_placeholders(argument, replacements);
            }
            SyntaxExpr::OperatorApply { left, right, .. } => {
                replace_braced_do_placeholders(left, replacements);
                replace_braced_do_placeholders(right, replacements);
            }
            SyntaxExpr::ComparisonChain { first, rest } => {
                replace_braced_do_placeholders(first, replacements);
                for (_, operand) in rest {
                    replace_braced_do_placeholders(operand, replacements);
                }
            }
            SyntaxExpr::OperatorSection { left, right, .. } => {
                if let Some(left) = left {
                    replace_braced_do_placeholders(left, replacements);
                }
                if let Some(right) = right {
                    replace_braced_do_placeholders(right, replacements);
                }
            }
        }
    }

    fn replace_key_placeholders(
        path: &mut [SyntaxKeyExpr],
        replacements: &mut [(String, Option<SyntaxExpr>)],
    ) {
        for key in path {
            match key {
                SyntaxKeyExpr::Atom(_) => {}
                SyntaxKeyExpr::Index(expr) | SyntaxKeyExpr::PathIndex(expr) => {
                    replace_braced_do_placeholders(expr, replacements);
                }
            }
        }
    }

    fn replace_object_body_placeholders(
        body: &mut [ObjectBodyDefinition],
        replacements: &mut [(String, Option<SyntaxExpr>)],
    ) {
        for definition in body {
            match &mut definition.kind {
                ObjectBodyDefinitionKind::Definition(definition) => {
                    if let Some(expr) = &mut definition.expr {
                        replace_braced_do_placeholders(expr, replacements);
                    }
                }
                ObjectBodyDefinitionKind::Object(object) => {
                    replace_object_body_placeholders(&mut object.body, replacements);
                }
            }
        }
    }

    fn parse_do_statement(
        text: &str,
        line: usize,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<ParsedDoStatement, String> {
        if text == "abstract"
            || text
                .strip_prefix("abstract")
                .is_some_and(|rest| rest.chars().next().is_some_and(legacy_is_glam_whitespace))
        {
            let names = text
                .strip_prefix("abstract")
                .expect("checked abstract statement prefix")
                .trim();
            let names = legacy_local_name()
                .padded()
                .separated_by(just(',').padded())
                .at_least(1)
                .collect::<Vec<_>>()
                .then_ignore(end())
                .parse(names)
                .into_result()
                .map_err(|_| {
                    "do abstract declaration requires one or more comma-separated local names"
                        .to_owned()
                })?;
            if names.iter().any(|name| name == "_") {
                return Err(
                    "do abstract declaration cannot use the inaccessible `_` name".to_owned(),
                );
            }
            return Ok(ParsedDoStatement::Abstract(names));
        }

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
        legacy_local_name()
            .then_ignore(end())
            .parse(text)
            .into_result()
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::g_syntax::parser::compound::token::parse_compound_expression_fragment;

    fn parse_expr_result(source: &str) -> Result<SyntaxExpr, String> {
        parse_compound_expression_fragment(source.as_bytes()).map_err(|diagnostics| {
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.message)
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

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
    fn parses_explicit_recursive_do_declarations() {
        let expr = parse_expr_result(
            "do\nabstract left, _right\n.r (\\_ -> left) -> use_left\nleft = 1\nright <- .r 2\n.r (use_left ())",
        )
        .unwrap();
        let SyntaxExpr::Do(do_expr) = expr else {
            panic!("expected a do expression");
        };

        assert!(matches!(
            &do_expr.steps[0].kind,
            DoStepKind::Abstract(names) if names == &["left", "_right"]
        ));
        assert!(matches!(
            &do_expr.steps[2].kind,
            DoStepKind::ValueBind { name, .. } if name == "left"
        ));
        assert!(matches!(
            &do_expr.steps[3].kind,
            DoStepKind::Bind { name, .. } if name == "right"
        ));
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
    fn parses_braced_do_with_optional_outer_semicolons_and_lines() {
        let expr = parse_expr_result(
            "do {\n; .prepare\n; value <- .read\n; retained = value\n; .r retained\n;\n}",
        )
        .unwrap();
        let SyntaxExpr::Do(do_expr) = expr else {
            panic!("expected a braced do expression");
        };

        assert_eq!(
            do_expr
                .steps
                .iter()
                .map(|step| step.line)
                .collect::<Vec<_>>(),
            [2, 3, 4]
        );
        assert!(matches!(do_expr.steps[0].kind, DoStepKind::Then(_)));
        assert!(matches!(
            &do_expr.steps[1].kind,
            DoStepKind::Bind { name, .. } if name == "value"
        ));
        assert!(matches!(
            &do_expr.steps[2].kind,
            DoStepKind::ValueBind { name, .. } if name == "retained"
        ));
        assert_eq!(do_expr.result_line, 5);
    }

    #[test]
    fn empty_braced_do_returns_unit_through_the_effect_api() {
        for source in ["do {}", "do {   }", "do {\r\n   \r\n}"] {
            assert!(matches!(
                parse_expr_result(source),
                Ok(SyntaxExpr::Do(DoExpr { steps, result, .. }))
                    if steps.is_empty()
                        && matches!(result.as_ref(), SyntaxExpr::Apply(function, argument)
                            if matches!(function.as_ref(), SyntaxExpr::Effect(path) if path == &["r"])
                                && matches!(argument.as_ref(), SyntaxExpr::Unit))
            ));
        }
    }

    #[test]
    fn braced_do_is_an_expression_atom_and_nests_without_stealing_semicolons() {
        let expr = parse_expr_result(
            "consume [do { .r 1 }, do { text = \"a;b\"; .r text }, do { x = do .r 2; .r x }]",
        )
        .unwrap();
        let SyntaxExpr::Apply(_, arguments) = expr else {
            panic!("expected the braced blocks as a function argument");
        };
        let SyntaxExpr::List(arguments) = arguments.as_ref() else {
            panic!("expected a list argument");
        };
        assert_eq!(arguments.len(), 3);
        assert!(
            arguments
                .iter()
                .all(|argument| matches!(argument, SyntaxExpr::Do(_)))
        );

        let SyntaxExpr::Do(outer) = &arguments[2] else {
            unreachable!();
        };
        assert!(matches!(
            &outer.steps[0].kind,
            DoStepKind::ValueBind {
                value: SyntaxExpr::Do(_),
                ..
            }
        ));
        assert!(parse_expr_result("do .r 1; .r 2").is_err());
        assert!(matches!(
            parse_expr_result("result:do { .r 1 }"),
            Ok(SyntaxExpr::PathDict(_, value)) if matches!(value.as_ref(), SyntaxExpr::Do(_))
        ));
    }

    #[test]
    fn braced_do_ignores_semicolons_in_multiline_text() {
        let expr = parse_expr_result(
            "do {\n; text = \"\"\"\n  \" semicolon; remains text\n\"\"\"\n; .r text\n}",
        )
        .unwrap();
        let SyntaxExpr::Do(do_expr) = expr else {
            panic!("expected a braced do expression");
        };
        assert_eq!(do_expr.steps.len(), 1);
        assert!(matches!(
            &do_expr.steps[0].kind,
            DoStepKind::ValueBind {
                value: SyntaxExpr::Text(text),
                ..
            } if text == "semicolon; remains text"
        ));
        assert_eq!(do_expr.result_line, 5);
    }

    #[test]
    fn rejects_malformed_braced_do_blocks() {
        let cases = [
            ("do {;}", "semicolon is not an empty computation"),
            ("do { .r ();; .r () }", "empty statement"),
            ("do { value <- .r 1; }", "cannot end with a binding"),
            ("do { .r ()", "expected `}`"),
        ];

        for (source, expected) in cases {
            let error = parse_expr_result(source).unwrap_err();
            assert!(
                error.contains(expected),
                "`{source}` reported `{error}` instead of `{expected}`"
            );
        }
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
            (
                "do\nabstract\n.r ()",
                "requires one or more comma-separated local names",
            ),
            (
                "do\nabstract _\n.r ()",
                "cannot use the inaccessible `_` name",
            ),
            (
                "do\nabstract value",
                "cannot end with an abstract declaration",
            ),
        ];

        for (source, expected) in cases {
            let error = parse_expr_result(source).unwrap_err();
            assert!(
                error.contains(expected),
                "`{source}` reported `{error}` instead of `{expected}`"
            );
        }
        assert!(matches!(
            parse_expr_result("do value"),
            Ok(SyntaxExpr::Do(DoExpr { steps, result, .. }))
                if steps.is_empty() && matches!(result.as_ref(), SyntaxExpr::Name(name) if name == "value")
        ));
        assert!(parse_expr_result("abstract value").is_err());
    }
}
