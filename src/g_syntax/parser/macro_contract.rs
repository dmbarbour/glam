//! Phase-zero executable contracts for the eventual `g0` macro parser.
//!
//! This module deliberately remains test-only. It uses the real source lexer
//! to pin macro-head tokenization and a dependency-independent reference
//! parser/matcher to pin the shared text-pattern language. Phase 1 replaces
//! the pattern oracle with production code; Phase 4 consumes macro heads in
//! the staged compiler. No macro expansion runs here.

use super::lexical::{LeadingTrivia, TokenKind, lex_source};

#[derive(Debug, PartialEq, Eq)]
struct MacroHead {
    path: String,
    token_index: usize,
}

fn macro_heads(source: &str) -> Result<Vec<MacroHead>, String> {
    let lexical = lex_source(source);
    if lexical.has_errors() {
        return Err(format!(
            "source failed lexical validation: {:#?}",
            lexical.diagnostics()
        ));
    }

    let tokens = lexical.tokens();
    let mut heads = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if !matches!(tokens[index].kind(), TokenKind::Symbol("@")) {
            index += 1;
            continue;
        }

        let macro_index = index;
        index += 1;
        let Some(first) = tokens.get(index) else {
            return Err("macro head is missing its static path".to_owned());
        };
        let TokenKind::Name(first_name) = first.kind() else {
            return Err("macro head requires a joint static name path".to_owned());
        };
        if first.leading() != LeadingTrivia::Joint {
            return Err("macro head requires its first name to be joint to `@`".to_owned());
        }

        let mut path = (*first_name).to_owned();
        index += 1;
        while tokens.get(index).is_some_and(|token| {
            matches!(token.kind(), TokenKind::Symbol("."))
                && token.leading() == LeadingTrivia::Joint
        }) {
            index += 1;
            let Some(component) = tokens.get(index) else {
                return Err("macro path ends after `.`".to_owned());
            };
            let TokenKind::Name(component_name) = component.kind() else {
                return Err("macro path requires a static name after `.`".to_owned());
            };
            if component.leading() != LeadingTrivia::Joint {
                return Err("macro path components must be joint".to_owned());
            }
            path.push('.');
            path.push_str(component_name);
            index += 1;
        }

        heads.push(MacroHead {
            path,
            token_index: macro_index,
        });
    }
    Ok(heads)
}

#[test]
fn static_macro_heads_use_real_lexer_jointness() {
    let cases: &[(&str, &[&str])] = &[
        // The bootstrap accepts one joint static name or path.
        ("@name", &["name"]),
        ("@name.child", &["name.child"]),
        ("@table.create value", &["table.create"]),
        // A spaced dot begins macro input rather than extending the head.
        ("@name .child", &["name"]),
        // Heads are found structurally inside groups and attached layouts.
        ("value = (@outer @inner input)", &["outer", "inner"]),
        ("value = @outer\n  @inner input", &["outer", "inner"]),
        // Source texts and comments never expose `@` to the macro scanner.
        ("value = \"@text\" # @comment", &[]),
    ];

    for (source, expected) in cases {
        let actual = macro_heads(source)
            .unwrap_or_else(|error| panic!("{source:?} should be accepted: {error}"))
            .into_iter()
            .map(|head| head.path)
            .collect::<Vec<_>>();
        assert_eq!(actual, *expected, "{source:?}");
    }
}

#[test]
fn malformed_or_dynamic_macro_heads_are_reserved_failures() {
    let cases = [
        // Missing and spaced heads do not name a macro.
        "@",
        "@ name",
        // Dynamic lookup remains outside the bootstrap contract.
        "@(name)",
        "@.name",
        "@name.[42]",
        // Every static path component is nonempty and joint.
        "@name.",
        "@name. child",
        "@name..child",
    ];

    for source in cases {
        assert!(
            macro_heads(source).is_err(),
            "{source:?} should be rejected"
        );
    }
}

#[test]
fn declaration_expansion_order_is_right_to_left() {
    let source = "value = @outer (@middle @inner input)";
    let mut heads = macro_heads(source).unwrap();
    heads.sort_by_key(|head| std::cmp::Reverse(head.token_index));
    assert_eq!(
        heads.into_iter().map(|head| head.path).collect::<Vec<_>>(),
        ["inner", "middle", "outer"]
    );
}

#[test]
fn textual_macro_output_reserves_source_markers() {
    for accepted in ["", "plain source", "\"ordinary source text\""] {
        assert!(valid_textual_write(accepted), "{accepted:?}");
    }
    for rejected in [
        "@",
        "#",
        "left@right",
        "text # comment",
        "\"@ still rejected by write.text\"",
    ] {
        assert!(!valid_textual_write(rejected), "{rejected:?}");
    }

    // `.write.data` is intentionally absent: its arbitrary Value payload is
    // atomic and must never be scanned for either reserved source marker.
}

fn valid_textual_write(text: &str) -> bool {
    !text.contains(['@', '#'])
}
