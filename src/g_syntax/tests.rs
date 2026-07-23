use crate::compiler::CompileContext;
use crate::core::{Dict, Key, Value};
use crate::number::Number;

fn test_eval_context() -> crate::evaluation::EvalContext {
    crate::api::Assembler::default().eval_context()
}

fn core_global_access(context: &CompileContext, path: Vec<Key>) -> Value {
    lower_resolved_expr(ResolvedExpr::Access {
        base: Box::new(ResolvedExpr::Provided(context.final_defs().clone())),
        path: path.into_iter().map(ResolvedPathPart::Key).collect(),
    })
}

fn evaluated_module_value(context: &CompileContext, lowered: &LoweredSource) -> Value {
    let Value::Promised(final_defs) = context.final_defs() else {
        panic!("final module binding should be a promised value");
    };
    final_defs
        .set(lowered.definitions.clone())
        .expect("future should not be set yet");
    crate::eval::eval_value(&test_eval_context(), &lowered.definitions)
        .expect("lowered module should evaluate")
}

fn value_at_atom_path(definitions: &Value, path: &[&str]) -> Option<Value> {
    let context = test_eval_context();
    let mut current = definitions.clone();
    for part in path {
        let current_value = crate::eval::eval_value(&context, &current).ok()?;
        let Value::Dict(dict) = current_value else {
            return None;
        };
        current = dict
            .get(&Key::Atom(Atom::from_key(&Key::binary_from_text(*part))))
            .cloned()?;
    }
    Some(current)
}

fn resolved_value_at_path(definitions: &Value, path: &[&str]) -> Value {
    let value = value_at_atom_path(definitions, path).expect("binding should exist");
    crate::eval::eval_value(&test_eval_context(), &value).expect("binding should resolve")
}

fn resolved_value_at_path_with_context(
    context: &crate::evaluation::EvalContext,
    definitions: &Value,
    path: &[&str],
) -> Value {
    let mut current = definitions.clone();
    for part in path {
        let current_value = fully_evaluated_value_with_context(context, current);
        let Value::Dict(dict) = current_value else {
            panic!("reflection-enabled path should traverse dictionaries");
        };
        current = dict
            .get(&Key::atom_from_text(part))
            .cloned()
            .expect("reflection-enabled binding should exist");
    }
    fully_evaluated_value_with_context(context, current)
}

fn fully_evaluated_value_with_context(
    context: &crate::evaluation::EvalContext,
    mut value: Value,
) -> Value {
    while matches!(value, Value::Lazy(_) | Value::Promised(_)) {
        value = crate::eval::eval_value(context, &value)
            .expect("reflection-enabled value should fully evaluate");
    }
    value
}

fn fully_evaluated_value(mut value: Value) -> Value {
    let context = test_eval_context();
    while matches!(value, Value::Lazy(_) | Value::Promised(_)) {
        value = crate::eval::eval_value(&context, &value).expect("value should fully evaluate");
    }
    value
}

fn fully_evaluated_error(mut value: Value) -> crate::eval::EvalError {
    let context = test_eval_context();
    loop {
        match crate::eval::eval_value(&context, &value) {
            Ok(next @ (Value::Lazy(_) | Value::Promised(_))) => value = next,
            Ok(other) => panic!("value should fail instead of evaluating to {other:?}"),
            Err(error) => return error,
        }
    }
}

fn output_bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::Binary(bytes) => bytes.to_vec(),
        Value::List(list) => crate::eval::list_output_bytes(&test_eval_context(), list)
            .expect("output list should render as bytes"),
        other => panic!("expected binary output value, got {other:?}"),
    }
}

fn output_binary_result_list(value: &Value) -> Vec<u8> {
    let Value::List(list) = value else {
        panic!("expected list output value, got {value:?}");
    };
    let bytes = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |segment| {
            bytes.borrow_mut().extend_from_slice(segment);
            Ok::<_, String>(())
        },
        &mut |values| {
            for value in values {
                let value = fully_evaluated_value(
                    crate::eval::eval_value(&test_eval_context(), value)
                        .map_err(|err| err.to_string())?,
                );
                bytes.borrow_mut().extend(output_bytes(&value));
            }
            Ok(())
        },
        &mut |thunk| match crate::eval::eval_value(
            &test_eval_context(),
            &match thunk {
                crate::core::ListThunk::Lazy(lazy) => Value::Lazy(lazy.clone()),
                crate::core::ListThunk::Promised(promise) => Value::Promised(promise.clone()),
            },
        )
        .map_err(|err| err.to_string())?
        {
            Value::Binary(bytes) => Ok(crate::core::List::from_bytes(bytes)),
            Value::List(list) => Ok(list),
            other => Err(format!(
                "lazy output chunk was not a list or binary: {other:?}"
            )),
        },
    )
    .expect("result list should render as binary values");
    bytes.into_inner()
}

use super::*;
use crate::diagnostic::Severity;
use std::sync::{Arc, Mutex};

fn parse(text: &str) -> ParsedSource {
    parse_source(text.as_bytes())
}

fn static_definition_target(path: &str) -> Vec<SyntaxKeyExpr> {
    path.split('.')
        .map(|part| SyntaxKeyExpr::Atom(part.to_owned()))
        .collect()
}

fn lower_with_module_path(text: &str, module_path: &[&str]) -> LoweredSource {
    let parsed = parse(text);
    let context = CompileContext::from_module_path(module_path.iter().copied());
    lower_parsed_source(parsed, &context)
}

fn abstract_path_atom(parts: &[&str]) -> Value {
    Value::Atom(Atom::from_key(&Key::abstract_global_path(
        parts.iter().copied(),
    )))
}

fn n(value: i64) -> Number {
    value.into()
}

fn reflection_test_module(
    source: &str,
    module_path: &[&str],
    guards: &[(&str, &str)],
) -> (
    crate::api::Assembler,
    crate::evaluation::EvalContext,
    Value,
    Arc<Mutex<Vec<crate::api::DiagnosticEvent>>>,
) {
    let prior = guards.iter().fold(Dict::new_sync(), |dict, (name, path)| {
        let value = CompileContext::from_module_path(module_path.iter().copied())
            .abstract_global_path(path);
        dict.insert(Key::atom_from_text(name), value)
    });
    let prior = prior.insert(
        Key::atom_from_text("object_refl_marker"),
        (*keys::OBJECT_REFLECTION_GUARD_VALUE).clone(),
    );
    let context = CompileContext::from_module_path(module_path.iter().copied())
        .with_prior_defs(Value::Dict(prior));
    let lowered = lower_parsed_source(parse(source), &context);
    assert_eq!(lowered.diagnostics, []);
    let Value::Promised(final_defs) = context.final_defs() else {
        panic!("final module binding should be promised");
    };
    final_defs
        .set(lowered.definitions.clone())
        .expect("final module binding should be unset");

    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let received = diagnostics.clone();
    let assembler = crate::api::Assembler::default().with_diagnostic_callback(move |event| {
        received
            .lock()
            .expect("diagnostic collector should not be poisoned")
            .push(event);
    });
    let eval_context = assembler.eval_context();
    let definitions = crate::eval::eval_value(&eval_context, &lowered.definitions)
        .expect("reflection-enabled module should expose its dictionary");
    (assembler, eval_context, definitions, diagnostics)
}

fn take_reflection_diagnostics(
    diagnostics: &Arc<Mutex<Vec<crate::api::DiagnosticEvent>>>,
) -> Vec<crate::api::DiagnosticEvent> {
    std::mem::take(
        &mut *diagnostics
            .lock()
            .expect("diagnostic collector should not be poisoned"),
    )
}

#[test]
fn parses_language_declaration_with_extensions() {
    let parsed = parse("language g0 with utf8, demo\nanswer = 42\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[0].kind,
        DeclarationKind::Language(LanguageDecl {
            base: "g0".to_owned(),
            extensions: vec!["utf8".to_owned(), "demo".to_owned()],
        })
    );
}

#[test]
fn simple_declarations_preserve_indented_line_continuations() {
    let parsed = parse(concat!(
        "language\n",
        "  g0 with utf8,\n",
        "    demo\n",
        "import\n",
        "  'std\n",
        "  as standard\n",
        "abstract first,\n",
        "  nested.second\n",
        "unique\n",
        "  Marker\n",
    ));

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[0].kind,
        DeclarationKind::Language(LanguageDecl {
            base: "g0".to_owned(),
            extensions: vec!["utf8".to_owned(), "demo".to_owned()],
        })
    );
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Import(ImportDecl {
            reference: ImportReference::Builtin("std".to_owned()),
            binary: false,
            placement: ImportPlacement::As("standard".to_owned()),
        })
    );
    assert_eq!(
        parsed.declarations[2].kind,
        DeclarationKind::Abstract(vec!["first".to_owned(), "nested.second".to_owned()])
    );
    assert_eq!(
        parsed.declarations[3].kind,
        DeclarationKind::Unique(vec!["Marker".to_owned()])
    );
}

#[test]
fn simple_declarations_preserve_continuation_alignment_diagnostics() {
    let parsed = parse("language\n    g0\n  with utf8\n");

    assert!(parsed.diagnostics.iter().any(|diagnostic| {
        diagnostic.line == 3
            && diagnostic
                .message
                .contains("continuation indentation must align")
    }));
    assert!(matches!(
        parsed.declarations[0].kind,
        DeclarationKind::Language(_)
    ));
}

#[test]
fn reports_indented_lines_before_the_first_lexical_declaration() {
    let parsed = parse("  orphan = 1\nlanguage g0\n");

    assert!(parsed.diagnostics.iter().any(|diagnostic| {
        diagnostic.line == 1
            && diagnostic
                .message
                .contains("continuation line without a preceding declaration")
    }));
    assert_eq!(parsed.declarations.len(), 1);
    assert!(matches!(
        parsed.declarations[0].kind,
        DeclarationKind::Language(_)
    ));
}

#[test]
fn groups_indented_continuation_lines() {
    let parsed = parse("language g0\nfoo = do\n  .bar\n  .baz\nqux := 1\n");

    assert_eq!(parsed.declarations.len(), 3);
    assert_eq!(parsed.declarations[1].preview, "foo = do");
    assert_eq!(
        parsed.declarations[2].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("qux"),
            parameters: vec![],
            kind: DefinitionKind::Override,
            expr: Some(SyntaxExpr::Number(n(1))),
        })
    );
}

#[test]
fn multiline_texts_preserve_content_and_ignore_source_only_lines() {
    let source = concat!(
        "language g0\n",
        "inline = \"hash # retained\" # erased comment\n",
        "text =\n",
        "    \"\"\"\n",
        "      \" first  \n",
        "  # erased comment\n",
        "\n",
        "  \" second # retained\n",
        "      \" \"\"\" retained\n",
        "    \"\"\"\n",
        "object holder with\n",
        "  text =\n",
        "    \"\"\"\n",
        "      \" nested\n",
        "    \"\"\"\n",
    )
    .replace('\n', "\r\n");
    let parsed = parse(&source);

    assert_eq!(parsed.diagnostics, []);
    let DeclarationKind::Definition(inline) = &parsed.declarations[1].kind else {
        panic!("inline text should be a definition");
    };
    assert_eq!(
        inline.expr,
        Some(SyntaxExpr::Text("hash # retained".to_owned()))
    );
    let DeclarationKind::Definition(text) = &parsed.declarations[2].kind else {
        panic!("multiline text should be a definition");
    };
    assert_eq!(
        text.expr,
        Some(SyntaxExpr::Text(
            "first  \nsecond # retained\n\"\"\" retained".to_owned()
        ))
    );
    let DeclarationKind::Object(holder) = &parsed.declarations[3].kind else {
        panic!("holder should be an object declaration");
    };
    let nested = holder.body[0]
        .definition()
        .expect("holder member should be a definition");
    assert_eq!(nested.expr, Some(SyntaxExpr::Text("nested".to_owned())));
}

#[test]
fn rejects_every_source_whitespace_other_than_space_cr_and_lf() {
    let parsed = parse(concat!(
        "language g0\n",
        "separator\t= 1\n",
        "\tindent = 2\n",
        "text = \"tab\there\"\n",
        "# non-breaking space: \u{00A0}\n",
        "vertical = 3\u{000B}\n",
    ));

    assert!(parsed.declarations.is_empty());
    let expected = [
        (2, "U+0009"),
        (3, "U+0009"),
        (4, "U+0009"),
        (5, "U+00A0"),
        (6, "U+000B"),
    ];
    for (line, codepoint) in expected {
        assert!(parsed.diagnostics.iter().any(|diagnostic| {
            diagnostic.line == line
                && diagnostic.message.contains(codepoint)
                && diagnostic.message.contains("only SP, CR, and LF")
        }));
    }
}

#[test]
fn parses_local_imports() {
    let parsed = parse("language g0\nimport \"minimal.g\" as conf\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Import(ImportDecl {
            reference: ImportReference::Local("minimal.g".to_owned()),
            binary: false,
            placement: ImportPlacement::As("conf".to_owned()),
        })
    );

    let parsed = parse("language g0\nimport \"payload.bin\" binary as payload\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Import(ImportDecl {
            reference: ImportReference::Local("payload.bin".to_owned()),
            binary: true,
            placement: ImportPlacement::As("payload".to_owned()),
        })
    );
}

