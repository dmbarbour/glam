use super::*;
use crate::g_syntax::parser::compound::parse_expr_result;

fn assert_parses_like_character_grammar(source: &str) {
    let character_ast = parse_expr_result(source)
        .unwrap_or_else(|error| panic!("character grammar rejected `{source}`: {error}"));
    let token_ast = parse_expression_fragment(source.as_bytes()).unwrap_or_else(|diagnostics| {
        panic!(
            "token grammar rejected `{source}`: {}",
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )
    });
    assert_eq!(
        token_ast, character_ast,
        "grammars produced different ASTs for `{source}`"
    );
}

fn assert_both_reject(source: &str) {
    assert!(
        parse_expr_result(source).is_err(),
        "character grammar unexpectedly accepted `{source}`"
    );
    assert!(
        parse_expression_fragment(source.as_bytes()).is_err(),
        "token grammar unexpectedly accepted `{source}`"
    );
}

#[test]
fn ordinary_expressions_match_the_character_grammar() {
    const EXPRESSIONS: &[&str] = &[
        "()",
        "name",
        "_prior",
        "^outer",
        "^^outer",
        "^(prefix ++ suffix).tail",
        "'atom",
        "'.foo.[42]",
        "'.foo.([1, 2]).bar",
        "'.[]",
        ".emit",
        ".heap.get",
        "0",
        "_42",
        "1/6",
        "3/4",
        "1.25e3",
        "\"ordinary text\"",
        "\"semicolons; and [brackets] {stay} text\"",
        "\"\"\"\n\" ordinary \"quotes\" are retained\n\"\"\"",
        "(grouped)",
        "(,)",
        "(singleton,)",
        "(,singleton)",
        "(first, second,)",
        "(\n, first\n, second\n,)",
        "[]",
        "[, first, second,]",
        "{}",
        "{, first, second,}",
        "{foo: 1, foo.bar: 2, [0, 1]: 3}",
        ":[tag]",
        ":foo.bar",
        "tag:value",
        "foo.bar:value",
        "[first, second]:value",
        "([first] ++ tail):value",
        "outer:inner:value",
        "g tag:f x",
        "tag:(f x)",
        "\\x y -> x y",
        "f x y z",
        "foo.bar",
        "foo .bar",
        "f\n  x\n  y",
        "(+)",
        "(+ 42)",
        "(42 -)",
        "(++ suffix)",
        "value |> f",
        "f <| value",
        "f >> g",
        "g << f",
        "op >>= k",
        "k1 >=> k2",
        "op =>> next",
        "first !> second !> function",
        "function <! first <! second",
        "1 + 2 * 3",
        "x < y =< z",
        "x and y or z",
        "android",
        "'and",
    ];

    for source in EXPRESSIONS {
        assert_parses_like_character_grammar(source);
    }
}

#[test]
fn ordinary_expression_rejections_match_the_character_grammar() {
    for source in [
        "and",
        "tag: value",
        ": tag",
        "(,,)",
        "(first,,second)",
        "(,,first)",
        "[first,,second]",
        "{first,,second}",
        "first !> function <! argument",
        "1 / 2 / 3",
        "foo. bar",
        "1e999999999999999999999",
    ] {
        assert_both_reject(source);
    }
}

#[test]
fn operator_relation_diagnostic_survives_token_migration() {
    let diagnostics = parse_expression_fragment(b"first !> function <! argument")
        .expect_err("opposing applicative directions need parentheses");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("no precedence relationship")),
        "{diagnostics:?}"
    );
}

#[test]
fn invalid_number_diagnostic_survives_token_migration() {
    let diagnostics = parse_expression_fragment(b"1e999999999999999999999")
        .expect_err("an exponent beyond the supported bound must be rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("invalid number literal")),
        "{diagnostics:?}"
    );
}

#[test]
fn non_associative_operator_diagnostic_survives_token_migration() {
    let diagnostics =
        parse_expression_fragment(b"1 / 2 / 3").expect_err("division chains need parentheses");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("non-associative")),
        "{diagnostics:?}"
    );
}
