use chumsky::prelude::*;

use super::*;
use crate::diagnostic::Severity;
use crate::g_syntax::parser::layout::{LayoutBase, LayoutView, validate_delimited_layouts};
use crate::g_syntax::parser::lexical::lex_source;

#[test]
fn token_views_constrain_ranges_and_project_source_locations() {
    let source = "alpha (beta [42]) omega\r\n  gamma \"text\"";
    let lexical = lex_source(source);
    let whole = TokenView::whole(&lexical);

    assert_eq!(whole.len(), lexical.tokens().len());
    assert!(!whole.is_empty());
    assert!(std::ptr::eq(whole.source(), &lexical));
    assert_eq!(whole.source_line(1), Some("alpha (beta [42]) omega"));
    assert_eq!(whole.source_line(2), Some("  gamma \"text\""));
    assert_eq!(whole.source_line(3), None);
    assert_eq!(whole.line_span(1).unwrap().range(), 0..23);
    assert_eq!(
        TokenView::declarations(&lexical)
            .map(|(line, declaration)| (line, declaration.source_text().unwrap()))
            .collect::<Vec<_>>(),
        [(1, source)]
    );

    let (first_index, first) = whole.first_significant().unwrap();
    let (last_index, last) = whole.last_significant().unwrap();
    assert!(matches!(first.kind(), TokenKind::Name("alpha")));
    assert!(matches!(last.kind(), TokenKind::Text(_)));
    assert_eq!(whole.absolute_index(first_index), Some(first_index));
    assert_eq!(whole.absolute_index(whole.len()), None);
    assert_eq!(whole.token_at(first_index), Some(first));
    assert_eq!(whole.token_at(last_index), Some(last));
    assert_eq!(whole.line_at_span(first.span()), Some(1));
    assert_eq!(whole.line_at_span(last.span()), Some(2));
    assert_eq!(whole.line_indentation_at(first_index), Some(0));
    assert_eq!(whole.line_indentation_at(last_index), Some(2));
    assert_eq!(&source[first.span().range()], "alpha");

    let significant = whole
        .subview(TokenRange::new(first_index, last_index + 1).unwrap())
        .unwrap();
    assert_eq!(significant.source_text(), Some(source));
    assert_eq!(significant.get(0), Some(first));
    assert_eq!(significant.absolute_index(0), Some(first_index));
    assert!(significant.before(0).unwrap().is_empty());
    assert!(significant.after(significant.len() - 1).unwrap().is_empty());
    assert!(TokenView::new(&lexical, TokenRange::new(0, whole.len() + 1).unwrap()).is_none());
    assert!(TokenRange::new(2, 1).is_none());
}

#[test]
fn top_level_iteration_jumps_balanced_groups() {
    let lexical = lex_source("alpha (beta [42]) omega");
    let whole = TokenView::whole(&lexical);
    let top = whole.top_level().collect::<Vec<_>>();
    let rendered = top
        .iter()
        .map(|indexed| whole.display(Some(indexed.token())).to_string())
        .collect::<Vec<_>>();

    assert_eq!(rendered, ["start of line", "`alpha`", "`(`", "`omega`"]);
    let open_index = top[2].index();
    let (group, contents) = whole.group_at(open_index, Delimiter::Parenthesis).unwrap();
    assert_eq!(
        whole.group(group).unwrap().delimiter(),
        Delimiter::Parenthesis
    );
    assert_eq!(contents.source_text(), Some("beta [42]"));
    assert_eq!(
        contents
            .top_level()
            .map(|indexed| contents.display(Some(indexed.token())).to_string())
            .collect::<Vec<_>>(),
        ["`beta`", "`[`"]
    );
    assert!(whole.group_at(open_index, Delimiter::Brace).is_none());
}