#[test]
fn binary_imports_require_a_named_target() {
    let lowered = lower_parsed_source(
        parse("language g0\nimport \"payload.bin\" binary\n"),
        &CompileContext::default(),
    );

    assert_eq!(lowered.diagnostics.len(), 1);
    assert_eq!(lowered.diagnostics[0].severity, Severity::Error);
    assert_eq!(lowered.diagnostics[0].line, 2);
    assert_eq!(
        lowered.diagnostics[0].message,
        "`import ... binary` requires `as name`"
    );
}

#[test]
fn rejects_non_child_local_import_requests_during_lowering() {
    for request in [
        "../parent.g",
        "/absolute.g",
        "C:/absolute.g",
        "./current.g",
        ".hidden.g",
        "lib/.hidden/child.g",
        "lib\\child.g",
    ] {
        let source = format!("language g0\nimport \"{request}\" as imported\n");
        let lowered =
            lower_parsed_source(parse_source(source.as_bytes()), &CompileContext::default());
        assert!(
            lowered.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error
                    && diagnostic.message.contains("local source request")
            }),
            "request `{request}` should report a lowering diagnostic: {:#?}",
            lowered.diagnostics
        );
    }
}

#[test]
fn parses_builtin_imports() {
    let parsed = parse("language g0\nimport 'std as std\nimport 'math\nimport 'list as list\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Import(ImportDecl {
            reference: ImportReference::Builtin("std".to_owned()),
            binary: false,
            placement: ImportPlacement::As("std".to_owned()),
        })
    );
    assert_eq!(
        parsed.declarations[2].kind,
        DeclarationKind::Import(ImportDecl {
            reference: ImportReference::Builtin("math".to_owned()),
            binary: false,
            placement: ImportPlacement::Inline,
        })
    );
}

#[test]
fn parses_unique_declarations() {
    let parsed = parse("language g0\nunique Foo, palette.Blue\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Unique(vec!["Foo".to_owned(), "palette.Blue".to_owned()])
    );
}

#[test]
fn parses_mixed_top_level_declarations() {
    let parsed = parse(concat!(
        "language g0 with demo\n",
        "import 'std as standard\n",
        "abstract missing, nested.value\n",
        "unique Marker\n",
        "object holder with\n",
        "  value = 41\n",
        "answer = holder.value + 1\n",
    ));

    assert_eq!(parsed.diagnostics, []);
    assert!(matches!(
        parsed.declarations[0].kind,
        DeclarationKind::Language(_)
    ));
    assert!(matches!(
        parsed.declarations[1].kind,
        DeclarationKind::Import(_)
    ));
    assert_eq!(
        parsed.declarations[2].kind,
        DeclarationKind::Abstract(vec!["missing".to_owned(), "nested.value".to_owned()])
    );
    assert_eq!(
        parsed.declarations[3].kind,
        DeclarationKind::Unique(vec!["Marker".to_owned()])
    );
    assert!(matches!(
        parsed.declarations[4].kind,
        DeclarationKind::Object(_)
    ));
    assert!(matches!(
        parsed.declarations[5].kind,
        DeclarationKind::Definition(_)
    ));
}

#[test]
fn malformed_simple_declarations_report_their_declaration_lines() {
    for (source, line) in [
        ("language\n", 1),
        ("language g0\nimport\n", 2),
        ("language g0\nabstract\n", 2),
        ("language g0\nunique\n", 2),
    ] {
        let parsed = parse(source);

        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.line == line
                    && diagnostic.message.starts_with("expected")),
            "{source:?} produced {:#?}",
            parsed.diagnostics
        );
        assert!(matches!(
            parsed.declarations.last().unwrap().kind,
            DeclarationKind::Unknown
        ));
    }
}

#[test]
fn parses_named_object_declarations() {
    let parsed = parse(
        "language g0\nobject child extends base, mixin with\n  text = \"Hello\"\n  target := \"World\"\n",
    );

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Object(ObjectDecl {
            target: "child".to_owned(),
            alias: None,
            deps: vec!["base".to_owned(), "mixin".to_owned()],
            body: vec![
                ObjectBodyDefinition {
                    line: 3,
                    kind: ObjectBodyDefinitionKind::Definition(DefinitionDecl {
                        target: static_definition_target("text"),
                        parameters: vec![],
                        kind: DefinitionKind::Introduce,
                        expr: Some(SyntaxExpr::Text("Hello".to_owned())),
                    }),
                },
                ObjectBodyDefinition {
                    line: 4,
                    kind: ObjectBodyDefinitionKind::Definition(DefinitionDecl {
                        target: static_definition_target("target"),
                        parameters: vec![],
                        kind: DefinitionKind::Override,
                        expr: Some(SyntaxExpr::Text("World".to_owned())),
                    }),
                },
            ],
        })
    );
}

#[test]
fn parses_extend_declarations() {
    let parsed =
        parse("language g0\nextend child with\n  text := _text ++ \"!\"\n  tail = \"done\"\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Extend(ObjectExtendDecl {
            target: "child".to_owned(),
            alias: None,
            body: vec![
                ObjectBodyDefinition {
                    line: 3,
                    kind: ObjectBodyDefinitionKind::Definition(DefinitionDecl {
                        target: static_definition_target("text"),
                        parameters: vec![],
                        kind: DefinitionKind::Override,
                        expr: Some(SyntaxExpr::Append(
                            Box::new(SyntaxExpr::PriorName("text".to_owned())),
                            Box::new(SyntaxExpr::Text("!".to_owned())),
                        )),
                    }),
                },
                ObjectBodyDefinition {
                    line: 4,
                    kind: ObjectBodyDefinitionKind::Definition(DefinitionDecl {
                        target: static_definition_target("tail"),
                        parameters: vec![],
                        kind: DefinitionKind::Introduce,
                        expr: Some(SyntaxExpr::Text("done".to_owned())),
                    }),
                },
            ],
        })
    );
}

#[test]
fn parses_hierarchical_object_body_declarations() {
    let parsed = parse(
        "language g0\nobject parent with\n  object child with\n    text = \"Hello\"\n  tail = \"done\"\n",
    );

    assert_eq!(parsed.diagnostics, []);
    let DeclarationKind::Object(parent) = &parsed.declarations[1].kind else {
        panic!("parent should parse as an object declaration");
    };
    assert_eq!(parent.body.len(), 2);
    let ObjectBodyDefinitionKind::Object(child) = &parent.body[0].kind else {
        panic!("first parent body item should parse as a nested object");
    };
    assert_eq!(child.target, "child");
    assert_eq!(child.body.len(), 1);
    assert!(child.body[0].definition().is_some());
}

#[test]
fn parses_object_expressions() {
    let parsed =
        parse("language g0\nhello = object \"hello\" as _h extends base with\n  text = h.target\n");

    assert_eq!(parsed.diagnostics, []);
    assert!(matches!(
        &parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            expr: Some(SyntaxExpr::Object(ObjectExpr {
                name: Some(_),
                alias: Some(alias),
                deps,
                body,
            })),
            ..
        }) if alias == "_h" && deps.len() == 1 && body.len() == 1
    ));
}

#[test]
fn object_declarations_require_named_targets() {
    let parsed = parse("language g0\nobject _ with\n  text = \"Hello\"\n");

    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].severity, Severity::Error);
    assert_eq!(parsed.diagnostics[0].line, 2);
    assert_eq!(
        parsed.diagnostics[0].message,
        "object declarations require a named target; use an object expression for an anonymous object"
    );
}

#[test]
fn parses_object_and_extend_aliases() {
    let parsed = parse(
        "language g0\nobject child as _c extends base with\n  text = c.base\nextend child as _c with\n  text := _c.text ++ \"!\"\n",
    );

    assert_eq!(parsed.diagnostics, []);
    match &parsed.declarations[1].kind {
        DeclarationKind::Object(object) => {
            assert_eq!(object.target, "child");
            assert_eq!(object.alias.as_deref(), Some("_c"));
            assert_eq!(object.deps, ["base".to_owned()]);
        }
        other => panic!("expected object declaration, got {other:?}"),
    }
    match &parsed.declarations[2].kind {
        DeclarationKind::Extend(extend) => {
            assert_eq!(extend.target, "child");
            assert_eq!(extend.alias.as_deref(), Some("_c"));
        }
        other => panic!("expected extend declaration, got {other:?}"),
    }
}

#[test]
fn object_declaration_aliases_follow_local_unused_warning_rules() {
    let parsed = parse(
        "language g0\nobject a as unused with\n  x = 1\nobject b as _unused with\n  x = 1\nobject c as _ with\n  x = 1\nobject d as used with\n  x = used.y\nextend d as update_unused with\n  x := 2\nextend d as _update_unused with\n  x := 3\n",
    );

    assert_eq!(
        parsed
            .diagnostics
            .iter()
            .filter(|diag| diag.message.contains("unused local"))
            .count(),
        2
    );
    assert!(parsed.diagnostics.iter().any(|diag| {
        diag.severity == Severity::Warning
            && diag.line == 2
            && diag.message == "unused local `unused`"
    }));
    assert!(parsed.diagnostics.iter().any(|diag| {
        diag.severity == Severity::Warning
            && diag.line == 10
            && diag.message == "unused local `update_unused`"
    }));
}

#[test]
fn reports_missing_language_declaration() {
    let parsed = parse("foo = 1\n");

    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].severity, Severity::Error);
}

#[test]
fn warns_on_inconsistent_line_endings() {
    let parsed = parse("language g0\r\nfoo = 1\n");

    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|diag| diag.message.contains("inconsistent line endings"))
    );
}

#[test]
fn definition_targets_retain_parsed_semantic_paths() {
    let parsed = parse("language g0\nroot.([1, 2] ++ [3]) := value\n");

    assert_eq!(parsed.diagnostics, []);
    let DeclarationKind::Definition(definition) = &parsed.declarations[1].kind else {
        panic!("expected a definition");
    };
    assert_eq!(
        definition.target,
        vec![
            SyntaxKeyExpr::Atom("root".to_owned()),
            SyntaxKeyExpr::PathIndex(Box::new(SyntaxExpr::Append(
                Box::new(SyntaxExpr::List(vec![
                    SyntaxExpr::Number(n(1)),
                    SyntaxExpr::Number(n(2)),
                ])),
                Box::new(SyntaxExpr::List(vec![SyntaxExpr::Number(n(3))])),
            ))),
        ]
    );
}

#[test]
fn parses_inline_text_literal_expressions() {
    let parsed = parse("language g0\nasm.result = \"Hello, World!\"\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("asm.result"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Text("Hello, World!".to_owned())),
        })
    );
}

#[test]
fn parses_atom_literal_expressions() {
    let parsed = parse("language g0\nanswer = 'deque\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("answer"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Atom("deque".to_owned())),
        })
    );
}

#[test]
fn parses_number_literals() {
    let parsed = parse(
        "language g0\nanswer = 42\nneg = _42\nhex = 0xc0de\nbits = 0b1011_1010\nscaled = 1.234e_7\nexact = 1/6\n",
    );

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("answer"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Number(n(42))),
        })
    );
    assert_eq!(
        parsed.declarations[2].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("neg"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Number(Number::parse("_42").unwrap())),
        })
    );
    assert_eq!(
        parsed.declarations[3].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("hex"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Number(Number::parse("0xc0de").unwrap())),
        })
    );
    assert_eq!(
        parsed.declarations[4].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("bits"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Number(Number::parse("0b1011_1010").unwrap())),
        })
    );
    assert_eq!(
        parsed.declarations[5].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("scaled"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Number(Number::parse("1.234e_7").unwrap())),
        })
    );
    assert_eq!(
        parsed.declarations[6].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("exact"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Divide(
                Box::new(SyntaxExpr::Number(n(1))),
                Box::new(SyntaxExpr::Number(n(6))),
            )),
        })
    );
}

#[test]
fn parses_list_and_append_expressions() {
    let parsed = parse("language g0\nbytes = [1, 2] ++ [3, 4]\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("bytes"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Append(
                Box::new(SyntaxExpr::List(vec![
                    SyntaxExpr::Number(n(1)),
                    SyntaxExpr::Number(n(2)),
                ])),
                Box::new(SyntaxExpr::List(vec![
                    SyntaxExpr::Number(n(3)),
                    SyntaxExpr::Number(n(4)),
                ])),
            )),
        })
    );
}

#[test]
fn parses_arithmetic_with_precedence() {
    let parsed = parse("language g0\nanswer = (1 + 2 * 3) - (4 / 5)\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("answer"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Subtract(
                Box::new(SyntaxExpr::Add(
                    Box::new(SyntaxExpr::Number(n(1))),
                    Box::new(SyntaxExpr::Multiply(
                        Box::new(SyntaxExpr::Number(n(2))),
                        Box::new(SyntaxExpr::Number(n(3))),
                    )),
                )),
                Box::new(SyntaxExpr::Divide(
                    Box::new(SyntaxExpr::Number(n(4))),
                    Box::new(SyntaxExpr::Number(n(5))),
                )),
            )),
        })
    );
}

