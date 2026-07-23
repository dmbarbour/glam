use super::*;

fn assert_parses(source: &str) {
    parse_expression_fragment(source.as_bytes()).unwrap_or_else(|diagnostics| {
        panic!(
            "expression grammar rejected `{source}`: {}",
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )
    });
}

fn assert_rejects(source: &str) {
    assert!(
        parse_expression_fragment(source.as_bytes()).is_err(),
        "expression grammar unexpectedly accepted `{source}`"
    );
}

#[test]
fn ordinary_expressions_parse() {
    const EXPRESSIONS: &[&str] = &[
        "()",
        "name",
        "module",
        "self",
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
        "'where",
        ".where",
    ];

    for source in EXPRESSIONS {
        assert_parses(source);
    }
}

#[test]
fn every_g0_keyword_is_reserved_as_an_ordinary_name() {
    for keyword in crate::g_syntax::keywords::G0_KEYWORDS {
        if matches!(keyword.spelling(), "do" | "module" | "self") {
            continue;
        }
        let source = keyword.spelling();
        let diagnostics = parse_expression_fragment(source.as_bytes())
            .expect_err("a bare keyword should not parse as an ordinary name");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.message.contains(keyword.spelling())
                    && diagnostic.message.contains("reserved keyword")
                    && diagnostic
                        .message
                        .contains(&format!("'{}", keyword.spelling()))
            }),
            "missing keyword escape diagnostic for `{}`: {diagnostics:?}",
            keyword.spelling()
        );
    }

    let do_diagnostics = parse_expression_fragment(b"do")
        .expect_err("a bare `do` should be diagnosed as a malformed do expression");
    assert!(do_diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("do expression requires a newline-delimited block")
    }));
}

#[test]
fn keyword_data_escapes_remain_available() {
    for keyword in crate::g_syntax::keywords::G0_KEYWORDS {
        assert_parses(&format!("'{}", keyword.spelling()));
        assert_parses(&format!("root.{}", keyword.spelling()));
        assert_parses(&format!("root.['{}]", keyword.spelling()));
        assert_parses(&format!("{}:()", keyword.spelling()));
        assert_parses(&format!(":{}", keyword.spelling()));
    }
}

#[test]
fn ordinary_expression_rejections_are_preserved() {
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
        assert_rejects(source);
    }
}

#[test]
fn operator_relation_diagnostic_is_preserved() {
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
fn invalid_number_diagnostic_is_preserved() {
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
fn non_associative_operator_diagnostic_is_preserved() {
    let diagnostics =
        parse_expression_fragment(b"1 / 2 / 3").expect_err("division chains need parentheses");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("non-associative")),
        "{diagnostics:?}"
    );
}