#[test]
fn mapped_input_parsers_preserve_token_categories_and_adjacency() {
    let parsed = parse_expression_fragment(b"command 42->value", |view| {
        let parser = keyword("command")
            .ignore_then(space_before(number().map(str::to_owned)))
            .then_ignore(joint(symbol("->")))
            .then(joint(name().map(str::to_owned)))
            .then_ignore(end());
        let result = parser.parse(view.chumsky_input()).into_result();
        result.map_err(|errors| diagnostics(view, errors))
    });
    assert_eq!(parsed.unwrap(), ("42".to_owned(), "value".to_owned()));

    let group = parse_expression_fragment(b"()", |view| {
        let parser = open(Delimiter::Parenthesis)
            .then(close(Delimiter::Parenthesis))
            .then_ignore(end());
        parser
            .parse(view.chumsky_input())
            .into_result()
            .map_err(|errors| diagnostics(view, errors))
    })
    .unwrap();
    assert_eq!(group.0, group.1);

    let text = parse_expression_fragment(b"\"payload\"", |view| {
        let result = text_id()
            .then_ignore(end())
            .parse(view.chumsky_input())
            .into_result();
        result
            .inspect(|id| assert_eq!(view.text(*id).unwrap().value(), "payload"))
            .map_err(|errors| diagnostics(view, errors))
    })
    .unwrap();
    let lexical = lex_source("\"payload\"");
    assert_eq!(lexical.text(text).unwrap().value(), "payload");
}

#[test]
fn line_start_and_layout_spacing_are_explicit_tokens() {
    let lexical = lex_source("first\n  second");
    let view = TokenView::whole(&lexical);
    let parser = line_start()
        .then(name())
        .then(line_break_before(line_start()))
        .then(space_before(name()))
        .then_ignore(end());

    assert_eq!(
        parser.parse(view.chumsky_input()).into_result().unwrap(),
        (((0, "first"), 2), "second")
    );

    let same_line = parse_expression_fragment(b"first second", |view| {
        name()
            .map(str::to_owned)
            .then(space_or_layout_before(name().map(str::to_owned)))
            .then_ignore(end())
            .parse(view.chumsky_input())
            .into_result()
            .map_err(|errors| diagnostics(view, errors))
    });
    assert_eq!(
        same_line.unwrap(),
        ("first".to_owned(), "second".to_owned())
    );
}

#[test]
fn token_errors_use_source_lines_and_source_lexemes() {
    let lexical = lex_source("first\n  42");
    let mut session = ParseSession::new(&lexical);
    let view = TokenView::whole(&lexical);
    let parser = line_start()
        .ignore_then(keyword("first"))
        .then_ignore(line_break_before(line_start()))
        .then_ignore(space_before(keyword("second")))
        .then_ignore(end());
    let errors = parser.parse(view.chumsky_input()).into_errors();
    session.record_token_errors(view, errors);

    let diagnostics = session.into_diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, Severity::Error);
    assert_eq!(diagnostics[0].line, 2);
    assert!(diagnostics[0].message.contains("second"));
    assert!(diagnostics[0].message.contains("`42`"));
}

#[test]
fn expression_fragment_validation_precedes_token_parsing() {
    let utf8 = parse_expression_fragment(&[0xff], |_| Ok::<_, Vec<Diagnostic>>(()));
    assert!(utf8.unwrap_err()[0].message.contains("valid UTF-8"));

    let structure = parse_expression_fragment(b"([)]", |_| Ok::<_, Vec<Diagnostic>>(()));
    assert!(
        structure
            .unwrap_err()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("mismatched closing delimiter"))
    );
}

#[test]
fn layout_lines_ignore_nested_groups_and_multiline_text() {
    let source = ".first {\n.at_column_zero\n}\n.next\n  continuation \"\"\"\ninside text\n\"\"\"";
    let lexical = lex_source(source);
    let view = TokenView::whole(&lexical);
    let layout = LayoutView::new(view);
    let lines = layout.lines();

    assert_eq!(
        lines
            .iter()
            .map(|line| (line.line(), line.indentation()))
            .collect::<Vec<_>>(),
        [(1, 0), (4, 0), (5, 2)]
    );
    assert_eq!(layout.tokens().range(), view.range());
    assert_eq!(lines[0].tokens().start(), view.range().start() + 1);

    let statements = layout.statements(LayoutBase::FirstLine).unwrap();
    assert_eq!(statements.len(), 2);
    assert_eq!(statements[0].line(), 1);
    assert_eq!(statements[1].line(), 4);
    assert_eq!(
        view.subview(statements[0].tokens()).unwrap().source_text(),
        Some(".first {\n.at_column_zero\n}")
    );
    assert_eq!(
        view.subview(statements[1].tokens()).unwrap().source_text(),
        Some(".next\n  continuation \"\"\"\ninside text\n\"\"\"")
    );
}