#[test]
fn parses_name_and_append_expressions() {
    let parsed = parse("language g0\nasm.result = hello ++ \", \" ++ world ++ \"!\"\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("asm.result"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Append(
                Box::new(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::Append(
                        Box::new(SyntaxExpr::Name("hello".to_owned())),
                        Box::new(SyntaxExpr::Text(", ".to_owned())),
                    )),
                    Box::new(SyntaxExpr::Name("world".to_owned())),
                )),
                Box::new(SyntaxExpr::Text("!".to_owned())),
            )),
        })
    );
}

#[test]
fn parses_prior_name_expressions_only_at_name_roots() {
    let parsed = parse("language g0\nasm.result = _hello ++ _world.tail\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("asm.result"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Append(
                Box::new(SyntaxExpr::PriorName("hello".to_owned())),
                Box::new(SyntaxExpr::Access(
                    Box::new(SyntaxExpr::PriorName("world".to_owned())),
                    vec![SyntaxKeyExpr::Atom("tail".to_owned())],
                )),
            )),
        })
    );

    for expression in ["foo._bar", "_foo._bar"] {
        let parsed = parse(&format!("language g0\nvalue = {expression}\n"));
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error),
            "`{expression}` should be rejected"
        );
    }
}

#[test]
fn parses_lambda_and_application_expressions() {
    let parsed = parse("language g0\nasm.result = (\\x -> x) \"Hello\"\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("asm.result"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Lambda(
                    vec!["x".to_owned()],
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                )),
                Box::new(SyntaxExpr::Text("Hello".to_owned())),
            )),
        })
    );
}

#[test]
fn parses_local_root_paths_inside_lambda_bodies() {
    let parsed = parse("language g0\nasm.result = \\x -> x.tail\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("asm.result"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Lambda(
                vec!["x".to_owned()],
                Box::new(SyntaxExpr::Access(
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                    vec![SyntaxKeyExpr::Atom("tail".to_owned())],
                )),
            )),
        })
    );
}

#[test]
fn parses_explicit_lambda_underscore_local_conventions() {
    let parsed = parse("language g0\nasm.result = (\\ _value _ -> value) 1 2\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("asm.result"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Apply(
                    Box::new(SyntaxExpr::Lambda(
                        vec!["_value".to_owned(), "_".to_owned()],
                        Box::new(SyntaxExpr::Name("value".to_owned())),
                    )),
                    Box::new(SyntaxExpr::Number(n(1))),
                )),
                Box::new(SyntaxExpr::Number(n(2))),
            )),
        })
    );
}

#[test]
fn parses_definition_argument_sugar() {
    let parsed = parse("language g0\nid x = x\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("id"),
            parameters: vec!["x".to_owned()],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Lambda(
                vec!["x".to_owned()],
                Box::new(SyntaxExpr::Name("x".to_owned())),
            )),
        })
    );
}

#[test]
fn parses_update_definition_argument_sugar() {
    let parsed = parse("language g0\nid x ::= x\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("id"),
            parameters: vec!["x".to_owned()],
            kind: DefinitionKind::Update,
            expr: Some(SyntaxExpr::Lambda(
                vec!["x".to_owned()],
                Box::new(SyntaxExpr::Name("x".to_owned())),
            )),
        })
    );
}

#[test]
fn parameterized_definitions_parse_compound_bodies_before_lambda_wrapping() {
    let parsed = parse(concat!(
        "language g0\n",
        "from_let x = let y = x in y\n",
        "from_where x = y where y = x\n",
        "update x ::= let y = x in y\n",
    ));

    assert_eq!(parsed.diagnostics, []);
    for (index, expected_target, expected_kind) in [
        (1, "from_let", DefinitionKind::Introduce),
        (2, "from_where", DefinitionKind::Introduce),
        (3, "update", DefinitionKind::Update),
    ] {
        let DeclarationKind::Definition(definition) = &parsed.declarations[index].kind else {
            panic!("{expected_target} should be a definition");
        };
        assert_eq!(definition.target, static_definition_target(expected_target));
        assert_eq!(definition.parameters, ["x"]);
        assert_eq!(definition.kind, expected_kind);
        assert!(matches!(
            &definition.expr,
            Some(SyntaxExpr::Lambda(parameters, body))
                if parameters == &["x"] && matches!(body.as_ref(), SyntaxExpr::Let { .. })
        ));
    }
}

#[test]
fn parses_layout_do_in_definition_lambda_and_application_positions() {
    let parsed = parse(concat!(
        "language g0\n",
        "main api = do\n",
        "  .prepare api\n",
        "  .r api\n",
        "wrapped = \\api -> do\n",
        "  .r api\n",
        "identity_net = interaction_net do\n",
        "  .bind -> ports\n",
        "  .r ports\n",
    ));

    assert_eq!(parsed.diagnostics, []);
    let DeclarationKind::Definition(main) = &parsed.declarations[1].kind else {
        panic!("main should be a definition");
    };
    assert_eq!(main.parameters, ["api"]);
    assert!(matches!(
        &main.expr,
        Some(SyntaxExpr::Lambda(parameters, body))
            if parameters == &["api"] && matches!(body.as_ref(), SyntaxExpr::Do(_))
    ));

    let DeclarationKind::Definition(wrapped) = &parsed.declarations[2].kind else {
        panic!("wrapped should be a definition");
    };
    assert!(matches!(
        &wrapped.expr,
        Some(SyntaxExpr::Lambda(parameters, body))
            if parameters == &["api"] && matches!(body.as_ref(), SyntaxExpr::Do(_))
    ));

    let DeclarationKind::Definition(identity_net) = &parsed.declarations[3].kind else {
        panic!("identity_net should be a definition");
    };
    assert!(matches!(
        &identity_net.expr,
        Some(SyntaxExpr::Apply(_, argument))
            if matches!(argument.as_ref(), SyntaxExpr::Do(_))
    ));
}

#[test]
fn braced_do_evaluates_like_layout_do_and_supports_empty_blocks() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "braced = do { first <- .r 70; second = first + 2; .r second }\n",
        "nested = do { value <- do .r 72; .r value }\n",
        "empty = do {}\n",
        "commented = do {\n",
        "  # comments do not make an empty block non-empty\n",
        "}\n",
        "asm.braced = list.head (list.pure braced)\n",
        "asm.nested = list.head (list.pure nested)\n",
        "asm.empty = list.head (list.pure empty)\n",
        "asm.commented = list.head (list.pure commented)\n",
    ));
    assert_eq!(parsed.diagnostics, []);

    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);
    let value = evaluated_module_value(&context, &lowered);
    for path in ["braced", "nested"] {
        assert_eq!(
            fully_evaluated_value(resolved_value_at_path(&value, &["asm", path])),
            Value::Number(n(72)),
            "{path}"
        );
    }
    for path in ["empty", "commented"] {
        assert_eq!(
            fully_evaluated_value(resolved_value_at_path(&value, &["asm", path])),
            (*crate::core::keys::UNIT_VALUE).clone(),
            "{path}"
        );
    }
}

#[test]
fn do_bindings_follow_sequential_unused_local_rules() {
    let parsed = parse(concat!(
        "language g0\n",
        "outer prior = do\n",
        "  current <- .r prior\n",
        "  unused = current\n",
        "  _quiet <- .r current\n",
        "  .r current\n",
        "nested = do\n",
        "  outer <- .r ()\n",
        "  inner <- do\n",
        "    nested <- .r outer\n",
        "    .r nested\n",
        "  .r inner\n",
    ));

    let warnings = parsed
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Warning)
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].line, 4);
    assert_eq!(warnings[0].message, "unused local `unused`");
}

#[test]
fn recursive_do_forward_names_follow_unused_local_rules() {
    let parsed = parse(concat!(
        "language g0\n",
        "recursive = do\n",
        "  abstract unused, _quiet, used\n",
        "  reader = \\_ -> used\n",
        "  unused = 1\n",
        "  quiet = 2\n",
        "  used = 3\n",
        "  .r reader\n",
    ));

    let warnings = parsed
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Warning)
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].line, 3);
    assert_eq!(warnings[0].message, "unused local `unused`");
}

#[test]
fn recursive_do_reports_invalid_regions() {
    let cases = [
        (
            concat!(
                "language g0\n",
                "bad = do\n",
                "  abstract missing\n",
                "  .r ()\n",
            ),
            3,
            "no later fulfillment for `missing`",
        ),
        (
            concat!(
                "language g0\n",
                "bad = do\n",
                "  abstract value, _value\n",
                "  value = 1\n",
                "  .r value\n",
            ),
            3,
            "duplicate recursive do abstract declaration for `value`",
        ),
        (
            concat!(
                "language g0\n",
                "bad outer = do\n",
                "  abstract outer\n",
                "  outer = 1\n",
                "  .r outer\n",
            ),
            3,
            "local `outer` shadows existing local `outer`",
        ),
    ];

    for (source, line, expected) in cases {
        let parsed = parse(source);
        let lowered = lower_parsed_source(parsed, &CompileContext::default());
        assert!(lowered.diagnostics.iter().any(|diagnostic| {
            diagnostic.line == line && diagnostic.message.contains(expected)
        }));
    }
}

#[test]
fn recursive_do_uses_standard_fix_and_preserves_region_locals() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "single = do\n",
        "  abstract answer\n",
        "  reader = \\_ -> answer\n",
        "  .r 72 -> answer\n",
        "  .r (reader ())\n",
        "mutual = do\n",
        "  abstract left, right\n",
        "  captured = 72\n",
        "  left = \\_ -> right\n",
        "  right = captured\n",
        "  ((left () == 72) and (captured == 72)) =>> .r 72\n",
        "sequential = do\n",
        "  abstract first\n",
        "  first = 70\n",
        "  abstract second\n",
        "  second = first + 2\n",
        "  .r second\n",
        "direct_hierarchy = do\n",
        "  abstract outer\n",
        "  abstract inner\n",
        "  inner = 2\n",
        "  outer = inner + 70\n",
        "  .r outer\n",
        "independent = do\n",
        "  abstract _x, y, _z\n",
        "  early_y = \\_ -> y\n",
        "  y = 72\n",
        "  (early_y () == 72) =>> .r ()\n",
        "  x = 70\n",
        "  z = 74\n",
        "  .r y\n",
        "hierarchical = do\n",
        "  abstract outer\n",
        "  inner = do\n",
        "    abstract nested\n",
        "    nested = outer + 2\n",
        "    .r nested\n",
        "  outer = 70\n",
        "  inner\n",
        "asm.single = list.head (list.pure single)\n",
        "asm.mutual = list.head (list.pure mutual)\n",
        "asm.sequential = list.head (list.pure sequential)\n",
        "asm.direct_hierarchy = list.head (list.pure direct_hierarchy)\n",
        "asm.independent = list.head (list.pure independent)\n",
        "asm.hierarchical = list.head (list.pure hierarchical)\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for path in [
        "single",
        "mutual",
        "sequential",
        "direct_hierarchy",
        "independent",
        "hierarchical",
    ] {
        assert_eq!(
            fully_evaluated_value(resolved_value_at_path(&value, &["asm", path])),
            Value::Number(n(72)),
            "{path}"
        );
    }
}

#[test]
fn crossing_recursive_do_promotes_fix_scope_without_leaking_name_visibility() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "inner = 1\n",
        "crossing = do\n",
        "  abstract outer\n",
        "  prior = inner\n",
        "  abstract _inner\n",
        "  outer = prior\n",
        "  inner = 2\n",
        "  .r outer\n",
        "asm.crossing = list.head (list.pure crossing)\n",
    ));
    let warnings = parsed
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Warning)
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].line, 7);
    assert!(warnings[0].message.contains("`_inner`"));
    assert!(warnings[0].message.contains("begun on line 5"));

    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(
        lowered
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .count(),
        0
    );
    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["asm", "crossing"])),
        Value::Number(n(1))
    );
}

#[test]
fn recursive_do_strict_forward_observation_reports_the_fixpoint_cycle() {
    let source = concat!(
        "language g0\n",
        "import 'std\n",
        "task = do\n",
        "  abstract answer\n",
        "  (answer == 72) =>> .r ()\n",
        "  answer = 72\n",
        "  .r ()\n",
        "probe = anno { refl:task } \"unreachable\"\n",
    );
    let (_assembler, eval_context, definitions, _diagnostics) =
        reflection_test_module(source, &["recursive_do_cycle"], &[]);
    let mut probe = value_at_atom_path(&definitions, &["probe"]).expect("probe should exist");
    let error = loop {
        match crate::eval::eval_value(&eval_context, &probe) {
            Ok(next @ (Value::Lazy(_) | Value::Promised(_))) => probe = next,
            Ok(other) => panic!("strict recursive observation produced {other:?}"),
            Err(error) => break error.to_string(),
        }
    };
    assert!(
        error.contains("recursively observed itself"),
        "unexpected error: {error}"
    );
}

