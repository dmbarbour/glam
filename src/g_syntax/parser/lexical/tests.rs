use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::diagnostic::Severity;
use crate::g_syntax::parser::source::parse_source;

#[test]
fn scans_validation_structure_and_declarations_together() {
    let source = "language g0\r\nvalue = do {\n  text = \"#;[]\"\n  .r (text)\n}\nnext = 1\n";
    let lexed = lex_source(source);

    assert_eq!(
        lexed.diagnostics(),
        vec![Diagnostic::warn(1, "source uses inconsistent line endings")]
    );
    assert!(!lexed.has_errors());
    assert_eq!(lexed.declarations().len(), 3);
    assert_eq!(
        lexed
            .declarations()
            .iter()
            .map(DeclarationSection::line)
            .collect::<Vec<_>>(),
        [1, 2, 6]
    );
    assert_eq!(lexed.groups().len(), 2);
    assert!(
        lexed
            .groups()
            .iter()
            .all(|group| group.close_token().is_some())
    );
    assert_eq!(lexed.text(0).unwrap().value(), "#;[]");
    assert!(lexed.invariants_hold());
}

#[test]
fn decodes_multiline_text_once_and_excludes_source_only_lines() {
    let source = concat!(
        "language g0\n",
        "text =\n",
        "    \"\"\"\n",
        "      \" first  \n",
        "  # source-only comment\n",
        "\n",
        "  \" second # retained\n",
        "      \" \"\"\" retained\n",
        "    \"\"\"\n",
    );
    let lexed = lex_source(source);

    assert_eq!(lexed.texts().len(), 1);
    assert_eq!(
        lexed.text(0).unwrap().value(),
        "first  \nsecond # retained\n\"\"\" retained"
    );
    assert!(lexed.text(0).unwrap().is_multiline());
    assert!(lexed.diagnostics().is_empty());
    assert!(lexed.invariants_hold());
}