#[test]
fn layout_policy_handles_fixed_bases_continuations_and_closers() {
    let lexical = lex_source("(\n  .first\n)\n  .second\n    continuation");
    let whole = TokenView::whole(&lexical);
    let open_index = whole
        .top_level()
        .find(|indexed| matches!(indexed.token().kind(), TokenKind::Open { .. }))
        .unwrap()
        .index();
    let group = match whole.token_at(open_index).unwrap().kind() {
        TokenKind::Open { group, .. } => *group,
        _ => unreachable!(),
    };
    let close = whole.group(group).unwrap().close_token().unwrap();
    let body_through_close = whole
        .subview(TokenRange::new(open_index + 1, close + 1).unwrap())
        .unwrap();
    let statements = LayoutView::new(body_through_close)
        .statements(LayoutBase::Indentation(2))
        .unwrap();
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0].line(), 2);

    let second_start = close + 1;
    let second = whole
        .subview(TokenRange::new(second_start, whole.range().end()).unwrap())
        .unwrap();
    let statements = LayoutView::new(second)
        .statements(LayoutBase::Indentation(2))
        .unwrap();
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0].line(), 4);

    let error = LayoutView::new(second)
        .statements(LayoutBase::Indentation(4))
        .unwrap_err();
    assert_eq!(error.line(), 4);
    assert!(error.message().contains("expected at least 4"));
}

#[test]
fn layout_blocks_report_the_first_unconsumed_dedent() {
    let lexical = lex_source("  first\n    continuation\n  second\n boundary\nlater");
    let view = TokenView::whole(&lexical);
    let block = LayoutView::new(view).block(LayoutBase::FirstLine).unwrap();

    assert_eq!(block.anchor(), 2);
    assert_eq!(block.statements().len(), 2);
    let boundary = block.boundary().expect("the block should yield at line 4");
    assert_eq!(boundary.line(), 4);
    assert_eq!(boundary.indentation(), 1);
    assert_eq!(
        view.subview(
            TokenRange::new(block.end(), view.range().end())
                .expect("the yielded tail is an ordered range")
        )
        .unwrap()
        .source_text(),
        Some("boundary\nlater")
    );
}

#[test]
fn hanging_layout_uses_the_inline_member_column_for_later_lines() {
    let lexical = lex_source("first\n   second\n     continuation\nboundary");
    let view = TokenView::whole(&lexical);
    let block = LayoutView::new(view).block(LayoutBase::Hanging(3)).unwrap();

    assert_eq!(block.anchor(), 3);
    assert_eq!(block.statements().len(), 2);
    assert_eq!(block.statements()[0].line(), 1);
    assert_eq!(block.statements()[1].line(), 2);
    assert_eq!(block.boundary().map(|line| line.line()), Some(4));
}

#[test]
fn delimited_layout_anchors_only_post_opening_group_contributions() {
    for source in [
        "[1,2,3,4,\n  5,6,7,\n  8,9,10]",
        "[1,2\n  ,3,4\n  ,5,6]",
        "[make\n    long_argument,\n  next_member]",
        "[\n  make\n    long_argument,\n  next_member\n]",
        "[\n  make\n  long_argument,\n  next_member\n]",
        "(\n  first,\n  second\n)",
        "{first:1,\n  second:2,\n  third:3}",
        "do { first\n   ; second\n   ; third }",
        ".with {first:1,\n  second:2,\n  third:3}",
    ] {
        let lexical = lex_source(source);
        assert_eq!(validate_delimited_layouts(&lexical), [], "{source}");
    }
}

#[test]
fn delimited_layout_reports_misaligned_contributions() {
    for (source, expected) in [
        ("[1,2,\n  3,4,\n   5,6]", "expected content indentation 2"),
        (
            "do { first\n   ; second\n    ; third }",
            "expected content indentation 3",
        ),
        (
            "{first:1,\n  second:2,\n   third:3}",
            "expected content indentation 2",
        ),
    ] {
        let lexical = lex_source(source);
        let diagnostics = validate_delimited_layouts(&lexical);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "`{source}` reported {diagnostics:#?}"
        );
    }
}

fn diagnostics<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    errors: Vec<TokenError<'lex, 'source>>,
) -> Vec<Diagnostic> {
    let mut session = ParseSession::new(view.source());
    session.record_token_errors(view, errors);
    session.into_diagnostics()
}