#[test]
fn recursive_do_without_fix_handler_fails_like_an_explicit_fix_request() {
    let parsed = parse(concat!(
        "language g0\n",
        "api = { r:(\\x -> x), seq:(\\op k -> (k (op.eff api)).eff api) }\n",
        "recursive = do\n",
        "  abstract answer\n",
        "  answer = 72\n",
        "  .r answer\n",
        "explicit = .fix (\\_future -> .r 72)\n",
        "asm.recursive = recursive.eff api\n",
        "asm.explicit = explicit.eff api\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let recursive = value_at_atom_path(&value, &["asm", "recursive"]).expect("result exists");
    let explicit = value_at_atom_path(&value, &["asm", "explicit"]).expect("result exists");
    assert_eq!(
        fully_evaluated_error(recursive).to_string(),
        fully_evaluated_error(explicit).to_string()
    );
}

#[test]
fn do_lowering_rejects_shadowing_at_the_binding_statement() {
    let parsed = parse(concat!(
        "language g0\n",
        "bad outer = do\n",
        "  inner <- .r outer\n",
        "  outer <- .r inner\n",
        "  .r outer\n",
    ));
    assert_eq!(parsed.diagnostics, []);

    let lowered = lower_parsed_source(parsed, &CompileContext::default());
    assert_eq!(lowered.diagnostics.len(), 1);
    assert_eq!(lowered.diagnostics[0].line, 4);
    assert!(
        lowered.diagnostics[0]
            .message
            .contains("local `outer` shadows existing local `outer`")
    );
}

#[test]
fn do_lowering_sequences_binds_value_guards_and_bare_operations() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "source_value = 70\n",
        "asm.backward = list.pure do\n",
        "  left <- .r \"A\"\n",
        "  right = \"B\"\n",
        "  .r ()\n",
        "  ((left == \"A\") and (right == \"B\")) =>> .r 65\n",
        "asm.forward = list.pure do\n",
        "  .r \"C\" -> left\n",
        "  .r \"D\" -> right\n",
        "  ((left == \"C\") and (right == \"D\")) =>> .r 67\n",
        "asm.drop = list.pure do\n",
        "  .r \"ignored\" -> _\n",
        "  .r 69\n",
        "asm.scope = list.pure do\n",
        "  source_value <- .r source_value\n",
        "  .r source_value\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("backward", b"A".as_slice()),
        ("forward", b"C".as_slice()),
        ("drop", b"E".as_slice()),
        ("scope", b"F".as_slice()),
    ] {
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", path]
            ))),
            expected,
            "{path}"
        );
    }
}

#[test]
fn bare_do_operations_reuse_the_effect_then_unit_policy() {
    let parsed = parse(concat!(
        "language g0\n",
        "api = { r:(\\x -> x), seq:(\\op k -> (k (op.eff api)).eff api) }\n",
        "bad = do\n",
        "  .r \"not unit\"\n",
        "  .r \"unreachable\"\n",
        "asm.result = bad.eff api\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let result = value_at_atom_path(&value, &["asm", "result"]).expect("result should exist");
    assert!(
        fully_evaluated_error(result)
            .to_string()
            .contains("requires discarded effect results to be unit")
    );
}

#[test]
fn warns_on_unused_locals_without_underscore_prefix() {
    let parsed = parse("language g0\nid x = 42\nasm.result = (\\y -> \"ok\") 1\n");

    assert!(parsed.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Warning
            && diagnostic.line == 2
            && diagnostic.message.contains("unused local `x`")
    }));
    assert!(parsed.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Warning
            && diagnostic.line == 3
            && diagnostic.message.contains("unused local `y`")
    }));
}

#[test]
fn let_bindings_follow_local_unused_warning_rules() {
    let parsed = parse("language g0\nasm.result = let unused = 1; _suppressed = 2; _ = 3 in 4\n");

    let warnings = parsed
        .diagnostics
        .iter()
        .filter(|diag| diag.message.contains("unused local"))
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].message, "unused local `unused`");
}

#[test]
fn underscore_locals_suppress_unused_warnings_and_drop_is_allowed() {
    let parsed = parse("language g0\nkeep _value = value\nskip _ y = y\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("keep"),
            parameters: vec!["_value".to_owned()],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Lambda(
                vec!["_value".to_owned()],
                Box::new(SyntaxExpr::Name("value".to_owned())),
            )),
        })
    );
    assert_eq!(
        parsed.declarations[2].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("skip"),
            parameters: vec!["_".to_owned(), "y".to_owned()],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Lambda(
                vec!["_".to_owned(), "y".to_owned()],
                Box::new(SyntaxExpr::Name("y".to_owned())),
            )),
        })
    );
}

#[test]
fn lowering_rejects_duplicate_and_nested_local_shadowing() {
    let parsed = parse(concat!(
        "language g0\n",
        "duplicate x x = x\n",
        "nested x = (\\x -> x)\n",
        "nested_let = let x = 1 in let x = 2 in x\n",
        "suppressed x _x = x\n",
    ));
    let lowered = lower_parsed_source(parsed, &CompileContext::default());
    let errors = lowered
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect::<Vec<_>>();

    assert_eq!(errors.len(), 4);
    assert_eq!(errors[0].line, 2);
    assert!(
        errors[0]
            .message
            .contains("local `x` shadows existing local `x`")
    );
    assert_eq!(errors[1].line, 3);
    assert!(
        errors[1]
            .message
            .contains("local `x` shadows existing local `x`")
    );
    assert_eq!(errors[2].line, 4);
    assert!(
        errors[2]
            .message
            .contains("local `x` shadows existing local `x`")
    );
    assert_eq!(errors[3].line, 5);
    assert!(
        errors[3]
            .message
            .contains("local `_x` shadows existing local `x`")
    );
}

#[test]
fn lowering_allows_local_reuse_in_disjoint_scopes_and_repeated_drops() {
    let parsed = parse(concat!(
        "language g0\n",
        "left = (\\x -> x) 1\n",
        "right = (\\x -> x) 2\n",
        "drop_both _ _ = ()\n",
    ));
    let lowered = lower_parsed_source(parsed, &CompileContext::default());

    assert!(
        lowered
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error),
        "unexpected diagnostics: {:?}",
        lowered.diagnostics
    );
}

#[test]
fn parses_dictionary_literals() {
    let parsed = parse("language g0\nd = { hello:\"Hello\", world:\"World\" }\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("d"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::DictUnion(vec![
                SyntaxExpr::PathDict(
                    vec![SyntaxKeyExpr::Atom("hello".to_owned())],
                    Box::new(SyntaxExpr::Text("Hello".to_owned())),
                ),
                SyntaxExpr::PathDict(
                    vec![SyntaxKeyExpr::Atom("world".to_owned())],
                    Box::new(SyntaxExpr::Text("World".to_owned())),
                ),
            ])),
        })
    );
}

#[test]
fn parses_dictionary_unions() {
    let parsed = parse("language g0\nd = { left, right, hello:\"Hello\" }\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("d"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::DictUnion(vec![
                SyntaxExpr::Name("left".to_owned()),
                SyntaxExpr::Name("right".to_owned()),
                SyntaxExpr::PathDict(
                    vec![SyntaxKeyExpr::Atom("hello".to_owned())],
                    Box::new(SyntaxExpr::Text("Hello".to_owned())),
                ),
            ])),
        })
    );
}

#[test]
fn parses_multiline_literals_with_leading_commas() {
    let parsed = parse(
        "language g0\nnums = [\n  , 1\n  , 2\n  ]\nd = {\n  , hello:\"Hello\"\n  , world:\"World\"\n  }\n",
    );

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("nums"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::List(vec![
                SyntaxExpr::Number(n(1)),
                SyntaxExpr::Number(n(2)),
            ])),
        })
    );
    assert_eq!(
        parsed.declarations[2].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("d"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::DictUnion(vec![
                SyntaxExpr::PathDict(
                    vec![SyntaxKeyExpr::Atom("hello".to_owned())],
                    Box::new(SyntaxExpr::Text("Hello".to_owned())),
                ),
                SyntaxExpr::PathDict(
                    vec![SyntaxKeyExpr::Atom("world".to_owned())],
                    Box::new(SyntaxExpr::Text("World".to_owned())),
                ),
            ])),
        })
    );
}

#[test]
fn parses_expression_indexed_names_and_keys() {
    let parsed = parse("language g0\nd = { [42]:\"World\" }\nasm.result = d.[42] ++ d.['tail]\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("d"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::DictUnion(vec![SyntaxExpr::PathDict(
                vec![SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(42))))],
                Box::new(SyntaxExpr::Text("World".to_owned())),
            )])),
        })
    );
    assert_eq!(
        parsed.declarations[2].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("asm.result"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Append(
                Box::new(SyntaxExpr::Access(
                    Box::new(SyntaxExpr::Name("d".to_owned())),
                    vec![SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(42))))],
                )),
                Box::new(SyntaxExpr::Access(
                    Box::new(SyntaxExpr::Name("d".to_owned())),
                    vec![SyntaxKeyExpr::Atom("tail".to_owned())],
                )),
            )),
        })
    );
}

#[test]
fn parses_path_list_shorthand_and_general_list_path_exprs() {
    let parsed = parse("language g0\nasm.result = foo.[1,2,3] ++ foo.([1,2] ++ [3])\n");

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("asm.result"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: Some(SyntaxExpr::Append(
                Box::new(SyntaxExpr::Access(
                    Box::new(SyntaxExpr::Name("foo".to_owned())),
                    vec![
                        SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(1)))),
                        SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(2)))),
                        SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(3)))),
                    ],
                )),
                Box::new(SyntaxExpr::Access(
                    Box::new(SyntaxExpr::Name("foo".to_owned())),
                    vec![SyntaxKeyExpr::PathIndex(Box::new(SyntaxExpr::Append(
                        Box::new(SyntaxExpr::List(vec![
                            SyntaxExpr::Number(n(1)),
                            SyntaxExpr::Number(n(2)),
                        ])),
                        Box::new(SyntaxExpr::List(vec![SyntaxExpr::Number(n(3))])),
                    )))],
                )),
            )),
        })
    );
}

#[test]
fn reports_ambiguous_slash_chains_as_parse_errors() {
    let parsed = parse("language g0\nasm.result = 3/4/5\n");

    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|diag| diag.line == 2 && diag.message.contains("non-associative"))
    );
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("asm.result"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: None,
        })
    );
}

#[test]
fn reports_ambiguous_subtract_chains_as_parse_errors() {
    let parsed = parse("language g0\nasm.result = 3 - 4 - 5\n");

    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|diag| diag.line == 2 && diag.message.contains("operator `-` is non-associative"))
    );
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("asm.result"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: None,
        })
    );
}

#[test]
fn reports_mixed_add_subtract_chains_as_parse_errors() {
    let parsed = parse("language g0\nasm.result = 3 + 1 - 4 + 1 - 5 + 1\n");

    assert!(parsed.diagnostics.iter().any(|diag| {
        diag.line == 2
            && diag
                .message
                .contains("operators `+` and `-` have no precedence relationship")
    }));
    assert_eq!(
        parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            target: static_definition_target("asm.result"),
            parameters: vec![],
            kind: DefinitionKind::Introduce,
            expr: None,
        })
    );
}

#[test]
fn reports_mixed_pipe_and_composition_directions_as_parse_errors() {
    let parsed = parse(
        "language g0\npipe = value |> f <| g\ncompose = f >> g << h\napplicative = op !> function <! argument\n",
    );

    assert!(parsed.diagnostics.iter().any(|diag| {
        diag.line == 2
            && diag
                .message
                .contains("operators `|>` and `<|` have no precedence relationship")
    }));
    assert!(parsed.diagnostics.iter().any(|diag| {
        diag.line == 3
            && diag
                .message
                .contains("operators `>>` and `<<` have no precedence relationship")
    }));
    assert!(parsed.diagnostics.iter().any(|diag| {
        diag.line == 4
            && diag
                .message
                .contains("operators `!>` and `<!` have no precedence relationship")
    }));
}

#[test]
fn lowers_list_expressions_to_core_terms() {
    let parsed = parse("language g0\nasm.result = [72, 101] ++ [108, 108, 111]\n");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello"
    );
}

#[test]
fn quoted_paths_lower_to_ordinary_path_lists() {
    let parsed =
        parse("language g0\nasm.result = { foo:{ [42]:{ bar:\"quoted\" } } }.('.foo.([42]).bar)\n");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"quoted"
    );
}

#[test]
fn lowers_name_expressions_to_core_terms() {
    let parsed = parse(
        "language g0\nasm.result = hello ++ \", \" ++ world ++ \"!\"\nhello = \"Hello\"\nworld = \"World\"\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn lowers_prior_name_expressions_to_visible_module_accesses() {
    let parsed = parse("language g0\nasm.result = _hello ++ \"!\"\n");
    let context = CompileContext::default().with_prior_defs(Value::Dict(
        crate::core::Dict::new_sync().insert(
            Key::atom_from_text("hello"),
            Value::binary_from_text("Hello"),
        ),
    ));
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello!"
    );
}