#[test]
fn delimiter_recovery_keeps_only_correctly_paired_groups() {
    let source = "language g0\nbad = ([)]\nnext = 1\n";
    let lexed = lex_source(source);

    assert!(lexed.has_errors());
    assert_eq!(lexed.groups().len(), 2);
    let parent = lexed.group(0).unwrap();
    let child = lexed.group(1).unwrap();
    assert_eq!(parent.delimiter(), Delimiter::Parenthesis);
    assert_eq!(parent.close_token(), None);
    assert_eq!(parent.parent(), None);
    assert_eq!(child.delimiter(), Delimiter::Bracket);
    assert!(child.close_token().is_some());
    assert_eq!(child.parent(), Some(0));
    assert!(
        lexed
            .tokens()
            .iter()
            .any(|token| { matches!(token.kind(), TokenKind::Symbol(symbol) if *symbol == ")") })
    );
    assert_eq!(
        lexed
            .declarations()
            .iter()
            .map(DeclarationSection::line)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_diagnostic_messages(
        &lexed,
        &[
            "mismatched closing delimiter `)`; expected `]`",
            "unclosed delimiter; expected `)`",
        ],
    );
    assert!(lexed.invariants_hold());
}

#[test]
fn delimiters_inside_text_do_not_participate_in_recovery() {
    let lexed = lex_source("language g0\nvalue = ([\"})]\"]}\n");

    assert_eq!(lexed.groups().len(), 2);
    assert_eq!(lexed.text(0).unwrap().value(), "})]");
    assert_diagnostic_messages(
        &lexed,
        &[
            "mismatched closing delimiter `}`; expected `)`",
            "unclosed delimiter; expected `)`",
        ],
    );
    assert!(lexed.invariants_hold());
}

#[test]
fn source_validation_matches_the_previous_contract() {
    let source = concat!(
        "language g0\r\n",
        "separator\t= 1\n",
        "text = \"tab\there\"\n",
        "# non-breaking space: \u{00A0}\n",
    );
    let lexed = lex_source(source);

    assert!(lexed.has_errors());
    assert_eq!(
        lexed
            .diagnostics()
            .iter()
            .map(|diagnostic| (diagnostic.line, diagnostic.severity))
            .collect::<Vec<_>>(),
        [
            (1, crate::diagnostic::Severity::Warning),
            (2, crate::diagnostic::Severity::Error),
            (3, crate::diagnostic::Severity::Error),
            (4, crate::diagnostic::Severity::Error),
        ]
    );
}

#[test]
fn line_and_source_queries_cover_mixed_endings_and_utf8() {
    let source = "a\r\nβ\nc\rd";
    let lexed = lex_source(source);

    for (byte, line) in [
        (0, 1),
        (2, 1),
        (3, 2),
        (5, 2),
        (6, 3),
        (7, 3),
        (8, 4),
        (9, 4),
    ] {
        assert_eq!(lexed.line_at_byte(byte), Some(line), "byte {byte}");
    }
    assert_eq!(lexed.line_at_byte(10), None);
    assert_eq!(lexed.source_slice(ByteSpan { start: 3, end: 5 }), Some("β"));
    assert_eq!(lexed.source_slice(ByteSpan { start: 4, end: 5 }), None);
    assert_eq!(lexed.source(), source);
    assert!(lexed.invariants_hold());
}

#[test]
fn declaration_spans_exclude_following_comments_and_blank_lines() {
    let source = concat!(
        "language g0   # trailing comment\n",
        "# between declarations\n",
        "\n",
        "value = (\n",
        "  1\n",
        ")   # another comment\n",
        "\n",
        "# trailing source comment\n",
    );
    let lexed = lex_source(source);

    assert_eq!(lexed.declarations().len(), 2);
    assert_eq!(
        lexed.source_slice(lexed.declarations()[0].span()),
        Some("language g0")
    );
    assert_eq!(
        lexed.source_slice(lexed.declarations()[1].span()),
        Some("value = (\n  1\n)")
    );
    assert_eq!(
        lexed.group_contents(0),
        lexed
            .group(0)
            .and_then(|group| group.close_token())
            .map(|close| lexed.group(0).unwrap().open_token() + 1..close)
    );
    assert!(lexed.invariants_hold());
}

#[test]
fn lexical_boundaries_preserve_names_numbers_symbols_and_adjacency() {
    let cases = [
        (
            "_42 _name _ 1/6",
            vec![
                "number:_42",
                "name:_name",
                "name:_",
                "number:1",
                "symbol:/",
                "number:6",
            ],
        ),
        (
            "::= >>= >=> =>> := <- -> !> <! >= == <> =< >> << |> <| ++",
            vec![
                "symbol:::=",
                "symbol:>>=",
                "symbol:>=>",
                "symbol:=>>",
                "symbol::=",
                "symbol:<-",
                "symbol:->",
                "symbol:!>",
                "symbol:<!",
                "symbol:>=",
                "symbol:==",
                "symbol:<>",
                "symbol:=<",
                "symbol:>>",
                "symbol:<<",
                "symbol:|>",
                "symbol:<|",
                "symbol:++",
            ],
        ),
        (
            "'.foo.[42] .heap.get :tag tag:value ()",
            vec![
                "symbol:'",
                "symbol:.",
                "name:foo",
                "symbol:.",
                "open:[",
                "number:42",
                "close:]",
                "symbol:.",
                "name:heap",
                "symbol:.",
                "name:get",
                "symbol::",
                "name:tag",
                "name:tag",
                "symbol::",
                "name:value",
                "open:(",
                "close:)",
            ],
        ),
        ("name ? # ignored", vec!["name:name", "unknown:?"]),
    ];

    for (source, expected) in cases {
        let lexed = lex_source(source);
        assert_eq!(token_labels(&lexed), expected, "{source}");
        assert!(!lexed.has_errors(), "{source}: {:#?}", lexed.diagnostics());
        assert!(lexed.invariants_hold());
    }

    let lexed = lex_source("alpha beta.gamma\n  delta");
    let alpha = significant_token(&lexed, 0);
    let beta = significant_token(&lexed, 1);
    let dot = significant_token(&lexed, 2);
    let gamma = significant_token(&lexed, 3);
    let line_start = lexed
        .tokens()
        .iter()
        .find(|token| matches!(token.kind(), TokenKind::LineStart { indentation: 2 }))
        .unwrap();
    let delta = significant_token(&lexed, 4);
    assert_eq!(alpha.leading(), LeadingTrivia::Joint);
    assert_eq!(beta.leading(), LeadingTrivia::Space);
    assert_eq!(dot.leading(), LeadingTrivia::Joint);
    assert_eq!(gamma.leading(), LeadingTrivia::Joint);
    assert_eq!(line_start.leading(), LeadingTrivia::LineBreak);
    assert_eq!(delta.leading(), LeadingTrivia::Space);
}

#[test]
fn structural_errors_are_authoritative_and_suppress_grammar_cascades() {
    let cases = [
        (
            "language g0\nvalue = ([)]\nnext = 1\n",
            vec![
                (2, "mismatched closing delimiter"),
                (2, "unclosed delimiter"),
            ],
        ),
        (
            "language g0\nvalue = )\nnext = 1\n",
            vec![(2, "unmatched closing delimiter")],
        ),
        (
            "language g0\nvalue = \"unterminated\n",
            vec![(2, "unterminated text literal")],
        ),
        (
            "language g0\nvalue =\n  \"\"\"\n  \" no close\n",
            vec![(3, "unterminated multiline text literal")],
        ),
    ];

    for (source, expected) in cases {
        let lexed = lex_source(source);
        assert!(lexed.has_errors(), "{source}");
        assert!(lexed.invariants_hold(), "{source}");
        let parsed = parse_source(source.as_bytes());
        assert!(parsed.declarations.is_empty(), "{source}");
        for (line, message) in expected {
            assert!(
                parsed.diagnostics.iter().any(|diagnostic| {
                    diagnostic.line == line && diagnostic.message.contains(message)
                }),
                "{source}: {:#?}",
                parsed.diagnostics
            );
        }
    }
}

#[test]
fn declaration_sections_match_every_valid_sample() {
    for folder in [
        "samples/syntax",
        "samples/config",
        "samples/assembly",
        "samples/hello",
    ] {
        for path in sample_files(Path::new(folder)) {
            let bytes = fs::read(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let source = std::str::from_utf8(&bytes)
                .unwrap_or_else(|error| panic!("{} is not UTF-8: {error}", path.display()));
            let lexed = lex_source(source);
            assert!(
                !lexed.has_errors(),
                "{} had lexical errors: {:#?}",
                path.display(),
                lexed.diagnostics()
            );
            assert!(lexed.invariants_hold(), "{}", path.display());
            let parsed = parse_source(&bytes);
            assert_eq!(
                lexed.declarations().len(),
                parsed.declarations.len(),
                "{}",
                path.display()
            );
            assert_eq!(
                lexed
                    .declarations()
                    .iter()
                    .map(DeclarationSection::line)
                    .collect::<Vec<_>>(),
                parsed
                    .declarations
                    .iter()
                    .map(|declaration| declaration.line)
                    .collect::<Vec<_>>(),
                "{}",
                path.display()
            );
        }
    }
}

#[test]
fn invalid_syntax_fixtures_have_explicit_lexical_classification() {
    let expected_grammatical = [
        "ambiguous_slash.g",
        "bad_asm_result.g",
        "bad_language_decl.g",
        "let_where_syntax.g",
        "missing_language.g",
        "multiline_text.g",
        "path_whitespace.g",
        "tagged_spacing.g",
    ];
    let expected_lexical = ["unbalanced_delimiters.g"];
    let paths = sample_files(Path::new("samples/invalid/syntax"));
    assert_eq!(
        paths.len(),
        expected_grammatical.len() + expected_lexical.len()
    );

    for path in paths {
        let file_name = path.file_name().unwrap().to_str().unwrap();
        let bytes = fs::read(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let source = std::str::from_utf8(&bytes)
            .unwrap_or_else(|error| panic!("{} is not UTF-8: {error}", path.display()));
        let lexed = lex_source(source);
        if expected_grammatical.contains(&file_name) {
            assert!(
                !lexed.has_errors(),
                "{} unexpectedly failed lexically: {:#?}",
                path.display(),
                lexed.diagnostics()
            );
        } else {
            assert!(expected_lexical.contains(&file_name), "{file_name}");
            assert!(
                lexed.has_errors(),
                "{} unexpectedly reached grammatical parsing",
                path.display()
            );
        }
        assert!(lexed.invariants_hold(), "{}", path.display());
    }
}

fn assert_diagnostic_messages(lexed: &LexedSource<'_>, expected: &[&str]) {
    let errors = lexed
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert_eq!(errors, expected);
}

fn significant_token<'lex, 'source>(
    lexed: &'lex LexedSource<'source>,
    index: usize,
) -> &'lex SpannedToken<'source> {
    lexed
        .tokens()
        .iter()
        .filter(|token| !matches!(token.kind(), TokenKind::LineStart { .. }))
        .nth(index)
        .unwrap()
}

fn token_labels(lexed: &LexedSource<'_>) -> Vec<String> {
    lexed
        .tokens()
        .iter()
        .filter_map(|token| {
            Some(match token.kind() {
                TokenKind::Name(name) => format!("name:{name}"),
                TokenKind::Number(number) => format!("number:{number}"),
                TokenKind::Text(id) => format!("text:{}", lexed.text(*id).unwrap().value()),
                TokenKind::Symbol(symbol) => format!("symbol:{symbol}"),
                TokenKind::Open { delimiter, .. } => {
                    format!("open:{}", delimiter_symbol(*delimiter, true))
                }
                TokenKind::Close { delimiter, .. } => {
                    format!("close:{}", delimiter_symbol(*delimiter, false))
                }
                TokenKind::LineStart { .. } => return None,
                TokenKind::Unknown(ch) => format!("unknown:{ch}"),
            })
        })
        .collect()
}

fn delimiter_symbol(delimiter: Delimiter, opening: bool) -> char {
    match (delimiter, opening) {
        (Delimiter::Parenthesis, true) => '(',
        (Delimiter::Parenthesis, false) => ')',
        (Delimiter::Bracket, true) => '[',
        (Delimiter::Bracket, false) => ']',
        (Delimiter::Brace, true) => '{',
        (Delimiter::Brace, false) => '}',
    }
}

fn sample_files(folder: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(folder)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", folder.display()))
        .flat_map(|entry| {
            let path = entry
                .expect("sample directory entry should be readable")
                .path();
            if path.is_dir() {
                sample_files(&path)
            } else if path.extension().is_some_and(|extension| extension == "g") {
                vec![path]
            } else {
                Vec::new()
            }
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}