#[test]
fn object_declarations_evaluate_as_object_instances() {
    let parsed = parse(
        "language g0\nobject hello with\n  text = \"Hello, World!\"\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn object_expressions_evaluate_as_object_instances() {
    let parsed = parse(
        "language g0\nhello = object \"hello\" with\n  text = \"Hello, World!\"\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn object_dependencies_apply_inherited_defs_to_child_self() {
    let parsed = parse(
        "language g0\nobject base with\n  text = hello ++ \", \" ++ target ++ \"!\"\n  hello = \"Hello\"\n  target = \"Base\"\nobject child extends base with\n  target := \"World\"\nasm.result = child.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn object_expressions_can_extend_other_object_expressions() {
    let parsed = parse(
        "language g0\nbase = object \"base\" with\n  text = hello ++ \", \" ++ target ++ \"!\"\n  hello = \"Hello\"\n  target = \"Base\"\nhello = object \"hello\" extends base with\n  target := \"World\"\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn object_expression_aliases_default_to_parent_scope() {
    let parsed = parse(
        "language g0\nprefix = \"Hello\"\nhello = object \"hello\" as _h with\n  target = \"World\"\n  text = prefix ++ \", \" ++ h.target ++ \"!\"\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn self_and_module_keywords_resolve_at_module_scope() {
    let parsed = parse(
        "language g0\nhello = \"Hello\"\nworld = \"World\"\nasm.result = self.hello ++ \", \" ++ module.world ++ \"!\"\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn prior_self_and_module_keywords_resolve_at_module_scope() {
    let parsed = parse(
        "language g0\nhello = \"Hello\"\nworld = \"World\"\nhello := _self.hello ++ \", \" ++ _module.world ++ \"!\"\nasm.result = hello\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn self_and_module_keywords_resolve_inside_aliased_object_scope() {
    let parsed = parse(
        "language g0\nprefix = \"Hello\"\nobject hello as self with\n  prefix = \"Nope\"\n  target = \"World\"\n  text = module.prefix ++ \", \" ++ self.target ++ \"!\"\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn prior_self_keyword_resolves_inside_object_scope() {
    let parsed = parse(
        "language g0\nobject hello with\n  text = \"Hello, World\"\n  text := _self.text ++ \"!\"\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn object_prior_names_resolve_against_inherited_self() {
    let parsed = parse(
        "language g0\nobject base with\n  text = \"Hello, World\"\nobject child extends base with\n  text := _text ++ \"!\"\nasm.result = child.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn object_dependencies_use_c3_deduplication() {
    let parsed = parse(
        "language g0\nobject root with\n  code = \"root\"\nobject left extends root with\n  code := _code ++ \"L\"\nobject right extends root with\n  code := _code ++ \"R\"\nobject child extends left, right with\n  code := _code ++ \"C\"\nasm.result = child.code\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"rootRLC"
    );
}

#[test]
fn object_dependencies_can_inherit_from_dictionaries() {
    let parsed = parse(
        "language g0\nbase = { hello:\"Hello\", target:\"Base\" }\nobject child extends base with\n  target := \"World\"\n  text = hello ++ \", \" ++ target ++ \"!\"\nasm.result = child.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn module_definitions_can_use_expression_indexed_targets() {
    let parsed = parse(
        "language g0\nidx = 42\n.[idx] = \"Hello\"\nasm.result = module.[idx] ++ \", World!\"\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn module_definitions_can_use_path_list_targets() {
    let parsed = parse(
        "language g0\nroot.(['hello, 'target]) = \"World\"\nroot.hello.prefix = \"Hello\"\nasm.result = root.hello.prefix ++ \", \" ++ root.hello.target ++ \"!\"\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn object_definitions_can_use_expression_indexed_targets() {
    let parsed = parse(
        "language g0\nidx = 42\nobject hello as self with\n  .[idx] = \"Hello\"\n  text = self.[idx] ++ \", World!\"\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn hierarchical_object_declarations_evaluate_in_host_scope() {
    let parsed = parse(
        "language g0\nobject parent with\n  prefix = \"Hello\"\n  object child with\n    text = ^prefix ++ \", World!\"\nasm.result = parent.child.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn hierarchical_object_declarations_can_extend_sibling_objects() {
    let parsed = parse(
        "language g0\nobject parent with\n  object base with\n    text = \"Hello, World\"\n  object child extends base with\n    text := _text ++ \"!\"\nasm.result = parent.child.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn hierarchical_object_declarations_inside_extend_evaluate_in_host_scope() {
    let parsed = parse(
        "language g0\nobject parent with\n  prefix = \"Hello\"\nextend parent with\n  object child with\n    text = ^prefix ++ \", World!\"\nasm.result = parent.child.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn hierarchical_object_names_are_local_to_the_host_object() {
    let parsed = parse(
        "language g0\nobject left with\n  object helper with\n    left = \"Hello\"\nobject right with\n  object helper with\n    right = \"World\"\nobject child extends left.helper, right.helper with\n  text = left ++ \", \" ++ right ++ \"!\"\nasm.result = child.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn repeated_anonymous_object_mixins_are_not_deduplicated() {
    let parsed = parse(
        "language g0\nobject base with\n  count = 0\ninc = object _ with\n  count := _count + 1\nobject child extends inc, inc, inc, base with\nasm.result = child.count\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["asm", "result"])),
        Value::Number(3.into())
    );
}

#[test]
fn anonymous_object_dependencies_can_have_anonymous_and_named_dependencies() {
    let parsed = parse(
        "language g0\nobject base with\n  code = \"B\"\nadd_a = object _ with\n  code := _code ++ \"A\"\nadd_m = object _ extends add_a, base with\n  code := _code ++ \"M\"\nobject child extends add_m with\n  code := _code ++ \"C\"\nasm.result = child.code\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"BAMC"
    );
}

#[test]
fn anonymous_object_dependencies_follow_dependency_override_order() {
    let parsed = parse(
        "language g0\nobject base with\n  code = \"\"\nadd_a = object _ with\n  code := _code ++ \"A\"\nadd_b = object _ with\n  code := _code ++ \"B\"\nobject child extends add_a, add_b, base with\nasm.result = child.code\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"BA"
    );
}

#[test]
fn extend_declarations_reinstantiate_objects() {
    let parsed = parse(
        "language g0\nobject hello with\n  text = \"Hello, World\"\nextend hello with\n  text := _text ++ \"!\"\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn object_body_edits_do_not_observe_direct_spec_definitions() {
    let parsed = parse(
        "language g0\nobject hello with\n  spec = { bad:\"bad\" }\n  text = { [{}]:\"Hello, World!\" }.[_spec]\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn object_bodies_can_escape_to_module_scope() {
    let parsed = parse(
        "language g0\nprefix = \"Hello\"\nseparator = \", \"\nobject hello with\n  target = \"World\"\n  text = ^(prefix ++ separator) ++ target ++ \"!\"\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn excessive_scope_escapes_report_lowering_errors() {
    let parsed = parse("language g0\nasm.result = ^foo\nobject hello with\n  text = ^^foo\n");
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);

    assert_eq!(lowered.diagnostics.len(), 2);
    assert!(lowered.diagnostics.iter().all(|diag| {
        diag.severity == Severity::Error && diag.message.contains("exceeds available parent scopes")
    }));
}

#[test]
fn aliased_object_bodies_default_to_module_scope() {
    let parsed = parse(
        "language g0\nprefix = \"Hello\"\nobject hello as h with\n  target = \"World\"\n  text = prefix ++ \", \" ++ h.target ++ \"!\"\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn suppressed_object_aliases_still_bind_canonical_names() {
    let parsed = parse(
        "language g0\nprefix = \"Hello\"\nobject hello as _h with\n  target = \"World\"\n  text = prefix ++ \", \" ++ h.target ++ \"!\"\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn aliased_extend_bodies_can_reference_prior_object_and_module_scope() {
    let parsed = parse(
        "language g0\nsuffix = \"!\"\nobject hello with\n  text = \"Hello, World\"\nextend hello as h with\n  text := _h.text ++ suffix\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn suppressed_extend_aliases_still_bind_canonical_prior_names() {
    let parsed = parse(
        "language g0\nsuffix = \"!\"\nobject hello with\n  text = \"Hello, World\"\nextend hello as _h with\n  text := _h.text ++ suffix\nasm.result = hello.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn dictionary_with_without_alias_uses_parent_scope() {
    let parsed = parse(
        "language g0\nhello = \"Hello\"\nworld = \"World\"\nd = { hello:\"Nope\", world:\"Nope\" } with\n  hello := \"Still Nope\"\n  text = hello ++ \", \" ++ world ++ \"!\"\nasm.result = d.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn dictionary_with_aliases_capture_prior_and_final_dictionaries() {
    let parsed = parse(
        "language g0\nsuffix = \"!\"\nbase = { text:\"Hello, World\" }\nd = base as b with\n  text := _b.text ++ suffix\n  result = b.text\nasm.result = d.result\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn dictionary_with_suppressed_aliases_still_bind_canonical_names() {
    let parsed = parse(
        "language g0\nsuffix = \"!\"\nbase = { text:\"Hello, World\" }\nd = base as _b with\n  text := _b.text ++ suffix\n  result = b.text\nasm.result = d.result\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn dictionary_with_aliases_follow_local_unused_warning_rules() {
    let parsed =
        parse("language g0\nd = {} as unused with\n  x = 1\ne = {} as _unused with\n  x = 1\n");

    assert_eq!(
        parsed
            .diagnostics
            .iter()
            .filter(|diag| diag.message.contains("unused local"))
            .count(),
        1
    );
    assert!(parsed.diagnostics.iter().any(|diag| {
        diag.severity == Severity::Warning
            && diag.line == 2
            && diag.message == "unused local `unused`"
    }));
}

#[test]
fn dictionary_with_self_alias_uses_object_style_scope() {
    let parsed = parse(
        "language g0\nsuffix = \"!\"\nbase = { text:\"Hello, World\" }\nd = base as self with\n  text := _text ++ ^suffix\nasm.result = d.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn lowers_lambda_and_application_expressions_to_core_terms() {
    let parsed =
        parse("language g0\nd = { tail:\"Hello, World!\" }\nasm.result = (\\x -> x.tail) d\n");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn lowers_multi_argument_lambda_to_one_curried_net() {
    let parsed =
        parse("language g0\nfirst = \\x _y _z -> x\nasm.result = first \"Hello, World!\" {} {}\n");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn front_end_closure_conversion_preserves_nested_captures() {
    let parsed = parse(
        "language g0\nmake = \\x -> \\_ignored -> x\nasm.result = make \"Hello, World!\" {}\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn lowers_let_expressions_to_lambda_application() {
    let parsed = parse(
        "language g0\nasm.result = let hello = \"Hello\"; world = \"World\" in hello ++ \", \" ++ world ++ \"!\"\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn lowers_where_expressions_to_lambda_application() {
    let parsed = parse(
        "language g0\nasm.result = hello ++ \", \" ++ world ++ \"!\" where hello = \"Hello\"; world = \"World\"\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn lowers_multiline_let_expressions_to_lambda_application() {
    let parsed = parse(
        "language g0\nasm.result =\n  let hello = \"Hello\"\n      world = \"World\"\n  hello ++ \", \" ++ world ++ \"!\"\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn effect_shorthand_builds_applicable_effect_values() {
    let parsed = parse(
        "language g0\napi = { emit:(\\x -> x ++ \"!\") }\neffect = .emit \"Hi\"\nasm.result = effect.eff api\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hi!"
    );
}

#[test]
fn method_objects_apply_via_apply_member_from_syntax() {
    let parsed =
        parse("language g0\nmethod = { apply:(\\x -> x ++ \"!\") }\nasm.result = method \"Hi\"\n");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hi!"
    );
}

#[test]
fn operator_sections_evaluate_as_curried_functions() {
    let parsed = parse(
        "language g0\nadd_answer = (+ 42)\nsub_from_answer = (42 -)\nadd = (+)\nappend = (++)\nasm.sum = add_answer 8\nasm.diff = sub_from_answer 8\nasm.full_sum = add 8 42\nasm.full_append = append \"Hello\" \"!\"\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["asm", "sum"])),
        Value::Number(n(50))
    );
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["asm", "diff"])),
        Value::Number(n(34))
    );
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["asm", "full_sum"])),
        Value::Number(n(50))
    );
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "full_append"]
        ))),
        b"Hello!"
    );
}

#[test]
fn pipe_and_composition_operators_evaluate_as_syntax_sugar() {
    let parsed = parse(
        "language g0\nid x = x\nbang x = x ++ \"!\"\nhello = \"Hello\"\npipe_section = (|> bang)\npipe_function = (|>)\ncompose_section = (>> bang)\ncompose_function = (>>)\nasm.pipe_forward = hello |> bang\nasm.pipe_backward = bang <| hello\nasm.compose_forward = (id >> bang) hello\nasm.compose_backward = (bang << id) hello\nasm.pipe_section = pipe_section hello\nasm.pipe_function = pipe_function hello bang\nasm.compose_section = (compose_section id) hello\nasm.compose_function = compose_function id bang hello\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for path in [
        "pipe_forward",
        "pipe_backward",
        "compose_forward",
        "compose_backward",
        "pipe_section",
        "pipe_function",
        "compose_section",
        "compose_function",
    ] {
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", path]
            ))),
            b"Hello!",
            "{path}"
        );
    }
}

#[test]
fn effect_operators_evaluate_as_syntax_sugar() {
    let parsed = parse(
        "language g0\napi = { r:(\\x -> x), seq:(\\op k -> (k (op.eff api)).eff api) }\nop = .r \"Hello\"\nk x = .r (x ++ \", World!\")\nf x = .r (x ++ \", World\")\ng x = .r (x ++ \"!\")\nop_unit = .r ()\nbind_section = (>>= k)\nbind_function = (>>=)\nthen_function = (=>>)\nkleisli_function = (>=>)\nasm.bind = (op >>= k).eff api\nasm.bind_section = (bind_section op).eff api\nasm.bind_function = (bind_function op k).eff api\nasm.kleisli = ((f >=> g) \"Hello\").eff api\nasm.kleisli_function = (kleisli_function f g \"Hello\").eff api\nasm.then = (op_unit =>> .r \"Hello, World!\").eff api\nasm.then_function = (then_function op_unit (.r \"Hello, World!\")).eff api\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for path in [
        "bind",
        "bind_section",
        "bind_function",
        "kleisli",
        "kleisli_function",
        "then",
        "then_function",
    ] {
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", path]
            ))),
            b"Hello, World!",
            "{path}"
        );
    }
}

#[test]
fn applicative_operators_apply_and_sequence_in_source_order() {
    let parsed = parse(concat!(
        "language g0\n",
        "api = { r:(\\value -> {value:value, trace:\"R\"}), seq:(\\operation continuation -> (\\first -> (\\second -> {value:second.value, trace:first.trace ++ second.trace}) ((continuation first.value).eff api)) (operation.eff api)) }\n",
        "marked tag value = {eff:(\\_api -> {value:value, trace:tag})}\n",
        "backward = (marked \"F\" (\\x -> x ++ \"!\") <! marked \"A\" \"Hello\").eff api\n",
        "forward = (marked \"A\" \"Hello\" !> marked \"F\" (\\x -> x ++ \"!\")).eff api\n",
        "backward_chain = (.r (\\x y -> x ++ y) <! marked \"A\" \"A\" <! marked \"B\" \"B\").eff api\n",
        "forward_chain = (marked \"A\" \"A\" !> marked \"B\" \"B\" !> .r (\\y x -> x ++ y)).eff api\n",
        "asm.backward_value = backward.value\n",
        "asm.backward_trace = backward.trace\n",
        "asm.forward_value = forward.value\n",
        "asm.forward_trace = forward.trace\n",
        "asm.backward_chain = backward_chain.value\n",
        "asm.forward_chain = forward_chain.value\n",
    ));
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("backward_value", b"Hello!".as_slice()),
        ("backward_trace", b"FAR".as_slice()),
        ("forward_value", b"Hello!".as_slice()),
        ("forward_trace", b"AFR".as_slice()),
        ("backward_chain", b"AB".as_slice()),
        ("forward_chain", b"AB".as_slice()),
    ] {
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", path]
            ))),
            expected,
            "{path}"
        );
    }
}

#[test]
fn effect_then_requires_unit_result_when_observed() {
    let parsed = parse(
        "language g0\napi = { r:(\\x -> x), seq:(\\op k -> (k (op.eff api)).eff api) }\nbad = .r \"not unit\" =>> .r \"unreachable\"\nasm.result = bad.eff api\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let mut result = value_at_atom_path(&value, &["asm", "result"]).expect("result should exist");
    let err = loop {
        match crate::eval::eval_value(&test_eval_context(), &result) {
            Ok(next @ (Value::Lazy(_) | Value::Promised(_))) => result = next,
            Ok(other) => panic!("non-unit result should not evaluate to {other:?}"),
            Err(err) => break err,
        }
    };
    assert!(
        err.to_string()
            .contains("requires discarded effect results to be unit")
    );
}

#[test]
fn comparisons_and_boolean_operators_evaluate_as_effects() {
    let parsed = parse(
        "language g0\nimport 'std\ntuple_left = { tuple:[1,2] }\ntuple_right = { tuple:[1,3] }\nasm.gt = list.pure ((3 > 2) =>> .r \"G\")\nasm.ge = list.pure ((3 >= 3) =>> .r \"E\")\nasm.eq = list.pure ((3 == 3) =>> .r \"Q\")\nasm.ne = list.pure ((3 <> 4) =>> .r \"N\")\nasm.le = list.pure ((3 =< 3) =>> .r \"L\")\nasm.lt = list.pure ((2 < 3) =>> .r \"T\")\nasm.fail = list.pure ((3 < 2) =>> .r \"bad\")\nasm.chain = list.pure ((1 < 2 =< 2 <> 3) =>> .r \"H\")\nasm.chain_fail = list.pure ((1 < 3 < 2) =>> .r \"bad\")\nasm.chain_raw = 1 < (2 + 0) < 3\nasm.list = list.pure (([1,2] < [1,3]) =>> .r \"S\")\nasm.binary_list = list.pure ((\"AB\" == [65,66]) =>> .r \"B\")\nasm.string_list = list.pure (([\"A\",\"B\"] == [\"A\",\"B\"]) =>> .r \"V\")\nasm.string_order = list.pure (([\"A\",\"B\"] < [\"A\",\"C\"]) =>> .r \"W\")\nasm.nested_list = list.pure (([\"A\",\"B\"] <> \"AB\") =>> .r \"X\")\nasm.list_tuple = list.pure (([1,2] <> tuple_left) =>> .r \"Y\")\nasm.tuple = list.pure ((tuple_left < tuple_right) =>> .r \"U\")\nasm.dict = list.pure (({ a:1, b:{} } == { a:1 }) =>> .r \"D\")\nasm.and = list.pure ((3 > 2 and \"A\" == [65]) =>> .r \"A\")\nasm.or = list.pure ((3 < 2 or 3 == 3) =>> .r \"O\")\nasm.not_true = list.pure ((not (3 > 2)) =>> .r \"bad\")\nasm.not_false = list.pure ((not (3 < 2)) =>> .r \"F\")\nasm.could_true = list.pure ((could (.alt .fail (3 == 3))) =>> .r \"C\")\nasm.could_false = list.pure ((could .fail) =>> .r \"bad\")\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("gt", b"G".as_slice()),
        ("ge", b"E"),
        ("eq", b"Q"),
        ("ne", b"N"),
        ("le", b"L"),
        ("lt", b"T"),
        ("fail", b""),
        ("chain", b"H"),
        ("chain_fail", b""),
        ("list", b"S"),
        ("binary_list", b"B"),
        ("string_list", b"V"),
        ("string_order", b"W"),
        ("nested_list", b"X"),
        ("list_tuple", b"Y"),
        ("tuple", b"U"),
        ("dict", b"D"),
        ("and", b"A"),
        ("or", b"O"),
        ("not_true", b""),
        ("not_false", b"F"),
        ("could_true", b"C"),
        ("could_false", b""),
    ] {
        assert_eq!(
            output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", path]
            ))),
            expected,
            "{path}"
        );
    }

    value_at_atom_path(&value, &["asm", "chain_raw"]).expect("raw chain should exist");
}

#[test]
fn list_effect_handler_runs_standard_backtracking_effects() {
    let parsed = parse(
        "language g0\nimport 'list as list\nchoices = (.alt (.r \"A\") (.alt .fail (.r \"B\"))) >>= (\\x -> .r (x ++ \"!\"))\ncut = .cut (.alt (.r \"C\") (.r \"D\"))\ncut_bad = .cut (.alt (.r \"G\") 42)\ncut_seq_bad = .cut ((.alt (.r \"S\") 42) >>= (\\x -> .r (x ++ \"!\")))\nobject_effect = { eff:(.r \"E\").eff, meta:1 }\nfixed = .fix (\\self -> .r { text:\"F\", self:self })\nasm.choices = list.pure choices\nasm.cut = list.pure cut\nasm.cut_fail = list.pure (.cut .fail)\nasm.cut_bad = list.pure cut_bad\nasm.cut_seq_bad = list.pure cut_seq_bad\nasm.object = list.pure object_effect\nasm.fixed = (list.head (list.pure fixed)).text\nasm.head = list.head \"Hi\"\nasm.tail = list.tail \"Hi\"\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "choices"]
        ))),
        b"A!B!"
    );
    assert_eq!(
        output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "cut"]
        ))),
        b"C"
    );
    assert_eq!(
        output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "cut_fail"]
        ))),
        b""
    );
    assert_eq!(
        output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "cut_bad"]
        ))),
        b"G"
    );
    assert_eq!(
        output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "cut_seq_bad"]
        ))),
        b"S!"
    );
    assert_eq!(
        output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "object"]
        ))),
        b"E"
    );
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "fixed"]
        ))),
        b"F"
    );
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["asm", "head"])),
        Value::Number(n(72))
    );
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "tail"]
        ))),
        b"i"
    );
}

#[test]
fn operator_section_operands_resolve_module_scope_names() {
    let parsed = parse(
        "language g0\nsuffix = \"!\"\nadd_suffix = (++ suffix)\nasm.result = add_suffix \"Hello\"\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello!"
    );
}

#[test]
fn lowers_definition_argument_sugar_to_lambda_terms() {
    let parsed = parse("language g0\nid x = x\nasm.result = id \"Hello, World!\"\n");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn update_definition_argument_sugar_applies_body_to_prior_definition() {
    let parsed = parse(
        "language g0\nhello who = \"Hello, \" ++ who\nhello who ::= \\prior -> prior who ++ \"!\"\nasm.result = hello \"World\"\n",
    );
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn prior_names_observe_prior_module_state_within_current_module_scope() {
    let parsed = parse("language g0\nhello = \"Hello\"\nasm.result = _hello ++ \", World!\"\n");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn lowers_suppressed_local_names_to_canonical_body_references() {
    let parsed = parse("language g0\nkeep _value = value\nasm.result = keep \"Hello, World!\"\n");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World!"
    );
}

#[test]
fn lowers_dictionary_literals_to_lazy_values() {
    let parsed =
        parse("language g0\nd = { hello:\"Hello\", world:other ++ \"!\" }\nother = \"World\"\n");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let dictionary = fully_evaluated_value(resolved_value_at_path(&value, &["d"]));
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &dictionary,
            &["hello"]
        ))),
        b"Hello"
    );
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &dictionary,
            &["world"]
        ))),
        b"World!"
    );
}

#[test]
fn lowers_general_dictionary_entry_paths() {
    let parsed = parse(concat!(
        "language g0\n",
        "path = [1] ++ [3,4]\n",
        "d = { [0]:\"A\", [1,2]:\"B\", (path):\"C\", named.deep:\"D\" }\n",
        "asm.result = d.[0] ++ d.[1,2] ++ d.[1,3,4] ++ d.named.deep\n",
    ));
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&value, &["asm", "result"]);
    assert_eq!(output_bytes(&fully_evaluated_value(result)), b"ABCD");
}

#[test]
fn lowers_general_tagged_paths_and_constructors() {
    let parsed = parse(concat!(
        "language g0\n",
        "tail = ['more]\n",
        "dotted = foo.bar:\"A\"\n",
        "indexed = ['left,'right]:\"B\"\n",
        "dynamic = (['deep] ++ tail):\"C\"\n",
        "constructor = :made.nested\n",
        "constructed = constructor \"D\"\n",
        "dynamic_constructor = :(['built] ++ tail)\n",
        "dynamic_constructed = dynamic_constructor \"E\"\n",
        "asm.result = dotted.foo.bar ++ indexed.left.right ++ ",
        "dynamic.deep.more ++ constructed.made.nested ++ dynamic_constructed.built.more\n",
    ));
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&value, &["asm", "result"]);
    assert_eq!(output_bytes(&fully_evaluated_value(result)), b"ABCDE");
}

#[test]
fn lowers_tuple_syntax_to_a_tagged_list() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std as std\n",
        "pair = (\"Hello, World!\", 42)\n",
        "asm.result = std.list.head pair.tuple\n",
    ));
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&value, &["asm", "result"]);
    assert_eq!(
        output_bytes(&fully_evaluated_value(result)),
        b"Hello, World!"
    );
}

#[test]
fn lowering_starts_from_prior_dictionary() {
    let parsed = parse("language g0\nworld = \"World\"\n");
    let context = CompileContext::default().with_prior_defs(Value::Dict(
        crate::core::Dict::new_sync().insert(
            Key::atom_from_text("hello"),
            Value::binary_from_text("Hello"),
        ),
    ));
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        value.get_atom_path(&[Atom::from_key(&Key::binary_from_text("hello"))]),
        Some(&Value::binary_from_text("Hello"))
    );
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["world"])),
        Value::binary_from_text("World")
    );
}

#[test]
fn lowers_builtin_imports_to_module_dictionaries() {
    let parsed = parse("language g0\nimport 'std as std\nimport 'math\nimport 'list as list\n");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let std = value
        .get_atom_path(&[Atom::from_key(&Key::binary_from_text("std"))])
        .expect("std import should exist");
    let std = crate::eval::eval_value(&test_eval_context(), std)
        .expect("std import should evaluate to a dictionary");
    let floor = value
        .get_atom_path(&[Atom::from_key(&Key::binary_from_text("floor"))])
        .expect("inline math import should expose floor");
    let mod_fn = value
        .get_atom_path(&[Atom::from_key(&Key::binary_from_text("mod"))])
        .expect("inline math import should expose mod");
    let list_len_import = crate::eval::eval_value(
        &test_eval_context(),
        &core_global_access(
            &context,
            vec![Key::atom_from_text("list"), Key::atom_from_text("len")],
        ),
    )
    .expect("list.len import should resolve");
    let list_spec = crate::eval::eval_value(
        &test_eval_context(),
        &core_global_access(
            &context,
            vec![Key::atom_from_text("list"), Key::atom_from_text("spec")],
        ),
    )
    .expect("list.spec import should resolve");
    let list_head_import = crate::eval::eval_value(
        &test_eval_context(),
        &core_global_access(
            &context,
            vec![Key::atom_from_text("list"), Key::atom_from_text("head")],
        ),
    )
    .expect("list.head import should resolve");
    let list_tail_import = crate::eval::eval_value(
        &test_eval_context(),
        &core_global_access(
            &context,
            vec![Key::atom_from_text("list"), Key::atom_from_text("tail")],
        ),
    )
    .expect("list.tail import should resolve");
    let list_pure_import = crate::eval::eval_value(
        &test_eval_context(),
        &core_global_access(
            &context,
            vec![Key::atom_from_text("list"), Key::atom_from_text("pure")],
        ),
    )
    .expect("list.pure import should resolve");
    let (
        anno,
        std_not,
        std_could,
        std_list_len,
        std_list_split,
        std_list_split_end,
        std_list_head,
        std_list_tail,
        std_list_pure,
    ) = match &std {
        Value::Dict(std) => {
            assert!(matches!(
                std.get(&Key::atom_from_text("interaction_net")),
                Some(Value::Builtin(crate::core::Builtin::InteractionNet))
            ));
            assert!(matches!(
                std.get(&Key::atom_from_text("net_arity")),
                Some(Value::Builtin(crate::core::Builtin::NetArity))
            ));
            assert!(matches!(
                std.get(&Key::atom_from_text("seq")),
                Some(Value::Builtin(crate::core::Builtin::Seq))
            ));
            assert!(matches!(
                std.get(&Key::atom_from_text("spark")),
                Some(Value::Builtin(crate::core::Builtin::Spark))
            ));
            let anno = std
                .get(&Key::atom_from_text("anno"))
                .expect("std import should expose anno");
            let not = std
                .get(&Key::atom_from_text("not"))
                .expect("std import should expose not");
            let could = std
                .get(&Key::atom_from_text("could"))
                .expect("std import should expose could");
            let std_list = std
                .get(&Key::atom_from_text("list"))
                .expect("std import should expose list");
            let Value::Dict(std_list) = crate::eval::eval_value(&test_eval_context(), std_list)
                .expect("std.list should evaluate")
            else {
                panic!("std.list should evaluate to a dictionary");
            };
            let len = std_list
                .get(&Key::atom_from_text("len"))
                .expect("std.list should expose len");
            let split = std_list
                .get(&Key::atom_from_text("split"))
                .expect("std.list should expose split");
            let split_end = std_list
                .get(&Key::atom_from_text("split_end"))
                .expect("std.list should expose split_end");
            let head = std_list
                .get(&Key::atom_from_text("head"))
                .expect("std.list should expose head");
            let tail = std_list
                .get(&Key::atom_from_text("tail"))
                .expect("std.list should expose tail");
            let pure = std_list
                .get(&Key::atom_from_text("pure"))
                .expect("std.list should expose pure");
            (
                anno,
                not.clone(),
                could.clone(),
                len.clone(),
                split.clone(),
                split_end.clone(),
                head.clone(),
                tail.clone(),
                pure.clone(),
            )
        }
        _ => unreachable!(),
    };
    let list_module = builtin_list_module();
    let list_len = list_module
        .get(&Key::atom_from_text("len"))
        .expect("list module should expose len");
    let list_split = list_module
        .get(&Key::atom_from_text("split"))
        .expect("list module should expose split");
    let list_split_end = list_module
        .get(&Key::atom_from_text("split_end"))
        .expect("list module should expose split_end");
    let list_head = list_module
        .get(&Key::atom_from_text("head"))
        .expect("list module should expose head");
    let list_tail = list_module
        .get(&Key::atom_from_text("tail"))
        .expect("list module should expose tail");
    let list_pure = list_module
        .get(&Key::atom_from_text("pure"))
        .expect("list module should expose pure");

    let Value::Dict(_) = std else {
        panic!("std import should evaluate to a dictionary");
    };
    assert!(matches!(std, Value::Dict(_)));
    assert!(matches!(anno, Value::Builtin(crate::core::Builtin::Anno)));
    assert!(matches!(
        crate::eval::eval_value(&test_eval_context(), &std_not).unwrap(),
        Value::Function(_) | Value::Net(_)
    ));
    assert!(matches!(
        crate::eval::eval_value(&test_eval_context(), &std_could).unwrap(),
        Value::Function(_) | Value::Net(_)
    ));
    assert!(matches!(floor, Value::Builtin(crate::core::Builtin::Floor)));
    assert!(matches!(mod_fn, Value::Builtin(crate::core::Builtin::Mod)));
    assert!(matches!(
        std_list_len,
        Value::Builtin(crate::core::Builtin::ListLen)
    ));
    assert!(matches!(
        std_list_split,
        Value::Builtin(crate::core::Builtin::ListSplit)
    ));
    assert!(matches!(
        std_list_split_end,
        Value::Builtin(crate::core::Builtin::ListSplitEnd)
    ));
    assert!(matches!(
        std_list_head,
        Value::Builtin(crate::core::Builtin::ListHead)
    ));
    assert!(matches!(
        std_list_tail,
        Value::Builtin(crate::core::Builtin::ListTail)
    ));
    assert!(matches!(
        std_list_pure,
        Value::Builtin(crate::core::Builtin::ListEffect)
    ));
    assert!(matches!(
        list_len,
        Value::Builtin(crate::core::Builtin::ListLen)
    ));
    assert!(matches!(
        list_split,
        Value::Builtin(crate::core::Builtin::ListSplit)
    ));
    assert!(matches!(
        list_split_end,
        Value::Builtin(crate::core::Builtin::ListSplitEnd)
    ));
    assert!(matches!(
        list_head,
        Value::Builtin(crate::core::Builtin::ListHead)
    ));
    assert!(matches!(
        list_tail,
        Value::Builtin(crate::core::Builtin::ListTail)
    ));
    assert!(matches!(
        list_pure,
        Value::Builtin(crate::core::Builtin::ListEffect)
    ));
    assert!(matches!(
        list_len_import,
        Value::Builtin(crate::core::Builtin::ListLen)
    ));
    assert!(matches!(
        list_head_import,
        Value::Builtin(crate::core::Builtin::ListHead)
    ));
    assert!(matches!(
        list_tail_import,
        Value::Builtin(crate::core::Builtin::ListTail)
    ));
    assert!(matches!(
        list_pure_import,
        Value::Builtin(crate::core::Builtin::ListEffect)
    ));
    assert!(!matches!(
        list_spec,
        Value::Dict(dict) if dict.is_empty()
    ));
}

#[test]
fn builtin_list_at_is_exposed_by_list_and_std_modules() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std as std\n",
            "import 'list as list\n",
            "from_std = std.list.at 1 \"ABC\"\n",
            "from_list = list.at 1 [10,20,30]\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["from_std"])),
        Value::Number(n(i64::from(b'B')))
    );
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["from_list"])),
        Value::Number(n(20))
    );
}

#[test]
fn constructs_and_observes_an_interaction_net_from_source_effects() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "answer_net = interaction_net do\n",
            "  .data \"Hello, World!\" -> ports\n",
            "  .r (list.head ports)\n",
            "asm.result = net_arity 0 answer_net\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&definitions, &["asm", "result"]);
    assert_eq!(output_bytes(&result), b"Hello, World!");
}

#[test]
fn interaction_net_bind_builds_an_ordinary_identity_function() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "identity_net = interaction_net (.bind >>= (\\ports -> .wire (list.head (list.tail ports)) (list.head (list.tail (list.tail ports))) =>> .r (list.head ports)))\n",
            "asm.result = net_arity 1 identity_net \"Hello, World!\"\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&definitions, &["asm", "result"]);
    assert_eq!(output_bytes(&result), b"Hello, World!");
}

#[test]
fn interaction_net_construction_is_memoized_and_preserves_initial_active_pairs() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "raw = interaction_net (.bind >>= (\\bind -> .data 0 >>= (\\data -> .copy 0 >>= (\\erase -> .wire (list.head bind) (list.head data) =>> .wire (list.head (list.tail (list.tail bind))) (list.head erase) =>> .r (list.head (list.tail bind))))))\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let first = resolved_value_at_path(&definitions, &["raw"]);
    let second = resolved_value_at_path(&definitions, &["raw"]);
    let (Value::Net(first), Value::Net(second)) = (first, second) else {
        panic!("interaction_net should produce a net")
    };
    assert!(first.runtime().ptr_eq(second.runtime()));
    assert_eq!(
        first.runtime().with(|runtime| runtime.active_pairs().len()),
        1
    );
}

#[test]
fn interaction_net_construction_supports_local_effect_state() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "answer_net = interaction_net (.data \"state\" >>= (\\ports -> .set '.port (list.head ports) =>> .get '.port >>= (\\port -> .r port)))\n",
            "asm.result = net_arity 0 answer_net\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&definitions, &["asm", "result"]);
    assert_eq!(output_bytes(&result), b"state");
}

#[test]
fn interaction_net_construction_backtracks_and_requires_one_result() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "fallback = interaction_net (.alt (.bind >>= (\\_ports -> .fail)) (.data \"fallback\" >>= (\\ports -> .r (list.head ports))))\n",
            "selected = interaction_net (.cut (.alt (.data \"left\" >>= (\\ports -> .r (list.head ports))) (.data \"right\" >>= (\\ports -> .r (list.head ports)))))\n",
            "ambiguous = interaction_net (.alt (.data \"left\" >>= (\\ports -> .r (list.head ports))) (.data \"right\" >>= (\\ports -> .r (list.head ports))))\n",
            "missing = interaction_net .fail\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let fallback = resolved_value_at_path(&definitions, &["fallback"]);
    let selected = resolved_value_at_path(&definitions, &["selected"]);
    assert!(matches!(fallback, Value::Net(_)));
    assert!(matches!(selected, Value::Net(_)));

    let ambiguous = value_at_atom_path(&definitions, &["ambiguous"]).unwrap();
    assert!(
        fully_evaluated_error(ambiguous)
            .to_string()
            .contains("produced multiple results")
    );
    let missing = value_at_atom_path(&definitions, &["missing"]).unwrap();
    assert!(
        fully_evaluated_error(missing)
            .to_string()
            .contains("produced no successful result")
    );
}

#[test]
fn interaction_net_data_does_not_force_its_payload_during_construction() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "bad = 1 / 0\n",
            "raw = interaction_net (.data bad >>= (\\ports -> .r (list.head ports)))\n",
            "observed = net_arity 0 raw\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    assert!(matches!(
        resolved_value_at_path(&definitions, &["raw"]),
        Value::Net(_)
    ));
    let observed = value_at_atom_path(&definitions, &["observed"]).unwrap();
    let error = fully_evaluated_error(observed).to_string();
    assert!(
        error.contains("divide by zero"),
        "unexpected error: {error}"
    );
}

#[test]
fn interaction_net_finalization_reports_invalid_topology() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "not_port = interaction_net (.r 42)\n",
            "unwired = interaction_net (.bind >>= (\\ports -> .r (list.head ports)))\n",
            "duplicate = interaction_net (.bind >>= (\\ports -> .wire (list.head (list.tail ports)) (list.head (list.tail (list.tail ports))) =>> .wire (list.head (list.tail ports)) (list.head (list.tail (list.tail ports))) =>> .r (list.head ports)))\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    for (name, expected) in [
        ("not_port", "requires a construction port"),
        ("unwired", "is unwired"),
        ("duplicate", "is wired more than once"),
    ] {
        let value = value_at_atom_path(&definitions, &[name]).unwrap();
        assert!(
            fully_evaluated_error(value).to_string().contains(expected),
            "{name} should report `{expected}`"
        );
    }
}

#[test]
fn interaction_net_copy_effect_covers_erase_tunnel_and_balanced_fans() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "erase = interaction_net (.copy 0 >>= (\\ports -> .r (list.head ports)))\n",
            "tunnel = interaction_net (.copy 1 >>= (\\copy -> .data \"through\" >>= (\\data -> .wire (list.head (list.tail copy)) (list.head data) =>> .r (list.head copy))))\n",
            "fan = interaction_net (.copy 3 >>= (\\copy -> .copy 0 >>= (\\e1 -> .copy 0 >>= (\\e2 -> .copy 0 >>= (\\e3 -> .wire (list.head (list.tail copy)) (list.head e1) =>> .wire (list.head (list.tail (list.tail copy))) (list.head e2) =>> .wire (list.head (list.tail (list.tail (list.tail copy)))) (list.head e3) =>> .r (list.head copy))))))\n",
            "asm.result = net_arity 0 tunnel\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    assert!(matches!(
        resolved_value_at_path(&definitions, &["erase"]),
        Value::Net(_)
    ));
    assert!(matches!(
        resolved_value_at_path(&definitions, &["fan"]),
        Value::Net(_)
    ));
    let result = resolved_value_at_path(&definitions, &["asm", "result"]);
    assert_eq!(output_bytes(&result), b"through");
}

#[test]
fn interaction_net_copy_requires_a_representable_nonnegative_integer() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "negative = interaction_net (.copy _1 >>= (\\ports -> .r (list.head ports)))\n",
            "fractional = interaction_net (.copy (1/2) >>= (\\ports -> .r (list.head ports)))\n",
            "overflow = interaction_net (.copy 18446744073709551616 >>= (\\ports -> .r (list.head ports)))\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    for name in ["negative", "fractional", "overflow"] {
        let value = value_at_atom_path(&definitions, &[name]).unwrap();
        let error = fully_evaluated_error(value).to_string();
        assert!(
            error.contains("requires non-negative integer indices"),
            "unexpected {name} error: {error}"
        );
    }
}

#[test]
fn lowers_unique_declarations_via_abstract_global_paths() {
    let context = CompileContext::default();
    let lowered = lower_with_module_path(
        "language g0\nunique Foo, palette.Blue\n",
        &["pkg", "module"],
    );
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        value.get_atom_path(&[Atom::from_key(&Key::binary_from_text("Foo"))]),
        Some(&abstract_path_atom(&["pkg", "module", "Foo"]))
    );
    assert_eq!(
        value_at_atom_path(&value, &["palette", "Blue"]).as_ref(),
        Some(&abstract_path_atom(&["pkg", "module", "palette", "Blue"]))
    );
}

#[test]
fn g_parser_rejects_non_utf8_source_bytes() {
    let parsed = parse_source(b"language g0\nanswer = \xff\n");

    assert_eq!(parsed.declarations, []);
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].severity, Severity::Error);
    assert!(parsed.diagnostics[0].message.contains("not valid UTF-8"));
}

#[test]
fn compile_source_emits_relative_diagnostics_through_context() {
    let emitted = Arc::new(Mutex::new(Vec::new()));
    let captured = emitted.clone();
    let context =
        CompileContext::default().with_diagnostic_emitter(Arc::new(move |severity, message| {
            captured
                .lock()
                .expect("diagnostic mutex should not be poisoned")
                .push((severity, message));
        }));

    let _definitions = compile_source(b"language g0\nbroken =\n", &context);

    let emitted = emitted
        .lock()
        .expect("diagnostic mutex should not be poisoned");
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].0, Severity::Error);
    let Value::Dict(message) = &emitted[0].1 else {
        panic!("diagnostic message must be a dictionary");
    };
    let Some(Value::Dict(interface)) = message.get(&*crate::core::keys::MSG) else {
        panic!("diagnostic message must provide msg");
    };
    let Some(Value::Dict(location)) = interface.get(&*crate::core::keys::LOCATION) else {
        panic!("diagnostic message must provide msg.location");
    };
    assert_eq!(
        location.get(&*crate::core::keys::LINE),
        Some(&Value::Number(crate::number::Number::from_usize(2)))
    );
    assert!(matches!(
        interface.get(&*crate::core::keys::TEXT),
        Some(Value::Binary(message)) if !message.is_empty()
    ));
    assert!(interface.get(&*crate::core::keys::SEVERITY).is_none());
}

#[test]
fn inline_builtin_imports_follow_ordered_module_updates() {
    let context = CompileContext::default();
    let parsed = parse("language g0\nmath.answer = 42\nimport 'std\n");
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let math = value
        .get_atom_path(&[Atom::from_key(&Key::binary_from_text("math"))])
        .expect("std import should merge into existing math");
    let math = crate::eval::eval_value(&test_eval_context(), math)
        .expect("merged math binding should evaluate");

    let Value::Dict(math) = math else {
        panic!("math should evaluate to a dictionary");
    };

    assert_eq!(
        math.get(&Key::atom_from_text("answer"))
            .map(|value| fully_evaluated_value(value.clone())),
        Some(Value::Number(42.into()))
    );
    assert!(matches!(
        math.get(&Key::atom_from_text("floor")),
        Some(Value::Builtin(crate::core::Builtin::Floor))
    ));
    assert!(matches!(
        math.get(&Key::atom_from_text("mod")),
        Some(Value::Builtin(crate::core::Builtin::Mod))
    ));
}

#[test]
fn introduce_and_override_checks_are_deferred_until_observed() {
    let context = CompileContext::default();
    let parsed = parse("language g0\nfoo := 1\nok = \"ok\"\n");
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["ok"])),
        Value::binary_from_text("ok")
    );

    let foo = value
        .get_atom_path(&[Atom::from_key(&Key::binary_from_text("foo"))])
        .expect("foo binding should exist lazily");
    let err = fully_evaluated_error(foo.clone());
    assert_eq!(
        err.to_string(),
        "cannot override `foo` because it is not defined"
    );
}

#[test]
fn duplicate_introductions_fail_lazily_against_prior_module_updates() {
    let context = CompileContext::default();
    let parsed = parse("language g0\nfoo = 1\nfoo = 2\nok = \"ok\"\n");
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["ok"])),
        Value::binary_from_text("ok")
    );

    let foo = value
        .get_atom_path(&[Atom::from_key(&Key::binary_from_text("foo"))])
        .expect("duplicate foo binding should exist lazily");
    let err = fully_evaluated_error(foo.clone());
    assert_eq!(
        err.to_string(),
        "cannot introduce `foo` because it is already defined"
    );
}

#[test]
fn update_definitions_observe_prior_module_state() {
    let context = CompileContext::default();
    let parsed = parse("language g0\nfoo = 1\nfoo ::= \\prior -> prior + 1\n");
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["foo"])),
        Value::Number(2.into())
    );
}

#[test]
fn ordinary_module_demand_launches_final_reflection_tasks_once() {
    let source = r#"language g0
import 'std
refl.notice = .log 'info { msg:{ text:"new reflection task" } }
refl.notice := .log 'info { msg:{ text:"final reflection task" } }
meta.hidden = "metadata"
spec.hidden = "specification"
ordinary = "ordinary"
ordinary_two = "ordinary two"
probe = anno { refl:(.heap.get [guard,'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [guard,'tasks] >>= (\tasks -> .task.join (list.head tasks).task >>= (\_ -> .r ()))))) } "probe"
"#;
    let (_assembler, context, module, diagnostics) =
        reflection_test_module(source, &["module_refl_test"], &[("guard", "refl")]);

    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["meta", "hidden"]),
        Value::binary_from_text("metadata")
    );
    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["spec", "hidden"]),
        Value::binary_from_text("specification")
    );
    assert!(take_reflection_diagnostics(&diagnostics).is_empty());

    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["ordinary"]),
        Value::binary_from_text("ordinary")
    );
    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["probe"]),
        Value::binary_from_text("probe")
    );
    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["ordinary_two"]),
        Value::binary_from_text("ordinary two")
    );

    let diagnostics = take_reflection_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message(), "final reflection task");
}

#[test]
fn named_top_level_object_uses_object_refl_without_triggering_module_refl() {
    let source = r#"language g0
import 'std
refl.module_notice = .log 'info { msg:{ text:"module reflection task" } }
meta.probe = anno { refl:(.heap.get [[object_refl_marker,foo.spec.name],'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [[object_refl_marker,foo.spec.name],'tasks] >>= (\tasks -> .task.join (list.head tasks).task >>= (\_ -> .r ()))))) } "probe"
object foo with
  refl.notice = .log 'info { msg:{ text:"object reflection task" } }
  meta.hidden = "metadata"
  value = "value"
"#;
    let (_assembler, context, module, diagnostics) =
        reflection_test_module(source, &["object_refl_test"], &[]);

    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["foo", "meta", "hidden"]),
        Value::binary_from_text("metadata")
    );
    assert!(take_reflection_diagnostics(&diagnostics).is_empty());

    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["foo", "value"]),
        Value::binary_from_text("value")
    );
    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        Value::binary_from_text("probe")
    );

    let diagnostics = take_reflection_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message(), "object reflection task");
}

#[test]
fn nested_declared_object_uses_final_extended_refl() {
    let source = r#"language g0
import 'std
meta.probe = anno { refl:(.heap.get [[object_refl_marker,parent.child.spec.name],'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [[object_refl_marker,parent.child.spec.name],'tasks] >>= (\tasks -> .task.join (list.head tasks).task >>= (\_ -> .r ()))))) } "probe"
object parent with
  refl.parent_notice = .log 'info { msg:{ text:"parent reflection task" } }
  object child with
    refl.notice = .log 'info { msg:{ text:"original child reflection task" } }
    value = "nested value"
extend parent.child with
  refl.notice := .log 'info { msg:{ text:"extended child reflection task" } }
"#;
    let (_assembler, context, module, diagnostics) =
        reflection_test_module(source, &["nested_object_refl_test"], &[]);

    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["parent", "child", "value"]),
        Value::binary_from_text("nested value")
    );
    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        Value::binary_from_text("probe")
    );

    let diagnostics = take_reflection_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message(), "extended child reflection task");
}

#[test]
fn inherited_member_uses_derived_object_refl_and_guard() {
    let source = r#"language g0
import 'std
meta.derived_probe = anno { refl:(.heap.get [[object_refl_marker,derived.spec.name],'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [[object_refl_marker,derived.spec.name],'tasks] >>= (\tasks -> .task.join (list.head tasks).task >>= (\_ -> .r ()))))) } "derived probe"
meta.base_probe = anno { refl:(.heap.get [[object_refl_marker,base.spec.name],'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [[object_refl_marker,base.spec.name],'tasks] >>= (\tasks -> .task.join (list.head tasks).task >>= (\_ -> .r ()))))) } "base probe"
object base with
  refl.notice = .log 'info { msg:{ text:"base reflection task" } }
  inherited = "inherited value"
object derived extends base with
  refl.notice := .log 'info { msg:{ text:"derived reflection task" } }
"#;
    let (_assembler, context, module, diagnostics) =
        reflection_test_module(source, &["inherited_object_refl_test"], &[]);

    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["derived", "inherited"]),
        Value::binary_from_text("inherited value")
    );
    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["meta", "derived_probe"]),
        Value::binary_from_text("derived probe")
    );
    let derived_diagnostics = take_reflection_diagnostics(&diagnostics);
    assert_eq!(derived_diagnostics.len(), 1);
    assert_eq!(derived_diagnostics[0].message(), "derived reflection task");

    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["base", "inherited"]),
        Value::binary_from_text("inherited value")
    );
    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["meta", "base_probe"]),
        Value::binary_from_text("base probe")
    );
    let base_diagnostics = take_reflection_diagnostics(&diagnostics);
    assert_eq!(base_diagnostics.len(), 1);
    assert_eq!(base_diagnostics[0].message(), "base reflection task");
}

#[test]
fn object_expressions_and_excluded_nested_objects_remain_inert() {
    let source = r#"language g0
import 'std
value = object "expression object" with
  refl.notice = .log 'info { msg:{ text:"object expression reflection task" } }
  ordinary = "ordinary"
object declared with
  object meta with
    refl.notice = .log 'info { msg:{ text:"excluded nested reflection task" } }
    ordinary = "excluded ordinary"
"#;
    let (_assembler, context, module, diagnostics) =
        reflection_test_module(source, &["object_expression_refl_test"], &[]);

    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["value", "ordinary"]),
        Value::binary_from_text("ordinary")
    );
    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["declared", "meta", "ordinary"]),
        Value::binary_from_text("excluded ordinary")
    );
    assert!(take_reflection_diagnostics(&diagnostics).is_empty());
}

#[test]
fn overriding_refl_with_undefined_disables_automatic_tasks() {
    let source = r#"language g0
import 'std
refl.notice = .log 'info { msg:{ text:"disabled reflection task" } }
refl := {}
meta.probe = anno { refl:(.heap.get [guard,'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [guard,'tasks] >>= (\_tasks -> .r ())))) } "probe"
ordinary = "ordinary"
"#;
    let (_assembler, context, module, diagnostics) =
        reflection_test_module(source, &["disabled_refl_test"], &[("guard", "refl")]);

    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["ordinary"]),
        Value::binary_from_text("ordinary")
    );
    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        Value::binary_from_text("probe")
    );
    assert!(take_reflection_diagnostics(&diagnostics).is_empty());
}

#[test]
fn automatic_reflection_tasks_require_unit_results() {
    let source = r#"language g0
import 'std
refl.bad = .r "not unit"
meta.probe = anno { refl:(.heap.get [guard,'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [guard,'tasks] >>= (\tasks -> .task.error (list.head tasks).task >>= (\_error -> .r ()))))) } "probe"
ordinary = "ordinary"
"#;
    let (_assembler, context, module, diagnostics) =
        reflection_test_module(source, &["unit_refl_test"], &[("guard", "refl")]);

    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["ordinary"]),
        Value::binary_from_text("ordinary")
    );
    assert_eq!(
        resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        Value::binary_from_text("probe")
    );
    assert!(take_reflection_diagnostics(&diagnostics).is_empty());
}

#[test]
fn update_definitions_can_use_named_updater_functions() {
    let context = CompileContext::default();
    let parsed = parse("language g0\ninc prior = prior + 1\nfoo = 1\nfoo ::= inc\n");
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["foo"])),
        Value::Number(2.into())
    );
}

#[test]
fn overrides_replace_prior_definitions_without_union_ambiguity() {
    let context = CompileContext::default().with_prior_defs(Value::Dict(
        Dict::new_sync().insert(Key::atom_from_text("foo"), Value::Number(1.into())),
    ));
    let parsed = parse("language g0\nfoo := 2\n");
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);

    assert_eq!(
        fully_evaluated_value(resolved_value_at_path(&value, &["foo"])),
        Value::Number(2.into())
    );
}
