use crate::compiler::CompileContext;
use crate::core::{Dict, Key, Value};
use crate::number::Number;
use crate::test_support::ResultTestExt as _;

fn test_assembler() -> crate::api::Assembler {
    thread_local! {
        static ASSEMBLER: crate::api::Assembler = crate::api::Assembler::builder()
            .evaluation_runtime(crate::api::compiler_test_runtime())
            .build()
            .expect("compiler test assembler should be constructible");
    }
    ASSEMBLER.with(Clone::clone)
}

fn test_eval_context() -> crate::evaluation::EvalContext {
    // A test's fixtures share its thread's compiler runtime and cached lazy
    // values. Use the same patient direct-client context as production
    // Assembler demands, so a producer claimed elsewhere is awaited rather
    // than exposed as a retryable machine-level block.
    test_assembler().eval_context()
}

fn same_value_representation(left: &Value, right: &Value) -> bool {
    crate::compiler::test_value_factory().same_representation_for_test(left, right)
}

fn diagnostic_kind_name(value: &Value) -> &'static str {
    crate::compiler::test_value_factory()
        .with_boundary_access(|access| access.diagnostic_kind_name(value))
}

#[track_caller]
fn assert_same_value_representation(left: &Value, right: &Value) {
    crate::compiler::test_value_factory().assert_same_representation_for_test(left, right);
}

fn context_final_defs(context: &CompileContext) -> Value {
    context
        .values()
        .with_boundary_access(|access| context.final_defs(&access))
}

fn core_global_access(context: &CompileContext, path: Vec<Key>) -> Value {
    lower_resolved_expr(
        context.values(),
        ResolvedExpr::Access {
            base: Box::new(ResolvedExpr::Provided(context_final_defs(context))),
            path: path.into_iter().map(ResolvedPathPart::Key).collect(),
        },
    )
}

fn evaluated_module_value(context: &CompileContext, lowered: &LoweredSource) -> Value {
    let Value::Promised(final_defs) = context_final_defs(context) else {
        panic!("final module binding should be a promised value");
    };
    let definitions = context
        .values()
        .with_boundary_access(|access| lowered.definitions(&access));
    crate::core::set_test_promise(
        context.values(),
        &final_defs,
        definitions.duplicate_for_test(context.values()),
    )
    .expect_without_debug("future should not be set yet");
    crate::evaluation::EvalContext::evaluate_compatibility_whnf(&test_eval_context(), &definitions)
        .expect("lowered module should evaluate")
}

fn assert_reserved_keyword_diagnostic(source: &str, keyword: &str) {
    let parsed = parse(source);
    assert!(
        parsed.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.message.contains(&format!("`{keyword}`"))
                && diagnostic.message.contains("reserved keyword")
                && diagnostic.message.contains(&format!("'{keyword}"))
                && diagnostic.message.contains(&format!(".['{keyword}]"))
        }),
        "missing reserved-keyword diagnostic for `{keyword}` in `{source}`: {:?}",
        parsed.diagnostics
    );
}

fn value_at_atom_path(definitions: &Value, path: &[&str]) -> Option<Value> {
    let context = test_eval_context();
    value_at_atom_path_with_context(&context, definitions, path)
}

fn value_at_atom_path_with_context(
    context: &crate::evaluation::EvalContext,
    definitions: &Value,
    path: &[&str],
) -> Option<Value> {
    let mut current = definitions.duplicate_for_test(context.values());
    for part in path {
        let current_value =
            crate::evaluation::EvalContext::evaluate_compatibility_whnf(context, &current).ok()?;
        let Value::Dict(dict) = current_value else {
            return None;
        };
        current = dict
            .get(&Key::Atom(Atom::from_key(&Key::binary_from_text(*part))))?
            .duplicate_for_test(context.values());
    }
    Some(current)
}

fn resolved_value_at_path(definitions: &Value, path: &[&str]) -> Value {
    let value = value_at_atom_path(definitions, path).expect("binding should exist");
    crate::evaluation::EvalContext::evaluate_compatibility_whnf(&test_eval_context(), &value)
        .expect("binding should resolve")
}

fn resolved_value_at_path_with_context(
    context: &crate::evaluation::EvalContext,
    definitions: &Value,
    path: &[&str],
) -> Value {
    let mut current = definitions.duplicate_for_test(context.values());
    for part in path {
        let current_value = fully_evaluated_value_with_context(context, current);
        let Value::Dict(dict) = current_value else {
            panic!("reflection-enabled path should traverse dictionaries");
        };
        current = dict
            .get(&Key::atom_from_text(part))
            .expect("reflection-enabled binding should exist")
            .duplicate_for_test(context.values());
    }
    fully_evaluated_value_with_context(context, current)
}

fn fully_evaluated_value_with_context(
    context: &crate::evaluation::EvalContext,
    mut value: Value,
) -> Value {
    while matches!(value, Value::Lazy(_) | Value::Promised(_)) {
        match crate::evaluation::EvalContext::evaluate_compatibility_whnf(context, &value) {
            Ok(evaluated) => value = evaluated,
            Err(blocked) if blocked.blocked_on().is_some() => {
                let wait = blocked
                    .blocked_on()
                    .expect("checked reflection dependency should remain available");
                loop {
                    match context.pump_wait(&wait.0, 16_384) {
                        crate::evaluation::EvaluationPumpOutcome::TargetReady => break,
                        crate::evaluation::EvaluationPumpOutcome::BudgetExhausted => continue,
                        crate::evaluation::EvaluationPumpOutcome::Busy => {
                            std::thread::yield_now();
                        }
                        crate::evaluation::EvaluationPumpOutcome::NoProgress => {
                            if !context.poll_runtime_background() {
                                panic!("reflection-enabled value made no progress: {blocked}")
                            }
                        }
                    }
                }
            }
            Err(error) => panic!("reflection-enabled value should fully evaluate: {error}"),
        }
    }
    value
}

fn fully_evaluated_value(mut value: Value) -> Value {
    let context = test_eval_context();
    while matches!(value, Value::Lazy(_) | Value::Promised(_)) {
        value = crate::evaluation::EvalContext::evaluate_compatibility_whnf(&context, &value)
            .expect("value should fully evaluate");
    }
    value
}

fn fully_evaluated_error(mut value: Value) -> crate::core::EvaluationHalt {
    let context = test_eval_context();
    loop {
        match crate::evaluation::EvalContext::evaluate_compatibility_whnf(&context, &value) {
            Ok(next @ (Value::Lazy(_) | Value::Promised(_))) => value = next,
            Ok(other) => panic!(
                "value should fail instead of evaluating to {}",
                diagnostic_kind_name(&other)
            ),
            Err(error) => return error,
        }
    }
}

fn output_bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::Binary(bytes) => bytes.to_vec(),
        Value::List(list) => crate::eval::list_output_bytes(&test_eval_context(), list)
            .expect("output list should render as bytes"),
        other => panic!(
            "expected binary output value, got {}",
            diagnostic_kind_name(other)
        ),
    }
}

fn output_binary_result_list(value: &Value) -> Vec<u8> {
    let Value::List(list) = value else {
        panic!(
            "expected list output value, got {}",
            diagnostic_kind_name(value)
        );
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
                    crate::evaluation::EvalContext::evaluate_compatibility_whnf(
                        &test_eval_context(),
                        value,
                    )
                    .map_err(|err| err.to_string())?,
                );
                bytes.borrow_mut().extend(output_bytes(&value));
            }
            Ok(())
        },
        &mut |thunk| match crate::evaluation::EvalContext::evaluate_compatibility_whnf(
            &test_eval_context(),
            &match thunk {
                crate::core::ListThunk::Lazy(lazy) => {
                    Value::Lazy(lazy.duplicate_for_test(&crate::compiler::test_value_factory()))
                }
                crate::core::ListThunk::Promised(promise) => Value::Promised(
                    promise.duplicate_for_test(&crate::compiler::test_value_factory()),
                ),
            },
        )
        .map_err(|err| err.to_string())?
        {
            Value::Binary(bytes) => Ok(crate::core::List::from_bytes(bytes)),
            Value::List(list) => Ok(list),
            other => Err(format!(
                "lazy output chunk was not a list or binary: {}",
                diagnostic_kind_name(&other)
            )),
        },
    )
    .expect("result list should render as binary values");
    bytes.into_inner()
}

use super::*;
use crate::diagnostic::Severity;

mod pure_handler;
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
    crate::runtime::RuntimeValueRoot,
    Arc<Mutex<Vec<crate::api::DiagnosticEvent>>>,
) {
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let received = diagnostics.clone();
    let runtime = crate::api::EvaluationRuntime::new(0)
        .expect("reflection test runtime should be constructible");
    let assembler = crate::api::Assembler::builder()
        .evaluation_runtime(runtime)
        .diagnostic_callback(move |event| {
            received
                .lock()
                .expect("diagnostic collector should not be poisoned")
                .push(event);
        })
        .build()
        .expect("reflection test assembler should be constructible");
    let context = CompileContext::from_module_path_with_values(
        assembler.core_values(),
        module_path.iter().copied(),
    );
    // Each guard is a namespace's reflection state key, as the compiler
    // builds it.
    let prior = guards
        .iter()
        .fold(Dict::new_sync(), |dict, (name, namespace)| {
            dict.insert(
                Key::atom_from_text(name),
                context.values().with_boundary_access(|access| {
                    let name = ResolvedExpr::Provided(context.namespace_name(&access, namespace));
                    lower_resolved_expr_in(
                        &access,
                        compiler_values::reflection_state_key_resolved(&access, name),
                    )
                }),
            )
        });
    let context = context.with_prior_defs(Value::Dict(prior));
    let lowered = lower_parsed_source(parse(source), &context);
    assert_eq!(lowered.diagnostics, []);
    let Value::Promised(final_defs) = context_final_defs(&context) else {
        panic!("final module binding should be promised");
    };
    let lowered_definitions = context
        .values()
        .with_boundary_access(|access| lowered.definitions(&access));
    crate::core::set_test_promise(
        context.values(),
        &final_defs,
        lowered_definitions.duplicate_for_test(context.values()),
    )
    .expect_without_debug("final module binding should be unset");

    let eval_context = assembler.eval_context();
    let definitions = crate::evaluation::EvalContext::evaluate_compatibility_whnf(
        &eval_context,
        &lowered_definitions,
    )
    .expect("reflection-enabled module should expose its dictionary");
    let definitions_root = context
        .values()
        .construct_runtime_value_root(|access| access.duplicate_value(&definitions));
    (
        assembler,
        eval_context,
        definitions,
        definitions_root,
        diagnostics,
    )
}

#[test]
fn latent_source_meta_refl_cycle_reclaims_with_its_module() {
    let runtime = crate::api::EvaluationRuntime::new(0)
        .expect("reflection-cycle test runtime should be constructible");
    let assembler = crate::api::Assembler::builder()
        .evaluation_runtime(runtime)
        .build()
        .expect("reflection-cycle test assembler should be constructible");
    let values = assembler.core_values();
    {
        let warm_context =
            CompileContext::from_module_path_with_values(values.clone(), ["meta_refl_warmup"]);
        let warm = lower_parsed_source(
            parse("language g0\nimport 'std\nmeta.warm = {}\n"),
            &warm_context,
        );
        assert_eq!(warm.diagnostics, []);
    }
    values.collect_and_drain_external_owners_for_test();
    let baseline = values
        .collect_managed_for_test()
        .expect("the source reflection-cycle fixture should start collectible");
    let baseline_owners = values.external_owner_count_for_test();

    {
        let context =
            CompileContext::from_module_path_with_values(values.clone(), ["meta_refl_cycle"]);
        let lowered = lower_parsed_source(
            parse(
                "language g0\n\
                 import 'std\n\
                 meta.x = anno meta_refl:(\\_ -> .r meta.x) [anno 'meta_init ()]\n",
            ),
            &context,
        );
        assert_eq!(lowered.diagnostics, []);
        let Value::Promised(final_defs) = context_final_defs(&context) else {
            panic!("final module binding should be promised");
        };
        let lowered_definitions = context
            .values()
            .with_boundary_access(|access| lowered.definitions(&access));
        crate::core::set_test_promise(
            context.values(),
            &final_defs,
            lowered_definitions.duplicate_for_test(context.values()),
        )
        .expect_without_debug("final module binding should start unassigned");

        let eval_context = assembler.eval_context();
        let module = crate::evaluation::EvalContext::evaluate_compatibility_whnf(
            &eval_context,
            &lowered_definitions,
        )
        .expect("the source reflection-cycle module should expose its dictionary");
        assert!(matches!(
            resolved_value_at_path_with_context(&eval_context, &module, &["meta", "x"]),
            Value::List(_)
        ));
    }

    values.collect_and_drain_external_owners_for_test();
    let reclaimed = values
        .collect_managed_for_test()
        .expect("dropping the module should reclaim its latent reflection cycle");
    assert_eq!(values.external_owner_count_for_test(), baseline_owners);
    assert_eq!(reclaimed.root_entries(), baseline.root_entries());
    assert_eq!(reclaimed.marked_slots(), baseline.marked_slots());
}

/// Each event's text, evaluated as a renderer does: a diagnostic built from
/// a value summarizes statically.
fn rendered_texts(
    context: &crate::evaluation::EvalContext,
    events: &[crate::api::DiagnosticEvent],
) -> Vec<String> {
    let values = crate::api::Values::from_core_factory(context.values().clone());
    events
        .iter()
        .map(|event| event.diagnostic().rendered_text_for_test(&values))
        .collect()
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
fn declaration_resolution_builds_uncached_effects_outside_managed_access() {
    let runtime = crate::api::EvaluationRuntime::new(0)
        .expect("declaration-boundary test runtime should be constructible");
    let assembler = crate::api::Assembler::builder()
        .evaluation_runtime(runtime)
        .build()
        .expect("declaration-boundary test assembler should be constructible");
    let context = CompileContext::from_module_path_with_values(
        assembler.core_values(),
        ["declaration_boundary"],
    );

    // This private effect path is absent from the compiler family's standard
    // construction set, so resolving it deterministically takes the cache-miss
    // path. The miss evaluates one closed compiler helper and therefore must
    // not inherit the access region used to project or lower module state.
    let lowered = lower_parsed_source(
        parse("language g0\nresult = .w2r_declaration_resolution_probe\n"),
        &context,
    );

    assert_eq!(lowered.diagnostics, []);
}

#[test]
fn parses_language_declaration_with_extensions() {
    let parsed = parse("language g0 with utf8, demo\nanswer = 42\n");

    assert_eq!(
        parsed.declarations[0].kind,
        DeclarationKind::Language(LanguageDecl {
            base: "g0".to_owned(),
            extensions: vec!["utf8".to_owned(), "demo".to_owned()],
        })
    );
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].severity, Severity::Error);
    assert!(
        parsed.diagnostics[0]
            .message
            .contains("unrecognized language extension `demo`")
    );
}

#[test]
fn unrecognized_language_version_stops_parsing() {
    let parsed = parse("language g9\nanswer = \n");

    assert_eq!(parsed.declarations.len(), 1);
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].line, 1);
    assert!(
        parsed.diagnostics[0]
            .message
            .contains("unrecognized language version `g9`")
    );
}

#[test]
fn non_ascii_texts_and_comments_require_utf8() {
    // `utf8` covers the whole file, including a header comment that precedes
    // the declaration.
    for (source, line) in [
        ("language g0\ngreeting = \"caf\u{e9}\"\n", 2),
        ("language g0\n# caf\u{e9}\ngreeting = \"cafe\"\n", 2),
        ("# caf\u{e9}\nlanguage g0\ngreeting = \"cafe\"\n", 1),
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.diagnostics.len(), 1, "{source:?}");
        assert_eq!(parsed.diagnostics[0].line, line, "{source:?}");
        assert!(
            parsed.diagnostics[0]
                .message
                .contains("non-ASCII character `\u{e9}` requires `language g0 with utf8`")
        );
        assert_eq!(
            parsed.declarations.len(),
            2,
            "the character set does not stop parsing"
        );

        let parsed = parse(&source.replacen("language g0", "language g0 with utf8", 1));
        assert_eq!(parsed.diagnostics, [], "{source:?}");
    }
}

#[test]
fn staged_module_lowering_matches_batch_module_values() {
    let source = "language g0\nanswer = 42\n";
    let batch_context = CompileContext::default();
    let batch = lower_parsed_source(parse(source), &batch_context);
    let staged_context = CompileContext::default();
    let staged = lower_source(source.as_bytes(), &staged_context);

    assert_eq!(batch.diagnostics, staged.diagnostics);
    let batch = evaluated_module_value(&batch_context, &batch);
    let staged = evaluated_module_value(&staged_context, &staged);
    assert_same_value_representation(
        &resolved_value_at_path(&batch, &["answer"]),
        &resolved_value_at_path(&staged, &["answer"]),
    );
}

#[test]
fn staged_module_lowering_preserves_diagnostic_order() {
    let source = "\
language g0
language g0
import 'missing
foo = \\foo -> foo
";
    let batch = lower_parsed_source(parse(source), &CompileContext::default());
    let staged = lower_source(source.as_bytes(), &CompileContext::default());

    assert_eq!(batch.diagnostics, staged.diagnostics);
    assert_eq!(staged.diagnostics.len(), 3);
    assert_eq!(
        staged.diagnostics[0].message,
        "language declaration must appear before all other declarations"
    );
    assert_eq!(
        staged.diagnostics[1].message,
        "local `foo` shadows external `foo` defined on line 4"
    );
    assert_eq!(
        staged.diagnostics[2].message,
        "unknown built-in module `'missing`"
    );
}

#[test]
fn g0_keywords_are_reserved_across_source_name_positions() {
    for (keyword, source) in [
        ("where", "language g0\nwhere = 1\n"),
        (
            "abstract_global_path",
            "language g0\nabstract_global_path = 1\n",
        ),
        ("module_origin", "language g0\nmodule_origin = 1\n"),
        ("where", "language g0\n.where = 1\n"),
        ("let", "language g0\nvalue let = let\n"),
        ("where", "language g0\nvalue = \\where -> 1\n"),
        ("as", "language g0\nvalue = let as = 1 in as\n"),
        (
            "where",
            "language g0\nvalue = do { .r 1 -> where; .r where }\n",
        ),
        (
            "binary",
            "language g0\nvalue = object \"value\" with { alias binary }\n",
        ),
    ] {
        assert_reserved_keyword_diagnostic(source, keyword);
    }
}

#[test]
fn bare_keyword_references_and_path_roots_are_rejected() {
    for (keyword, source) in [
        ("where", "language g0\nvalue = f (where)\n"),
        ("abstract", "language g0\nvalue = abstract\n"),
        ("if", "language g0\nvalue = if.x then 1 else 0\n"),
        ("match", "language g0\nvalue = (match.x)\n"),
        ("where", "language g0\nvalue = f where.y\n"),
        ("object", "language g0\nvalue = object.x\n"),
    ] {
        assert_reserved_keyword_diagnostic(source, keyword);
    }
}

/// A keyword spelled as a key, before a joint `:` or after a joint `.`, `'`
/// or constructor `:`, is data wherever an expression stands: never a
/// form's head, a suffix or a postfix `if`.
#[test]
fn keyword_keys_are_data_in_every_expression_position() {
    for keyword in crate::g_syntax::keywords::G0_KEYWORDS {
        let spelling = keyword.spelling();
        for expression in [
            format!("{spelling}:atom"),
            format!("({spelling}:atom)"),
            format!("{spelling}.key:atom"),
            format!("(\\x -> x) {spelling}:atom"),
            format!("key.{spelling}:atom"),
            format!("{{{spelling}.key: atom}}"),
            format!("'{spelling}"),
            format!("'.{spelling}"),
            format!("({spelling}:)"),
            format!("atom.{spelling}"),
            format!("(.{spelling})"),
            format!("(\\x -> x) '{spelling}"),
            format!("(\\x -> x) ({spelling}:)"),
            format!("(\\x -> x) atom.{spelling}"),
        ] {
            let parsed = parse(&format!("language g0\natom = 'a\nvalue = {expression}\n"));
            assert_eq!(parsed.diagnostics, [], "value = {expression}");
        }
    }
}

#[test]
fn keyword_atoms_explicit_key_paths_and_tags_remain_data() {
    let parsed = parse(concat!(
        "language g0\n",
        "atom = 'where\n",
        "effect = .where\n",
        ".['where] = atom\n",
        "nested.where = atom\n",
        "declare outer.where\n",
        "unique markers.unique\n",
        "import 'list as imports.with\n",
        "object nested.object with {}\n",
        "direct = module.where\n",
        "self_direct = object self with { .['where] = atom; copy = self.where }\n",
        "tagged = binary:atom\n",
        "constructor = (binary:)\n",
        "selected = module.['where]\n",
        "direct_quoted = '.where\n",
        "quoted = '.['where]\n",
    ));
    assert_eq!(parsed.diagnostics, []);
}

#[test]
fn simple_declarations_preserve_indented_line_continuations() {
    let parsed = parse(concat!(
        "language\n",
        "  g0 with\n",
        "    utf8\n",
        "import\n",
        "  'std\n",
        "  as standard\n",
        "declare first,\n",
        "  nested.second\n",
        "unique\n",
        "  Marker\n",
    ));

    assert_eq!(parsed.diagnostics, []);
    assert_eq!(
        parsed.declarations[0].kind,
        DeclarationKind::Language(LanguageDecl {
            base: "g0".to_owned(),
            extensions: vec!["utf8".to_owned()],
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
        DeclarationKind::Declare(vec!["first".to_owned(), "nested.second".to_owned()])
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
fn boundary_aligned_closers_must_terminate_declarations() {
    let valid = parse(concat!(
        "language g0\n",
        "terminal = (\n",
        "  1\n",
        ")\n",
        "continued = (\n",
        "  1\n",
        "  ) + 2\n",
    ));
    assert_eq!(valid.diagnostics, []);

    let same_line_suffix = parse(concat!("language g0\n", "value = (\n", "  1\n", ") + 2\n",));
    assert!(same_line_suffix.diagnostics.iter().any(|diagnostic| {
        diagnostic.line == 4
            && diagnostic
                .message
                .contains("expression continues after a boundary-aligned closing delimiter")
    }));

    let later_suffix = parse(concat!(
        "language g0\n",
        "value = (\n",
        "  1\n",
        ")\n",
        "  + 2\n",
    ));
    assert!(later_suffix.diagnostics.iter().any(|diagnostic| {
        diagnostic.line == 4
            && diagnostic
                .message
                .contains("indent the closing delimiter to continue")
    }));
}

#[test]
fn boundary_aligned_closer_policy_applies_to_nested_declarations() {
    let parsed = parse(concat!(
        "language g0\n",
        "object outer with\n",
        "  member = (\n",
        "    1\n",
        "  ) + 2\n",
    ));

    assert!(parsed.diagnostics.iter().any(|diagnostic| {
        diagnostic.line == 5
            && diagnostic
                .message
                .contains("expression continues after a boundary-aligned closing delimiter")
    }));
}

#[test]
fn non_closer_continuations_cannot_start_at_a_declaration_boundary() {
    let parsed = parse(concat!(
        "language g0\n",
        "value = (\n",
        "unindented\n",
        ")\n",
    ));

    assert!(parsed.diagnostics.iter().any(|diagnostic| {
        diagnostic.line == 3
            && diagnostic
                .message
                .contains("expression continuation is indented 0 spaces")
    }));
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
        "language g0 with utf8\n",
        "import 'std as standard\n",
        "declare missing, nested.value\n",
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
        DeclarationKind::Declare(vec!["missing".to_owned(), "nested.value".to_owned()])
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
        ("language g0\ndeclare\n", 2),
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
            realization: ObjectRealization::Instance,
            target: "child".to_owned(),
            alias: None,
            deps: vec![
                SyntaxExpr::Name("base".to_owned()),
                SyntaxExpr::Name("mixin".to_owned()),
            ],
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
fn hanging_object_and_extend_declarations_parse() {
    let object_anchor = "object child with ".len();
    let extend_anchor = "extend child with ".len();
    let source = format!(
        "language g0\nobject child with first = 1\n{object:object_anchor$}second = 2\nextend child with first := 3\n{extend:extend_anchor$}third = 4\n",
        object = "",
        extend = "",
    );
    let parsed = parse(&source);

    assert_eq!(parsed.diagnostics, []);
    assert!(matches!(
        &parsed.declarations[1].kind,
        DeclarationKind::Object(ObjectDecl { body, .. }) if body.len() == 2
    ));
    assert!(matches!(
        &parsed.declarations[2].kind,
        DeclarationKind::Extend(ObjectExtendDecl { body, .. }) if body.len() == 2
    ));
}

#[test]
fn object_declarations_parse_expression_valued_parents() {
    let parsed =
        parse("language g0\nobject child extends configured_parent options, (choose left right)\n");

    assert_eq!(parsed.diagnostics, []);
    let DeclarationKind::Object(object) = &parsed.declarations[1].kind else {
        panic!("expected object declaration");
    };
    assert_eq!(object.deps.len(), 2);
    assert!(matches!(
        &object.deps[0],
        SyntaxExpr::Apply(function, argument)
            if matches!(&**function, SyntaxExpr::Name(name) if name == "configured_parent")
                && matches!(&**argument, SyntaxExpr::Name(name) if name == "options")
    ));
    assert!(matches!(&object.deps[1], SyntaxExpr::Apply(_, _)));
}

#[test]
fn object_parent_lists_reject_empty_expression_slots() {
    for source in [
        "language g0\nobject child extends , base\n",
        "language g0\nobject child extends base,, other\n",
        "language g0\nobject child extends base,\n",
        "language g0\nvalue = object \"child\" extends , base\n",
        "language g0\nvalue = object \"child\" extends base,, other\n",
        "language g0\nvalue = object \"child\" extends base,\n",
    ] {
        let parsed = parse(source);

        assert!(
            parsed.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error
                    && diagnostic.message == "object `extends` contains an empty parent expression"
            }),
            "{source}: {:#?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn empty_and_malformed_object_parents_have_distinct_diagnostics() {
    let empty = parse("language g0\nobject child extends\n");
    assert!(empty.diagnostics.iter().any(|diagnostic| {
        diagnostic.message == "object `extends` requires at least one parent expression"
    }));

    let malformed = parse("language g0\nobject child extends base +\n");
    assert!(
        malformed
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.message.contains("empty parent")),
        "{:#?}",
        malformed.diagnostics
    );
    assert!(
        malformed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "{:#?}",
        malformed.diagnostics
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
            realization: ObjectRealization::Instance,
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
fn parses_hierarchical_extend_body_declarations() {
    let parsed = parse(
        "language g0\nobject parent with\n  object child with\n    text = \"Hello\"\n  extend child with\n    alias prior\n    text := _prior.text ++ \"!\"\n",
    );

    assert_eq!(parsed.diagnostics, []);
    let DeclarationKind::Object(parent) = &parsed.declarations[1].kind else {
        panic!("parent should parse as an object declaration");
    };
    let ObjectBodyDefinitionKind::Extend(extend) = &parent.body[1].kind else {
        panic!("second parent body item should parse as an extend declaration");
    };
    assert_eq!(extend.target, "child");
    assert_eq!(extend.alias.as_ref().map(Alias::name), Some("prior"));
    assert_eq!(extend.body.len(), 1);
    assert!(extend.body[0].definition().is_some());
}

#[test]
fn parses_object_expressions() {
    let parsed = parse(
        "language g0\nhello = object \"hello\" extends base with\n  alias _h\n  text = h.target\n",
    );

    assert_eq!(parsed.diagnostics, []);
    assert!(matches!(
        &parsed.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            expr: Some(SyntaxExpr::Object(ObjectExpr {
                name: Some(_),
                alias: Some(alias),
                deps,
                body,
                ..
            })),
            ..
        }) if alias.name == "_h" && deps.len() == 1 && body.len() == 1
    ));
}

#[test]
fn parses_abstract_object_declarations_expressions_and_extensions() {
    let parsed = parse(
        "language g0\nabstract object protocol with {}\nvalue = abstract object \"value\" with {}\nobject outer with {\n  abstract object nested with {};\n  extend abstract nested with {};\n}\nextend abstract protocol with {}\ndeclare provided\n",
    );

    assert_eq!(parsed.diagnostics, []);
    assert!(matches!(
        &parsed.declarations[1].kind,
        DeclarationKind::Object(ObjectDecl {
            realization: ObjectRealization::Abstract,
            target,
            ..
        }) if target == "protocol"
    ));
    assert!(matches!(
        &parsed.declarations[2].kind,
        DeclarationKind::Definition(DefinitionDecl {
            expr: Some(SyntaxExpr::Object(ObjectExpr {
                realization: ObjectRealization::Abstract,
                ..
            })),
            ..
        })
    ));
    let DeclarationKind::Object(ObjectDecl { body, .. }) = &parsed.declarations[3].kind else {
        panic!("outer should parse as an object declaration");
    };
    assert!(matches!(
        &body[0].kind,
        ObjectBodyDefinitionKind::Object(ObjectDecl {
            realization: ObjectRealization::Abstract,
            ..
        })
    ));
    assert!(matches!(
        &body[1].kind,
        ObjectBodyDefinitionKind::Extend(ObjectExtendDecl {
            realization: ObjectRealization::Abstract,
            ..
        })
    ));
    assert!(matches!(
        &parsed.declarations[4].kind,
        DeclarationKind::Extend(ObjectExtendDecl {
            realization: ObjectRealization::Abstract,
            target,
            ..
        }) if target == "protocol"
    ));
    assert!(matches!(
        &parsed.declarations[5].kind,
        DeclarationKind::Declare(names) if names == &["provided"]
    ));
}

#[test]
fn malformed_abstract_object_forms_report_errors() {
    for source in [
        "language g0\nabstract object\n",
        "language g0\nabstract object _ with {}\n",
        "language g0\nvalue = abstract object\n",
        "language g0\nextend abstract target\n",
    ] {
        let parsed = parse(source);
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error),
            "expected an abstract-object diagnostic for {source:?}"
        );
    }
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
fn objects_without_with_bodies_remain_valid() {
    let parsed = parse("language g0\nobject declared\nexpression = object \"expression\"\n");

    assert_eq!(parsed.diagnostics, []);
    assert!(matches!(
        &parsed.declarations[1].kind,
        DeclarationKind::Object(ObjectDecl { body, .. }) if body.is_empty()
    ));
    assert!(matches!(
        &parsed.declarations[2].kind,
        DeclarationKind::Definition(DefinitionDecl {
            expr: Some(SyntaxExpr::Object(ObjectExpr { body, .. })),
            ..
        }) if body.is_empty()
    ));
}

#[test]
fn omitted_and_layout_empty_object_with_bodies_are_rejected() {
    for (source, expected) in [
        (
            "language g0\nobject declared with\n",
            "object `with` requires a body; use `with {}` for an explicit empty body",
        ),
        (
            "language g0\nobject declared with\n  # comments are not definitions\n",
            "object `with` requires a body; use `with {}` for an explicit empty body",
        ),
        (
            "language g0\nexpression = object \"expression\" with\n",
            "object `with` requires a body; use `with {}` for an explicit empty body",
        ),
        (
            "language g0\nexpression = object \"expression\" with\n  # comments are not definitions\n",
            "object `with` requires a body; use `with {}` for an explicit empty body",
        ),
    ] {
        let parsed = parse(source);

        assert_eq!(parsed.diagnostics.len(), 1, "{source}");
        assert_eq!(parsed.diagnostics[0].severity, Severity::Error, "{source}");
        assert_eq!(parsed.diagnostics[0].message, expected, "{source}");
    }
}

#[test]
fn explicit_braced_compound_bodies_may_be_empty() {
    let parsed = parse(
        "language g0\nobject declared with {}\nexpression = object () with {}\nextend declared with {}\nunchanged = { value:\"Hello, World!\" } with {}\nasm.result = unchanged.value where {}\n",
    );
    assert_eq!(parsed.diagnostics, []);
    assert!(matches!(
        &parsed.declarations[1].kind,
        DeclarationKind::Object(ObjectDecl { body, .. }) if body.is_empty()
    ));
    assert!(matches!(
        &parsed.declarations[2].kind,
        DeclarationKind::Definition(DefinitionDecl {
            expr: Some(SyntaxExpr::Object(ObjectExpr { body, .. })),
            ..
        }) if body.is_empty()
    ));
    assert!(matches!(
        &parsed.declarations[3].kind,
        DeclarationKind::Extend(ObjectExtendDecl { body, .. }) if body.is_empty()
    ));

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
fn braced_with_bodies_share_the_recursive_object_declaration_grammar() {
    let parsed = parse(
        "language g0\nobject parent with {;\n  prefix = \"Hello\";\n  object child with { text = ^prefix ++ \", World\" };\n  extend child with { text := _text ++ \"!\" };\n}\nasm.result = parent.child.text\n",
    );
    assert_eq!(parsed.diagnostics, []);

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
fn layout_with_bodies_preserve_nested_dictionary_match_arms() {
    let parsed = parse(concat!(
        "language g0\n",
        "object matcher with\n",
        "  classify = match {} as\n",
        "    {} => \"empty\"\n",
        "    {x:item} as _whole => item\n",
        "    _ => \"other\"\n",
        "  braced = match {} as { {} => \"empty\"; _ => \"other\"; }\n",
        "  neighbor = 1\n",
        "updated = base with\n",
        "  classify = match {} as\n",
        "    {x:item} => item\n",
        "    _ => \"other\"\n",
        "  neighbor = 2\n",
    ));
    assert_eq!(parsed.diagnostics, []);

    let DeclarationKind::Object(ObjectDecl { body, .. }) = &parsed.declarations[1].kind else {
        panic!("the layout object declaration should remain an object");
    };
    assert_eq!(body.len(), 3);
    let ObjectBodyDefinitionKind::Definition(classify) = &body[0].kind else {
        panic!("the first object member should remain a definition");
    };
    assert!(matches!(
        classify.expr.as_ref(),
        Some(SyntaxExpr::Match(subject))
            if subject.arms.len() == 3
                && matches!(subject.arms[1].pattern.kind, SyntaxPatternKind::As(_, _))
    ));
    let ObjectBodyDefinitionKind::Definition(braced) = &body[1].kind else {
        panic!("the second object member should remain a definition");
    };
    assert!(matches!(
        braced.expr.as_ref(),
        Some(SyntaxExpr::Match(subject)) if subject.arms.len() == 2
    ));

    let DeclarationKind::Definition(DefinitionDecl {
        expr: Some(SyntaxExpr::With { body, .. }),
        ..
    }) = &parsed.declarations[2].kind
    else {
        panic!("the layout dictionary update should remain a `with` expression");
    };
    assert_eq!(body.len(), 2);
    let ObjectBodyDefinitionKind::Definition(classify) = &body[0].kind else {
        panic!("the first dictionary update member should remain a definition");
    };
    assert!(matches!(
        classify.expr.as_ref(),
        Some(SyntaxExpr::Match(subject)) if subject.arms.len() == 2
    ));
}

#[test]
fn malformed_nested_dictionary_match_reports_the_arm_not_the_object_header() {
    let parsed = parse(concat!(
        "language g0\n",
        "object matcher with\n",
        "  classify = match {} as\n",
        "    {x:item} as => item\n",
        "    _ => \"other\"\n",
        "  neighbor = 1\n",
    ));

    assert!(!parsed.diagnostics.is_empty());
    assert!(
        parsed
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.message.contains("object declaration header")),
        "nested match recovery escaped to the object header: {:#?}",
        parsed.diagnostics
    );
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.line == 4),
        "nested match diagnostic should remain on its malformed arm: {:#?}",
        parsed.diagnostics
    );
}

#[test]
fn where_binding_can_own_a_deeper_layout_with_expression() {
    let source = concat!(
        "language g0\n",
        "base = { text:\"before\" }\n",
        "asm.result = updated.text where\n",
        "  updated = base with\n",
        "    text := \"after\"\n",
    );
    let parsed = parse(source);
    assert_eq!(parsed.diagnostics, []);

    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);
    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"after"
    );
}

#[test]
fn leading_infix_resumption_after_layout_do_and_with_evaluates() {
    let source = concat!(
        "language g0\n",
        "finish _ = \"after-do\"\n",
        "base = { text:\"before\" }\n",
        "read_text value = value.text\n",
        "asm.do_result =\n",
        "  do\n",
        "    .r \"before-do\"\n",
        "  |> finish\n",
        "asm.with_result = base with\n",
        "    text := \"after-with\"\n",
        "  |> read_text\n",
    );
    let parsed = parse(source);
    assert_eq!(parsed.diagnostics, []);

    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);
    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "do_result"]
        ))),
        b"after-do"
    );
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "with_result"]
        ))),
        b"after-with"
    );
}

#[test]
fn braced_with_bodies_reject_semicolon_only_and_interior_empty_members() {
    for source in [
        "language g0\nobject bad with {;}\n",
        "language g0\nobject bad with { first = 1;; second = 2 }\n",
    ] {
        let parsed = parse(source);
        assert!(
            parsed.diagnostics.iter().any(|diagnostic| {
                diagnostic.message.contains("use `{}` for an empty body")
                    || diagnostic
                        .message
                        .contains("empty member between semicolons")
            }),
            "`{source}` reported {:#?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn object_header_aliases_point_to_their_bodies() {
    for source in [
        "language g0\nobject declared as self\n",
        "language g0\nobject declared as named with\n  x = named.y\n",
        "language g0\nexpression = object \"expression\" as self\n",
    ] {
        let parsed = parse(source);

        assert_eq!(parsed.diagnostics.len(), 1, "{source}");
        assert_eq!(parsed.diagnostics[0].severity, Severity::Error, "{source}");
        assert_eq!(
            parsed.diagnostics[0].message,
            "an alias is its body's first member: write `with { alias Name; … }`, not `as Name`",
            "{source}"
        );
    }
}

#[test]
fn an_alias_is_only_its_bodys_first_member() {
    for (source, expected) in [
        (
            "language g0\nobject declared with\n  x = 1\n  alias late\n",
            "`alias` may only be the first member of its body",
        ),
        (
            "language g0\nobject declared with { alias a; alias b }\n",
            "`alias` may only be the first member of its body",
        ),
        (
            "language g0\nupdated = {} with { x = 1; alias late }\n",
            "`alias` may only be the first member of its body",
        ),
        (
            "language g0\nobject declared with\n  alias\n  x = 1\n",
            "`alias` requires a name, `self`, or `_`",
        ),
        (
            "language g0\nobject declared with { alias 'a }\n",
            "`alias` requires a name, `self`, or `_`",
        ),
        (
            "language g0\nalias named\n",
            "`alias` may only be the first member of a `with` body",
        ),
    ] {
        let parsed = parse(source);

        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error
                    && diagnostic.message.starts_with(expected)),
            "{source}: {:?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn parses_object_and_extend_aliases() {
    let parsed = parse(
        "language g0\nobject child extends base with\n  alias _c\n  text = c.base\nextend child with\n  alias _c\n  text := _c.text ++ \"!\"\n",
    );

    assert_eq!(parsed.diagnostics, []);
    match &parsed.declarations[1].kind {
        DeclarationKind::Object(object) => {
            assert_eq!(object.target, "child");
            assert_eq!(object.alias.as_ref().map(Alias::name), Some("_c"));
            assert_eq!(object.deps, [SyntaxExpr::Name("base".to_owned())]);
        }
        other => panic!("expected object declaration, got {other:?}"),
    }
    match &parsed.declarations[2].kind {
        DeclarationKind::Extend(extend) => {
            assert_eq!(extend.target, "child");
            assert_eq!(extend.alias.as_ref().map(Alias::name), Some("_c"));
        }
        other => panic!("expected extend declaration, got {other:?}"),
    }
}

#[test]
fn object_declaration_aliases_follow_local_unused_warning_rules() {
    let parsed = parse(
        "language g0\nobject a with\n  alias unused\n  x = 1\nobject b with\n  alias _unused\n  x = 1\nobject c with\n  alias _\n  x = 1\nobject d with\n  alias used\n  x = used.y\nextend d with\n  alias update_unused\n  x := 2\nextend d with\n  alias _update_unused\n  x := 3\n",
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
            && diag.line == 3
            && diag.message == "unused local `unused`"
    }));
    assert!(parsed.diagnostics.iter().any(|diag| {
        diag.severity == Severity::Warning
            && diag.line == 15
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

/// A long literal with computed items composes lists of a few items and
/// long closed runs with `++`; it must evaluate to the same list as the
/// literal written out in full.
#[test]
fn composed_list_literals_keep_their_items_in_order() {
    let items = (1..=40)
        .map(|item| match item {
            // A lone computed item, then a long closed run, then a short one.
            5 | 30 | 31 | 33 => format!("{item} + 0"),
            _ => item.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let closed = (1..=40)
        .map(|item| item.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(&format!(
            "language g0\nresult = if [{items}] == [{closed}] then \"same\" else \"different\"\n"
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&definitions, &["result"]);
    assert_eq!(output_bytes(&result), b"same");
}

/// A dictionary literal with computed values joins its entries in a
/// shallow tree of unions; it must build the same dictionary as the literal
/// written with closed values.
#[test]
fn composed_dict_literals_build_the_same_dictionary() {
    let entries = |computed: bool| {
        (1..=40)
            .map(|item| {
                if computed && item % 3 == 0 {
                    format!("k{item}: {item} + 0")
                } else {
                    format!("k{item}: {item}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(&format!(
            "language g0\nresult = if {{{}}} == {{{}}} then \"same\" else \"different\"\n",
            entries(true),
            entries(false)
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&definitions, &["result"]);
    assert_eq!(output_bytes(&result), b"same");
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
fn parses_explicitly_grouped_mixed_arithmetic() {
    let parsed = parse("language g0\nanswer = (1 + (2 * 3)) - (4 / 5)\n");

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
fn parses_layout_do_in_definition_lambda_and_back_pipe_positions() {
    let parsed = parse(concat!(
        "language g0\n",
        "main api = do\n",
        "  .prepare api\n",
        "  .r api\n",
        "wrapped = \\api -> do\n",
        "  .r api\n",
        "identity_net = interaction_net <| do\n",
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
        Some(SyntaxExpr::OperatorApply {
            operator: SyntaxOperator::PipeBackward,
            right,
            ..
        }) if matches!(right.as_ref(), SyntaxExpr::Do(_))
    ));
}

#[test]
fn back_piped_lambda_and_layout_do_arguments_evaluate() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "invoke input transform = transform input\n",
        "run_effect operation = list.head (list.pure operation)\n",
        "asm.lambda = invoke \"Hello\" <| \\value -> value ++ \", World!\"\n",
        "asm.layout = run_effect <| do\n",
        "  .r \"Hello, Do!\"\n",
    ));
    assert_eq!(parsed.diagnostics, []);

    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);
    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "lambda"]
        ))),
        b"Hello, World!"
    );
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "layout"]
        ))),
        b"Hello, Do!"
    );
}

#[test]
fn hanging_do_and_let_layout_evaluate() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "hanging = do first = 70\n",
        "             second = first + 2\n",
        "             .r second\n",
        "asm.do_result = list.head (list.pure hanging)\n",
        "asm.let_result =\n",
        "  let _ignored = 70\n",
        "      second = 72\n",
        "  second\n",
    ));
    assert_eq!(parsed.diagnostics, []);

    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);
    let value = evaluated_module_value(&context, &lowered);
    for path in ["do_result", "let_result"] {
        assert!(
            same_value_representation(
                &fully_evaluated_value(resolved_value_at_path(&value, &["asm", path])),
                &Value::Number(n(72)),
            ),
            "{path}"
        );
    }
}

#[test]
fn hanging_where_and_with_layout_evaluate() {
    let where_header = "where_result = first + second where ";
    let object_header = "object values with ";
    let with_header = "updated = values with ";
    let source = format!(
        "language g0\n{where_header}first = 40\n{where_indent:where_width$}second = 2\n{object_header}first = 40\n{object_indent:object_width$}second = 2\n{with_header}first := 70\n{with_indent:with_width$}second := 2\nasm.where_result = where_result\nasm.with_result = updated.first + updated.second\n",
        where_indent = "",
        where_width = where_header.len(),
        object_indent = "",
        object_width = object_header.len(),
        with_indent = "",
        with_width = with_header.len(),
    );
    let parsed = parse(&source);
    assert_eq!(parsed.diagnostics, []);

    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);
    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [("where_result", 42), ("with_result", 72)] {
        assert!(
            same_value_representation(
                &fully_evaluated_value(resolved_value_at_path(&value, &["asm", path])),
                &Value::Number(n(expected)),
            ),
            "{path}"
        );
    }
}

#[test]
fn braced_do_evaluates_like_layout_do_and_supports_empty_blocks() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "braced = do { first <- .r 70; second = first + 2; .r second }\n",
        "nested = do { value <- do .r 72; .r value }\n",
        "patterned = do { .r 70 -> (value); (_) = 1; .r (value + 2) }\n",
        "empty = do {}\n",
        "commented = do {\n",
        "  # comments do not make an empty block non-empty\n",
        "}\n",
        "asm.braced = list.head (list.pure braced)\n",
        "asm.nested = list.head (list.pure nested)\n",
        "asm.patterned = list.head (list.pure patterned)\n",
        "asm.empty = list.head (list.pure empty)\n",
        "asm.commented = list.head (list.pure commented)\n",
    ));
    assert_eq!(parsed.diagnostics, []);

    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);
    let value = evaluated_module_value(&context, &lowered);
    for path in ["braced", "nested", "patterned"] {
        assert!(
            same_value_representation(
                &fully_evaluated_value(resolved_value_at_path(&value, &["asm", path])),
                &Value::Number(n(72)),
            ),
            "{path}"
        );
    }
    let unit = crate::core::test_value_factory().with_boundary_access(|access| access.unit());
    for path in ["empty", "commented"] {
        assert!(
            same_value_representation(
                &fully_evaluated_value(resolved_value_at_path(&value, &["asm", path])),
                &unit,
            ),
            "{path}"
        );
    }
}

#[test]
fn do_bindings_follow_sequential_unused_local_rules() {
    let parsed = parse(concat!(
        "language g0\n",
        "outer prior = do\n",
        "  (current) <- .r prior\n",
        "  (unused) = current\n",
        "  (_) <- .r current\n",
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
        "  declare unused, _quiet, used\n",
        "  reader = \\_ -> used\n",
        "  (unused) = 1\n",
        "  quiet = 2\n",
        "  (used) = 3\n",
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
                "  declare missing\n",
                "  .r ()\n",
            ),
            3,
            "no later fulfillment for `missing`",
        ),
        (
            concat!(
                "language g0\n",
                "bad = do\n",
                "  declare value, _value\n",
                "  value = 1\n",
                "  .r value\n",
            ),
            3,
            "duplicate recursive do `declare` statement for `value`",
        ),
        (
            concat!(
                "language g0\n",
                "bad outer = do\n",
                "  declare outer\n",
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
        "  declare answer\n",
        "  reader = \\_ -> answer\n",
        "  .r 72 -> answer\n",
        "  .r (reader ())\n",
        "mutual = do\n",
        "  declare left, right\n",
        "  captured = 72\n",
        "  left = \\_ -> right\n",
        "  right = captured\n",
        "  ((left () == 72) and (captured == 72)) =>> .r 72\n",
        "sequential = do\n",
        "  declare first\n",
        "  first = 70\n",
        "  declare second\n",
        "  second = first + 2\n",
        "  .r second\n",
        "direct_hierarchy = do\n",
        "  declare outer\n",
        "  declare inner\n",
        "  inner = 2\n",
        "  outer = inner + 70\n",
        "  .r outer\n",
        "independent = do\n",
        "  declare _x, y, _z\n",
        "  early_y = \\_ -> y\n",
        "  y = 72\n",
        "  (early_y () == 72) =>> .r ()\n",
        "  x = 70\n",
        "  z = 74\n",
        "  .r y\n",
        "hierarchical = do\n",
        "  declare outer\n",
        "  inner = do\n",
        "    declare nested\n",
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
        assert!(
            same_value_representation(
                &fully_evaluated_value(resolved_value_at_path(&value, &["asm", path])),
                &Value::Number(n(72)),
            ),
            "{path}"
        );
    }
}

#[test]
fn crossing_recursive_do_promotes_fix_scope_without_leaking_name_visibility() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "initial = 1\n",
        "crossing = do\n",
        "  declare outer\n",
        "  prior = initial\n",
        "  declare _inner\n",
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
    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["asm", "crossing"])),
        &Value::Number(n(1)),
    );
}

#[test]
fn recursive_do_strict_forward_observation_reports_the_fixpoint_cycle() {
    let source = concat!(
        "language g0\n",
        "import 'std\n",
        "task = do\n",
        "  declare answer\n",
        "  (answer == 72) =>> .r ()\n",
        "  answer = 72\n",
        "  .r ()\n",
        "probe = anno { refl:task } \"unreachable\"\n",
    );
    let (_assembler, eval_context, definitions, _definitions_root, _diagnostics) =
        reflection_test_module(source, &["recursive_do_cycle"], &[]);
    let mut probe = value_at_atom_path_with_context(&eval_context, &definitions, &["probe"])
        .expect("probe should exist");
    let error = loop {
        match crate::evaluation::EvalContext::evaluate_compatibility_whnf(&eval_context, &probe) {
            Ok(next @ (Value::Lazy(_) | Value::Promised(_))) => probe = next,
            Ok(other) => panic!(
                "strict recursive observation produced {}",
                diagnostic_kind_name(&other)
            ),
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
        "  declare answer\n",
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
fn irrefutable_as_patterns_reject_duplicate_captures() {
    let parsed = parse(concat!(
        "language g0\n",
        "bad = do\n",
        "  .r 1 -> same as same\n",
        "  .r same\n",
    ));
    assert!(
        parsed
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error)
    );

    let lowered = lower_parsed_source(parsed, &CompileContext::default());
    let errors = lowered
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].line, 3);
    assert!(
        errors[0]
            .message
            .contains("local `same` shadows existing local `same`")
    );
}

#[test]
fn do_lowering_sequences_binds_value_guards_and_bare_operations() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "source_value = 70\n",
        "asm.backward = list.pure <| do\n",
        "  left <- .r \"A\"\n",
        "  right = \"B\"\n",
        "  .r ()\n",
        "  ((left == \"A\") and (right == \"B\")) =>> .r 65\n",
        "asm.forward = list.pure <| do\n",
        "  .r \"C\" -> left\n",
        "  .r \"D\" -> right\n",
        "  ((left == \"C\") and (right == \"D\")) =>> .r 67\n",
        "asm.drop = list.pure <| do\n",
        "  .r \"ignored\" -> _\n",
        "  .r 69\n",
        "asm.alias = list.pure <| do\n",
        "  .r \"G\" -> first as second\n",
        "  ((first == \"G\") and (second == \"G\")) =>> .r 71\n",
        "asm.scope = list.pure <| do\n",
        "  selected <- .r source_value\n",
        "  .r selected\n",
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
        ("alias", b"G".as_slice()),
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
fn literal_and_list_patterns_match_or_fall_through() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "asm.literal = list.pure <| do { .r 42 -> 42; .r \"L\" }\n",
        "asm.fixed = list.pure <| do { .r [65,66,67] -> [65,middle,67]; (middle == 66) =>> .r \"B\" }\n",
        "asm.segment = list.pure <| do { .r \"ABCD\" -> [65] ++ middle ++ [68]; .r middle }\n",
        "asm.text_segment = list.pure <| do { .r \"AB\" -> \"A\" ++ rest; .r rest }\n",
        "asm.quoted = list.pure <| do { .r ['foo,42,'bar] -> '.foo.[42,'bar]; .r \"Q\" }\n",
        "asm.backward_pattern = list.pure <| do { [65,last] <- .r \"AB\"; (last == 66) =>> .r \"W\" }\n",
        "asm.pure_pattern = list.pure <| do { [65,last] = \"AB\"; (last == 66) =>> .r \"P\" }\n",
        "asm.literal_fallback = list.pure (.alt (do { .r 41 -> 42; .r \"bad\" }) (.r \"F\"))\n",
        "asm.list_fallback = list.pure (.alt (do { .r [1] -> [1,2]; .r \"bad\" }) (.r \"S\"))\n",
        "asm.kind_fallback = list.pure (.alt (do { .r 1 -> []; .r \"bad\" }) (.r \"K\"))\n",
        "asm.recursive = list.pure <| do { declare left, right; .r [65,66] -> [left,right]; ((left == 65) and (right == 66)) =>> .r \"AB\" }\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("literal", b"L".as_slice()),
        ("fixed", b"B".as_slice()),
        ("segment", b"BC".as_slice()),
        ("text_segment", b"B".as_slice()),
        ("quoted", b"Q".as_slice()),
        ("backward_pattern", b"W".as_slice()),
        ("pure_pattern", b"P".as_slice()),
        ("literal_fallback", b"F".as_slice()),
        ("list_fallback", b"S".as_slice()),
        ("kind_fallback", b"K".as_slice()),
        ("recursive", b"AB".as_slice()),
    ] {
        let result = fully_evaluated_value(resolved_value_at_path(&value, &["asm", path]));
        assert_eq!(output_binary_result_list(&result), expected, "{path}");
    }
}

#[test]
fn dictionary_tag_and_tuple_patterns_match_or_fall_through() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "asm.empty = list.pure <| do { .r {} -> {}; .r \"E\" }\n",
        "asm.nested = list.pure <| do { .r {foo:{bar:65,baz:66},other:67} -> {foo.bar:value,rest}; ((value == 65) and (rest.foo.baz == 66) and (rest.other == 67)) =>> .r \"D\" }\n",
        "asm.remainder = list.pure <| do { .r {foo:65,extra:66} -> {foo:value,rest}; ((value == 65) and (rest.extra == 66)) =>> .r \"R\" }\n",
        "asm.whole = list.pure <| do { .r {foo:65} -> {whole}; (whole.foo == 65) =>> .r \"W\" }\n",
        "asm.tag = list.pure <| do { .r tag:65 -> tag:value; (value == 65) =>> .r \"T\" }\n",
        "asm.tag_short = list.pure <| do { .r tag:65 -> {:tag}; (tag == 65) =>> .r \"S\" }\n",
        "asm.tuple = list.pure <| do { .r (65,66) -> (left,right); ((left == 65) and (right == 66)) =>> .r \"U\" }\n",
        "asm.backward = list.pure <| do { {:value} <- .r {value:66}; (value == 66) =>> .r \"B\" }\n",
        "asm.pure = list.pure <| do { {:value} = {value:67}; (value == 67) =>> .r \"C\" }\n",
        "asm.extra_fallback = list.pure (.alt (do { .r {foo:1,extra:2} -> {foo:1}; .r \"bad\" }) (.r \"X\"))\n",
        "asm.tag_fallback = list.pure (.alt (do { .r {tag:1,extra:2} -> tag:1; .r \"bad\" }) (.r \"G\"))\n",
        "asm.tuple_fallback = list.pure (.alt (do { .r {tuple:[1],extra:2} -> (1,); .r \"bad\" }) (.r \"P\"))\n",
        "asm.kind_fallback = list.pure (.alt (do { .r 1 -> {}; .r \"bad\" }) (.r \"K\"))\n",
        "asm.required_missing = list.pure (.alt (do { .r {} -> {foo:{}}; .r \"bad\" }) (.r \"Q\"))\n",
        "asm.optional_missing = list.pure <| do { .r {} -> {foo?:[]}; .r \"M\" }\n",
        "asm.optional_capture = list.pure <| do { .r {} -> {foo?:value}; .r value -> []; .r \"C\" }\n",
        "asm.optional_present = list.pure <| do { .r {foo:65} -> {foo?:[value]}; (value == 65) =>> .r \"O\" }\n",
        "asm.optional_empty_value = list.pure <| do { .r {foo:{}} -> {foo?:[value]}; (value == {}) =>> .r \"E\" }\n",
        "asm.optional_remainder = list.pure <| do { .r {extra:66} -> {foo?:[],rest}; (rest.extra == 66) =>> .r \"A\" }\n",
        "asm.optional_nested = list.pure <| do { .r {foo:{extra:67}} -> {foo.bar?:[],rest}; (rest.foo.extra == 67) =>> .r \"N\" }\n",
        "asm.optional_literal_fallback = list.pure (.alt (do { .r {} -> {foo?:42}; .r \"bad\" }) (.r \"L\"))\n",
        "asm.optional_prefix_fallback = list.pure (.alt (do { .r {foo:1} -> {foo.bar?:[]}; .r \"bad\" }) (.r \"I\"))\n",
        "asm.computed_entry = list.pure <| do { .r {selector:'target,target:65} -> {selector:key,[key]:value}; (value == 65) =>> .r \"J\" }\n",
        "asm.computed_nested = list.pure <| do { .r {selector:'target,root:{target:66}} -> {selector:key,root.[key]:value}; (value == 66) =>> .r \"H\" }\n",
        "asm.computed_splice = list.pure <| do { .r {path:['root,'target],root:{target:67}} -> {path:path,(path):value}; (value == 67) =>> .r \"V\" }\n",
        "asm.computed_optional = list.pure <| do { .r {selector:'missing,keep:68} -> {selector:key,[key]?:[],rest}; (rest.keep == 68) =>> .r \"Y\" }\n",
        "asm.computed_exact = list.pure <| do { .r {selector:'target,root:{target:69}} -> {selector:key,root.[key]:value}; (value == 69) =>> .r \"Q\" }\n",
        "asm.computed_quoted = list.pure <| do { .r (42,['foo,42]) -> (index,'.foo.[index]); .r \"Z\" }\n",
        "asm.spliced_quoted = list.pure <| do { .r (['foo,42],['foo,42]) -> (path,'.(path)); .r \"F\" }\n",
        "asm.computed_fallback = list.pure (.alt (do { .r {selector:'missing} -> {selector:key,[key]:_value}; .r \"bad\" }) (.r \"G\"))\n",
        "asm.quoted_fallback = list.pure (.alt (do { .r (43,['foo,42]) -> (index,'.foo.[index]); .r \"bad\" }) (.r \"P\"))\n",
        "asm.computed_recursive = list.pure <| do { declare key,value; .r {selector:'target,target:65} -> {selector:key,[key]:value}; ((value == 65) and (key == 'target)) =>> .r \"X\" }\n",
        "asm.recursive = list.pure <| do { declare left, right; .r {left:65,right:66} -> {:left,:right}; ((left == 65) and (right == 66)) =>> .r \"AB\" }\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("empty", b"E".as_slice()),
        ("nested", b"D".as_slice()),
        ("remainder", b"R".as_slice()),
        ("whole", b"W".as_slice()),
        ("tag", b"T".as_slice()),
        ("tag_short", b"S".as_slice()),
        ("tuple", b"U".as_slice()),
        ("backward", b"B".as_slice()),
        ("pure", b"C".as_slice()),
        ("extra_fallback", b"X".as_slice()),
        ("tag_fallback", b"G".as_slice()),
        ("tuple_fallback", b"P".as_slice()),
        ("kind_fallback", b"K".as_slice()),
        ("required_missing", b"Q".as_slice()),
        ("optional_missing", b"M".as_slice()),
        ("optional_capture", b"C".as_slice()),
        ("optional_present", b"O".as_slice()),
        ("optional_empty_value", b"E".as_slice()),
        ("optional_remainder", b"A".as_slice()),
        ("optional_nested", b"N".as_slice()),
        ("optional_literal_fallback", b"L".as_slice()),
        ("optional_prefix_fallback", b"I".as_slice()),
        ("computed_entry", b"J".as_slice()),
        ("computed_nested", b"H".as_slice()),
        ("computed_splice", b"V".as_slice()),
        ("computed_optional", b"Y".as_slice()),
        ("computed_exact", b"Q".as_slice()),
        ("computed_quoted", b"Z".as_slice()),
        ("spliced_quoted", b"F".as_slice()),
        ("computed_fallback", b"G".as_slice()),
        ("quoted_fallback", b"P".as_slice()),
        ("computed_recursive", b"X".as_slice()),
        ("recursive", b"AB".as_slice()),
    ] {
        let result = fully_evaluated_value(resolved_value_at_path(&value, &["asm", path]));
        assert_eq!(output_binary_result_list(&result), expected, "{path}");
    }
}

#[test]
fn computed_tag_patterns_use_prior_captures_and_match_singleton_dicts() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "asm.key = list.pure <| do { .r ('target,target:69) -> (tag,[tag]:value); (value == 69) =>> .r \"K\" }\n",
        "asm.path = list.pure <| do { .r (['root,'target],root.target:70) -> (path,(path):value); (value == 70) =>> .r \"P\" }\n",
        "asm.fallback = list.pure (.alt (do { .r ('missing,target:71) -> (tag,[tag]:_value); .r \"bad\" }) (.r \"F\"))\n",
        "asm.recursive = list.pure <| do { declare tag,value; .r ('target,target:72) -> (tag,[tag]:value); ((tag == 'target) and (value == 72)) =>> .r \"R\" }\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("key", b"K".as_slice()),
        ("path", b"P".as_slice()),
        ("fallback", b"F".as_slice()),
        ("recursive", b"R".as_slice()),
    ] {
        let result = fully_evaluated_value(resolved_value_at_path(&value, &["asm", path]));
        assert_eq!(output_binary_result_list(&result), expected, "{path}");
    }
}

#[test]
fn refutable_dictionary_remainders_match_the_residual_dictionary() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "asm.match = list.pure <| do { .r {selector:'target,target:73} -> {selector:key,{[key]:value}}; (value == 73) =>> .r \"M\" }\n",
        "asm.empty = list.pure <| do { .r {selected:74} -> {selected:value,{}}; (value == 74) =>> .r \"E\" }\n",
        "asm.missing = list.pure (.alt (do { .r {selected:75} -> {selected:_value,{required:other}}; .r other }) (.r \"A\"))\n",
        "asm.extra = list.pure (.alt (do { .r {selected:76,required:77,extra:78} -> {selected:_value,{required:other}}; .r other }) (.r \"X\"))\n",
        "asm.recursive = list.pure <| do { declare key,value; .r {selector:'target,target:79} -> {selector:key,{[key]:value}}; ((key == 'target) and (value == 79)) =>> .r \"R\" }\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("match", b"M".as_slice()),
        ("empty", b"E".as_slice()),
        ("missing", b"A".as_slice()),
        ("extra", b"X".as_slice()),
        ("recursive", b"R".as_slice()),
    ] {
        let result = fully_evaluated_value(resolved_value_at_path(&value, &["asm", path]));
        assert_eq!(output_binary_result_list(&result), expected, "{path}");
    }
}

#[test]
fn refutable_variable_list_segments_match_the_middle_slice() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "asm.nested = list.pure <| do { .r [0,1,2,9] -> [0] ++ ([head] ++ tail) ++ [9]; ((head == 1) and (tail == [2])) =>> .r \"N\" }\n",
        "asm.exact = list.pure <| do { .r [0,1,2,9] -> [0] ++ ([1,2]) ++ [9]; .r \"E\" }\n",
        "asm.computed = list.pure <| do { .r [42,'foo,42,9] -> [key] ++ '.foo.[key] ++ [9]; .r \"C\" }\n",
        "asm.fallback = list.pure (.alt (do { .r [0,1,3,9] -> [0] ++ ([1,2]) ++ [9]; .r \"bad\" }) (.r \"F\"))\n",
        "asm.recursive = list.pure <| do { declare head,tail; .r [0,1,2,9] -> [0] ++ ([head] ++ tail) ++ [9]; ((head == 1) and (tail == [2])) =>> .r \"R\" }\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("nested", b"N".as_slice()),
        ("exact", b"E".as_slice()),
        ("computed", b"C".as_slice()),
        ("fallback", b"F".as_slice()),
        ("recursive", b"R".as_slice()),
    ] {
        let result = fully_evaluated_value(resolved_value_at_path(&value, &["asm", path]));
        assert_eq!(output_binary_result_list(&result), expected, "{path}");
    }
}

#[test]
fn effectful_patterns_transform_filter_guard_and_preserve_the_subject() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "increment value = .r (value + 1)\n",
        "add delta value = .r (delta + value)\n",
        "equals expected actual = expected == actual\n",
        "asm.view = list.pure <| do { .r 64 -> (increment -> viewed); (viewed == 65) =>> .r \"A\" }\n",
        "asm.backward_view = list.pure <| do { .r 65 -> (viewed <- increment); (viewed == 66) =>> .r \"B\" }\n",
        "asm.predicate = list.pure <| do { .r 67 -> (equals 67 kept); (kept == 67) =>> .r \"C\" }\n",
        "asm.general_as = list.pure <| do { .r [68,69] -> [left,right] as whole; ((list.at 0 whole == left) and (right == 69)) =>> .r \"D\" }\n",
        "asm.progressive_view = list.pure <| do { .r [1,71] -> [delta,(add delta -> sum)]; (sum == 72) =>> .r \"H\" }\n",
        "asm.guard = list.pure <| do { .r 72 -> (value when .r (value + 1) -> next and doubled = next + next and doubled == 146); (next == 73) =>> .r \"I\" }\n",
        "asm.backward_guard = list.pure <| do { .r 73 -> (value when next <- .r (value + 1) and next == 74); .r \"J\" }\n",
        "asm.wildcard_guard = list.pure <| do { .r 1 -> (_ when _); .r \"W\" }\n",
        "asm.view_fallback = list.pure (.alt (do { .r 1 -> (increment -> 3); .r \"bad\" }) (.r \"V\"))\n",
        "asm.predicate_fallback = list.pure (.alt (do { .r 1 -> (equals 2 _); .r \"bad\" }) (.r \"P\"))\n",
        "asm.guard_fallback = list.pure (.alt (do { .r 1 -> (_ when 1 == 2); .r \"bad\" }) (.r \"G\"))\n",
        "asm.recursive = list.pure <| do { declare viewed,next; .r 74 -> ((increment -> viewed) when .r (viewed + 1) -> next); ((viewed == 75) and (next == 76)) =>> .r \"R\" }\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("view", b"A".as_slice()),
        ("backward_view", b"B".as_slice()),
        ("predicate", b"C".as_slice()),
        ("general_as", b"D".as_slice()),
        ("progressive_view", b"H".as_slice()),
        ("guard", b"I".as_slice()),
        ("backward_guard", b"J".as_slice()),
        ("wildcard_guard", b"W".as_slice()),
        ("view_fallback", b"V".as_slice()),
        ("predicate_fallback", b"P".as_slice()),
        ("guard_fallback", b"G".as_slice()),
        ("recursive", b"R".as_slice()),
    ] {
        let result = fully_evaluated_value(resolved_value_at_path(&value, &["asm", path]));
        assert_eq!(output_binary_result_list(&result), expected, "{path}");
    }
}

#[test]
fn effectful_patterns_support_every_do_binding_direction() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "increment value = .r (value + 1)\n",
        "equals expected actual = expected == actual\n",
        "asm.forward_view = list.pure <| do { .r 64 -> (increment -> value); (value == 65) =>> .r \"A\" }\n",
        "asm.backward_view = list.pure <| do { (increment -> value) <- .r 65; (value == 66) =>> .r \"B\" }\n",
        "asm.value_view = list.pure <| do { (increment -> value) = 66; (value == 67) =>> .r \"C\" }\n",
        "asm.forward_predicate = list.pure <| do { .r 68 -> (equals 68 value); (value == 68) =>> .r \"D\" }\n",
        "asm.backward_predicate = list.pure <| do { (equals 69 value) <- .r 69; (value == 69) =>> .r \"E\" }\n",
        "asm.value_predicate = list.pure <| do { (equals 70 value) = 70; (value == 70) =>> .r \"F\" }\n",
        "asm.forward_as = list.pure <| do { .r [71] -> [value] as whole; (value == list.head whole) =>> .r \"G\" }\n",
        "asm.backward_as = list.pure <| do { [value] as whole <- .r [72]; (value == list.head whole) =>> .r \"H\" }\n",
        "asm.value_as = list.pure <| do { [value] as whole = [73]; (value == list.head whole) =>> .r \"I\" }\n",
        "asm.forward_guard = list.pure <| do { .r 74 -> (value when value == 74); .r \"J\" }\n",
        "asm.backward_guard = list.pure <| do { (value when value == 75) <- .r 75; .r \"K\" }\n",
        "asm.value_guard = list.pure <| do { (value when value == 76) = 76; .r \"L\" }\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("forward_view", b"A".as_slice()),
        ("backward_view", b"B".as_slice()),
        ("value_view", b"C".as_slice()),
        ("forward_predicate", b"D".as_slice()),
        ("backward_predicate", b"E".as_slice()),
        ("value_predicate", b"F".as_slice()),
        ("forward_as", b"G".as_slice()),
        ("backward_as", b"H".as_slice()),
        ("value_as", b"I".as_slice()),
        ("forward_guard", b"J".as_slice()),
        ("backward_guard", b"K".as_slice()),
        ("value_guard", b"L".as_slice()),
    ] {
        let result = fully_evaluated_value(resolved_value_at_path(&value, &["asm", path]));
        assert_eq!(output_binary_result_list(&result), expected, "{path}");
    }
}

#[test]
fn local_pattern_guards_require_unit_when_their_result_is_discarded() {
    let parsed = parse(concat!(
        "language g0\n",
        "api = {r:(\\x -> x),seq:(\\op k -> (k (op.eff api)).eff api)}\n",
        "bad = do { .r 1 -> (_ when .r 2); .r 3 }\n",
        "asm.result = bad.eff api\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let result = value_at_atom_path(&value, &["asm", "result"]).expect("result should exist");
    assert_eq!(
        fully_evaluated_error(result).to_string(),
        "pattern condition: unit expected, received Number"
    );
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
    assert_eq!(
        fully_evaluated_error(result).to_string(),
        "effect-only `do` statement: unit expected, received Binary"
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
    let parsed =
        parse("language g0\nasm.result = let { unused = 1; _suppressed = 2; _ = 3 } in 4\n");

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
fn source_locals_cannot_shadow_file_globals_regardless_of_declaration_order() {
    let parsed = parse(concat!(
        "language g0\n",
        "apply = \\_foo -> foo\n",
        "foo := 42\n",
    ));
    let lowered = lower_parsed_source(parsed, &CompileContext::default());
    let errors = lowered
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect::<Vec<_>>();

    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].line, 2);
    assert_eq!(
        errors[0].message,
        "local `_foo` shadows external `foo` defined on line 3"
    );
}

#[test]
fn source_locals_cannot_shadow_global_roots_used_elsewhere_in_the_file() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "other = \\_net_arity -> net_arity\n",
        "build = net_arity.member\n",
    ));
    let lowered = lower_parsed_source(parsed, &CompileContext::default());
    let errors = lowered
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect::<Vec<_>>();

    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].line, 3);
    assert_eq!(
        errors[0].message,
        "local `_net_arity` shadows external `net_arity` used by this file on line 4"
    );
}

#[test]
fn available_but_unselected_import_names_do_not_conflict_with_locals() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "identity = \\net_arity -> net_arity\n",
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
fn literal_paths_are_not_global_uses_but_computed_keys_are() {
    let literal = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "payload = { net_arity:1 }\n",
        "identity = \\net_arity -> net_arity\n",
    ));
    let literal = lower_parsed_source(literal, &CompileContext::default());
    assert!(
        literal
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error),
        "unexpected diagnostics: {:?}",
        literal.diagnostics
    );

    let computed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        ".[net_arity] = 1\n",
        "identity = \\net_arity -> net_arity\n",
    ));
    let computed = lower_parsed_source(computed, &CompileContext::default());
    assert!(computed.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Error
            && diagnostic.line == 4
            && diagnostic
                .message
                .contains("external `net_arity` used by this file on line 3")
    }));
}

/// A local has no prior definition, and `_x` cannot reach past a local
/// `x` to the namespace's prior `x`.
#[test]
fn prior_names_of_locals_are_errors() {
    for source in [
        "language g0\nf = \\item -> _item\n",
        "language g0\nf = let item = 1 in _item\n",
        "language g0\nf = g where\n  item = 1\n  g = _item\n",
        "language g0\nf v = match v as\n  item => _item\n",
        "language g0\nf = do\n  item <- .read\n  .r _item\n",
        "language g0\nobject o with\n  f = \\item -> _item\n",
    ] {
        let lowered = lower_parsed_source(parse(source), &CompileContext::default());
        assert!(
            lowered.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error
                    && diagnostic.message
                        == "`_item` names the local `item`, which has no prior definition"
            }),
            "{source}: {:?}",
            lowered.diagnostics
        );
    }
}

#[test]
fn object_body_locals_use_their_visible_namespace_index() {
    let parsed = parse(concat!(
        "language g0\n",
        "outer = 1\n",
        "object parent with\n",
        "  member = 2\n",
        "  apply = \\_member -> member\n",
        "object child with\n  alias _named\n",
        "  apply = \\_outer -> outer\n",
    ));
    let lowered = lower_parsed_source(parsed, &CompileContext::default());
    let errors = lowered
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect::<Vec<_>>();

    assert_eq!(errors.len(), 2);
    assert_eq!(errors[0].line, 5);
    assert_eq!(
        errors[0].message,
        "local `_member` shadows external `member` defined on line 4"
    );
    assert_eq!(errors[1].line, 8);
    assert_eq!(
        errors[1].message,
        "local `_outer` shadows external `outer` defined on line 2"
    );
}

#[test]
fn escaped_global_uses_are_recorded_in_the_scope_they_resolve_through() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "object child with\n",
        "  probe = ^net_arity\n",
        "  identity = \\_net_arity -> net_arity\n",
    ));
    let lowered = lower_parsed_source(parsed, &CompileContext::default());

    assert!(lowered.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Error
            && diagnostic.line == 5
            && diagnostic
                .message
                .contains("external `net_arity` used by this file on line 4")
    }));
}

#[test]
fn explicit_module_and_prior_references_do_not_select_unqualified_globals() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "module_probe = module.net_arity\n",
        "prior_probe = _net_arity\n",
        "identity = \\net_arity -> net_arity\n",
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
fn using_scopes_bind_temporary_namespaces_and_preserve_outer_escapes() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "outer = \"O\"\n",
        "namespace = {outer:\"I\",value:\"V\",emit:(\\value -> .r value)}\n",
        "choose value = using namespace in ^value\n",
        "shadow value = using namespace in value\n",
        "asm.scope = using namespace in outer ++ self.value ++ ^outer\n",
        "asm.local = choose \"L\"\n",
        "asm.shadow = shadow \"L\"\n",
        "asm.namespace = using {copied:outer} in copied\n",
        "asm.short = list.pure (using namespace do emit \"D\")\n",
        "asm.explicit = list.pure (using namespace in do emit \"E\")\n",
        "asm.prior = list.pure <| do { using namespace in (_self == {}); .r \"P\" }\n",
    ));
    assert_eq!(parsed.diagnostics, []);

    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);
    let value = evaluated_module_value(&context, &lowered);

    for (path, expected) in [
        ("scope", b"IVO".as_slice()),
        ("local", b"L".as_slice()),
        ("shadow", b"V".as_slice()),
        ("namespace", b"O".as_slice()),
    ] {
        let result = resolved_value_at_path(&value, &["asm", path]);
        assert_eq!(
            output_bytes(&fully_evaluated_value(result)),
            expected,
            "{path}"
        );
    }
    for (path, expected) in [
        ("short", b"D".as_slice()),
        ("explicit", b"E".as_slice()),
        ("prior", b"P".as_slice()),
    ] {
        let result = fully_evaluated_value(resolved_value_at_path(&value, &["asm", path]));
        assert_eq!(output_binary_result_list(&result), expected, "{path}");
    }
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
            expr: Some(SyntaxExpr::dict_literal(vec![
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
            expr: Some(SyntaxExpr::dict_literal(vec![
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
fn dictionary_puns_and_constructors_read_alike_everywhere() {
    let parsed = parse(concat!(
        "language g0\n",
        "left = \"L\"\n",
        "right = \"R\"\n",
        "punned = {:left,:right}\n",
        "tagged = {(tag:) left}\n",
        "outside = :left\n",
        "asm.result = punned.left ++ punned.right ++ tagged.tag ++ outside.left\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    assert!(matches!(
        &parsed.declarations[3].kind,
        DeclarationKind::Definition(DefinitionDecl {
            expr: Some(SyntaxExpr::DictLiteral(members)),
            ..
        }) if matches!(
            members.as_slice(),
            [
                DictMember::Path(left_path, left),
                DictMember::Path(right_path, right),
            ] if left_path == &[SyntaxKeyExpr::Atom("left".to_owned())]
                && right_path == &[SyntaxKeyExpr::Atom("right".to_owned())]
                && matches!(left, SyntaxExpr::Name(name) if name == "left")
                && matches!(right, SyntaxExpr::Name(name) if name == "right")
        )
    ));

    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);
    let value = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&value, &["asm", "result"]);
    assert_eq!(output_bytes(&fully_evaluated_value(result)), b"LRLL");

    // A pun in braces is a pun as anywhere: `{:tag left}` applies `tag:tag`.
    let applied = parse("language g0\napplied = {:tag left}\n");
    assert!(matches!(
        &applied.declarations[1].kind,
        DeclarationKind::Definition(DefinitionDecl {
            expr: Some(SyntaxExpr::DictLiteral(members)),
            ..
        }) if matches!(
            members.as_slice(),
            [DictMember::Dict(SyntaxExpr::Apply(function, _))]
                if matches!(function.as_ref(), SyntaxExpr::PathDict(..))
        )
    ));
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
            expr: Some(SyntaxExpr::dict_literal(vec![
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
fn delimited_groups_anchor_post_opening_contribution_lines() {
    let parsed = parse(concat!(
        "language g0\n",
        "dense = [1,2,3,4,\n",
        "  5,6,7,8,9,10]\n",
        "leading = [1,2\n",
        "  ,3,4\n",
        "  ,5,6]\n",
        "continued = [\n",
        "  make\n",
        "    long_argument,\n",
        "  next_member\n",
        "]\n",
        "tuple = (first,\n",
        "  second,\n",
        "  third)\n",
        "dict = {first:1,\n",
        "  second:2,\n",
        "  third:3}\n",
        "effect = do { .r ()\n",
        "  ; .r ()\n",
        "  ; .r () }\n",
    ));

    assert_eq!(parsed.diagnostics, []);
}

#[test]
fn delimited_group_diagnostics_distinguish_members_and_floors() {
    for (source, line, expected) in [
        (
            "language g0\nvalue = [1,2,\n  3,\n   4]\n",
            4,
            "expected content indentation 2",
        ),
        (
            "language g0\nvalue = [\nmember,\n  next\n]\n",
            3,
            "expression continuation is indented 0 spaces; expected at least 1",
        ),
    ] {
        let parsed = parse(source);
        assert!(
            parsed.diagnostics.iter().any(|diagnostic| {
                diagnostic.line == line && diagnostic.message.contains(expected)
            }),
            "`{source}` reported {:#?}",
            parsed.diagnostics
        );
    }
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
            expr: Some(SyntaxExpr::dict_literal(vec![SyntaxExpr::PathDict(
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
fn reports_mixed_arithmetic_and_boolean_operators_as_parse_errors() {
    let parsed = parse(concat!(
        "language g0\n",
        "add_multiply = 1 + 2 * 3\n",
        "multiply_divide = 1 * 2 / 3\n",
        "and_or = left and middle or right\n",
        "or_and = left or middle and right\n",
    ));

    for (line, left, right) in [
        (2, "+", "*"),
        (3, "*", "/"),
        (4, "and", "or"),
        (5, "or", "and"),
    ] {
        assert!(parsed.diagnostics.iter().any(|diagnostic| {
            diagnostic.line == line
                && diagnostic.message.contains(&format!(
                    "operators `{left}` and `{right}` have no precedence relationship"
                ))
        }));
    }
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
fn prefix_if_selects_first_success_and_fallback_with_progressive_captures() {
    let parsed = parse(concat!(
        "language g0\n",
        "asm.first = if _ then \"first\" else 1 / 0\n",
        "asm.fallback = if 42 = 41 then \"bad\" else \"fallback\"\n",
        "asm.capture = if .r \"captured\" -> value and (value == \"captured\") then value else \"bad\"\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("first", b"first".as_slice()),
        ("fallback", b"fallback".as_slice()),
        ("capture", b"captured".as_slice()),
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
fn postfix_if_runs_guards_before_its_textually_earlier_success_result() {
    let parsed = parse(concat!(
        "language g0\n",
        "asm.capture = value if value = \"captured\" else \"bad\"\n",
        "asm.fallback = \"bad\" if 42 = 41 else \"fallback\"\n",
        "asm.nested = \"bad\" if 1 = 2 else if _ then \"nested\" else \"worse\"\n",
        "asm.layout = \"layout\"\n",
        "  if\n",
        "    _\n",
        "  else\n",
        "    \"bad\"\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("capture", b"captured".as_slice()),
        ("fallback", b"fallback".as_slice()),
        ("nested", b"nested".as_slice()),
        ("layout", b"layout".as_slice()),
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
fn postfix_if_captures_do_not_enter_the_fallback_or_surrounding_scope() {
    for source in [
        "language g0\nasm.result = value if value = 1 else value\n",
        "language g0\nasm.result = [value if value = 1 else 0, value]\n",
    ] {
        let parsed = parse(source);
        assert_eq!(parsed.diagnostics, []);
        let lowered = lower_parsed_source(parsed, &CompileContext::default());
        assert!(
            lowered.diagnostics.iter().any(|diagnostic| diagnostic
                .message
                .contains("local `value` shadows external `value` used by this file")),
            "`{source}` should resolve the out-of-branch `value` as an external name: {:#?}",
            lowered.diagnostics
        );
    }
}

#[test]
fn raw_or_still_enumerates_while_conditionals_own_a_root_cut() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "raw = list.pure <| do\n",
        "  .r () or .r ()\n",
        "  .r \"R\"\n",
        "asm.raw = list.at 0 raw ++ list.at 1 raw\n",
        "asm.conditional = if (.r () or .r ()) then \"C\" else \"bad\"\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "raw"]
        ))),
        b"RR"
    );
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "conditional"]
        ))),
        b"C"
    );
}

#[test]
fn prefix_if_preserves_selected_result_laziness_and_guard_errors() {
    let parsed = parse(concat!(
        "language g0\n",
        "asm.lazy_error = if _ then 1 / 0 else \"fallback\"\n",
        "asm.non_unit_guard = if .r 42 then \"bad\" else \"fallback\"\n",
        "asm.unsupported_effect = if .heap.get [] then \"bad\" else \"fallback\"\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let lazy_error = fully_evaluated_error(
        value_at_atom_path(&value, &["asm", "lazy_error"])
            .expect("lazy error binding should exist"),
    );
    assert!(
        lazy_error.to_string().contains("divide"),
        "selected lazy result should retain its own error: {lazy_error}"
    );

    let non_unit = fully_evaluated_error(
        value_at_atom_path(&value, &["asm", "non_unit_guard"])
            .expect("non-unit guard binding should exist"),
    );
    assert_eq!(
        non_unit.to_string(),
        "conditional guard: unit expected, received Number"
    );

    let unsupported = fully_evaluated_error(
        value_at_atom_path(&value, &["asm", "unsupported_effect"])
            .expect("unsupported-effect binding should exist"),
    );
    assert_eq!(
        unsupported.to_string(),
        "application requires a function value, received Undefined",
        "the isolated conditional handler must identify its missing host capability as undefined"
    );
}

#[test]
fn tentative_conditional_results_return_explicitly_or_fall_through() {
    let parsed = parse(concat!(
        "language g0\n",
        "asm.if_fallback = if _ then? .fail else \"if fallback\"\n",
        "asm.if_return = if _ then? .r \"if return\" else \"bad\"\n",
        "asm.match_fallback = match 1 as { 1 =>? .fail; _ => \"match fallback\"; }\n",
        "asm.match_return = match 1 as { 1 =>? .r \"match return\"; _ => \"bad\"; }\n",
        "asm.nested = match when { _ when { _ =>? .fail; _ => \"nested fallback\"; }; _ => \"bad\"; }\n",
        "asm.non_effect = if _ then? 42 else \"bad\"\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("if_fallback", b"if fallback".as_slice()),
        ("if_return", b"if return".as_slice()),
        ("match_fallback", b"match fallback".as_slice()),
        ("match_return", b"match return".as_slice()),
        ("nested", b"nested fallback".as_slice()),
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

    let non_effect = fully_evaluated_error(
        value_at_atom_path(&value, &["asm", "non_effect"])
            .expect("non-effect tentative result should exist"),
    );
    assert_eq!(
        non_effect.to_string(),
        "list effect handler requires an effect dictionary, got Number(42)"
    );
}

#[test]
fn flat_match_supports_patterns_guards_and_ordered_fallback() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "increment value = .r (value + 1)\n",
        "equals expected actual = expected == actual\n",
        "asm.literal = match 42 as\n",
        "  41 => \"bad\"\n",
        "  42 => \"literal\"\n",
        "  _ => \"fallback\"\n",
        "asm.binary = match \"ABCD\" as\n",
        "  [65] ++ middle ++ [68] => middle\n",
        "  _ => \"bad\"\n",
        "asm.dict = match {selector:'target,target:68} as\n",
        "  {selector:key,{[key]:value}} when value == 68 => \"dict\"\n",
        "  _ => \"bad\"\n",
        "asm.view = match 64 as\n",
        "  increment -> value when value == 65 => \"view\"\n",
        "  _ => \"bad\"\n",
        "asm.predicate = match 66 as\n",
        "  equals 66 value => value\n",
        "  _ => 0\n",
        "asm.guards = match 65 as\n",
        "  value when next = value + 1 and checked <- .r next and .r checked -> final and final == 66 => \"guards\"\n",
        "  _ => \"bad\"\n",
        "asm.siblings = match 2 as\n",
        "  value when value == 1 => \"bad\"\n",
        "  value when value == 2 => \"siblings\"\n",
        "  _ => \"bad\"\n",
        "asm.braced = match 0 as {; 1 => \"bad\"; _ => \"braced\";}\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("literal", b"literal".as_slice()),
        ("binary", b"BC".as_slice()),
        ("dict", b"dict".as_slice()),
        ("view", b"view".as_slice()),
        ("guards", b"guards".as_slice()),
        ("siblings", b"siblings".as_slice()),
        ("braced", b"braced".as_slice()),
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
    assert_same_value_representation(
        &resolved_value_at_path(&value, &["asm", "predicate"]),
        &Value::Number(Number::from(66_i64)),
    );
}

#[test]
fn flat_match_exhaustion_and_selected_result_errors_are_distinct() {
    let parsed = parse(concat!(
        "language g0\n",
        "asm.exhausted =\n",
        "  match 1 as {}\n",
        "asm.exhausted_when =\n",
        "  match when {}\n",
        "asm.selected_error = match 1 as { 1 => 1 / 0; _ => \"fallback\"; }\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let exhausted = fully_evaluated_error(
        value_at_atom_path(&value, &["asm", "exhausted"])
            .expect("exhausted match binding should exist"),
    );
    assert_eq!(exhausted.to_string(), "match exhausted on line 3");

    let exhausted_when = fully_evaluated_error(
        value_at_atom_path(&value, &["asm", "exhausted_when"])
            .expect("exhausted guard-only match binding should exist"),
    );
    assert_eq!(exhausted_when.to_string(), "match exhausted on line 5");

    let selected_error = fully_evaluated_error(
        value_at_atom_path(&value, &["asm", "selected_error"])
            .expect("selected error binding should exist"),
    );
    assert!(
        selected_error.to_string().contains("divide"),
        "selected result errors must not fall through to a later match arm: {selected_error}"
    );
}

#[test]
fn open_match_forms_preserve_all_results_and_ordinary_exhaustion() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "asm.pure = match* 1 as { 1 => \"A\"; _ => \"B\"; }\n",
        "asm.pure_empty = match* 1 as {}\n",
        "host = try_match* 1 as { 1 => \"C\"; _ => \"D\"; }\n",
        "host_empty = try_match* 1 as {}\n",
        "host_cut = try_match* 1 as { 1 => \"E\"; _ => \"bad\"; }\n",
        "asm.host = list.pure host\n",
        "asm.host_empty = list.pure host_empty\n",
        "asm.host_cut = list.pure (.cut host_cut)\n",
        "asm.host_fallback = list.pure (.alt host_empty (.r \"F\"))\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("pure", b"AB".as_slice()),
        ("pure_empty", b"".as_slice()),
        ("host", b"CD".as_slice()),
        ("host_empty", b"".as_slice()),
        ("host_cut", b"E".as_slice()),
        ("host_fallback", b"F".as_slice()),
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
}

#[test]
fn hierarchical_match_shares_prefixes_and_falls_back_at_each_level() {
    let parsed = parse(concat!(
        "language g0\n",
        "import 'std\n",
        "asm.child = match [65,66] as\n",
        "  [first] ++ rest when first == 65 when\n",
        "    second = list.head rest and second == 66 => [first,second]\n",
        "    _ => \"bad child\"\n",
        "  _ => \"bad outer\"\n",
        "asm.outer = match 1 as\n",
        "  value when\n",
        "    value == 2 => \"bad child\"\n",
        "  1 => \"outer fallback\"\n",
        "  _ => \"bad outer\"\n",
        "asm.guard_only = match when\n",
        "  base = 64 when\n",
        "    .r (base + 1) -> value and value == 65 => \"guard only\"\n",
        "    _ => \"bad child\"\n",
        "  _ => \"bad outer\"\n",
        "asm.deep = match when { _ when { _ when { _ => \"deep\"; }; }; }\n",
        "asm.child_scope = match when\n",
        "  _ when\n",
        "    value = 1 and value == 0 => \"bad\"\n",
        "    value = 2 and value == 2 => \"child scope\"\n",
    ));
    assert_eq!(parsed.diagnostics, []);
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("child", b"AB".as_slice()),
        ("outer", b"outer fallback".as_slice()),
        ("guard_only", b"guard only".as_slice()),
        ("deep", b"deep".as_slice()),
        ("child_scope", b"child scope".as_slice()),
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
fn host_try_variants_use_reflection_state_and_rollback_failed_branches() {
    let source = r#"language g0
import 'std
refl.try_access = .heap.set ['visible] 42 =>> (try 42 <- .heap.get ['visible] then () else 1 / 0)
refl.try_rollback = (try (.heap.set ['try_rolled_back] "bad" =>> .fail) then 1 / 0 else ()) =>> (not (.heap.try_get ['try_rolled_back]))
refl.try_match_rollback = try_match 42 as {
  42 when (.heap.set ['rolled_back] "bad" =>> .fail) => ();
  42 when ({}) <- .heap.get ['rolled_back] => ();
}
refl.try_match_when = try_match when { 42 <- .heap.get ['visible] => (); }
refl.try_match_exhaustion = try (try_match 1 as {}) then 1 / 0 else ()
refl.try_tentative_rollback = (try _ then? (.heap.set ['tentative_try] "bad" =>> .fail) else ()) =>> (not (.heap.try_get ['tentative_try]))
refl.try_match_tentative_rollback = (try_match when {
  _ =>? (.heap.set ['tentative_match] "bad" =>> .fail);
  {} <- .heap.get ['tentative_match] => ();
})
ordinary = "ordinary"
"#;
    let (assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["host_try_test"], &[]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["ordinary"]),
        &Value::binary_from_text("ordinary"),
    );
    let crate::api::RuntimeReadiness::Ready(snapshot) = assembler.drain_reasoning() else {
        panic!("successful host choices should leave the runtime ready")
    };
    let report = snapshot
        .settle()
        .expect("unchanged runtime readiness should settle");
    assert!(report.task_failures().is_empty());
    assert!(report.killed_work().is_empty());
    assert!(
        take_reflection_diagnostics(&diagnostics).is_empty(),
        "successful host choices must not emit diagnostics"
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
fn abstract_objects_retain_specs_without_instantiating_members() {
    let parsed = parse(
        "language g0\nabstract object protocol with\n  text = \"Hello\"\nexpression = abstract object _ with\n  punctuation = \"!\"\nobserved_name = expression.spec.name\nobject concrete extends expression, protocol with\n  result = text ++ punctuation\nasm.result = concrete.result\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let Value::Dict(protocol) = resolved_value_at_path(&value, &["protocol"]) else {
        panic!("abstract declaration should evaluate to a dictionary");
    };
    assert_eq!(protocol.iter().count(), 1);
    assert!(protocol.get(&*keys::SPEC).is_some());
    assert!(protocol.get(&Key::atom_from_text("text")).is_none());

    let Value::Dict(expression) = resolved_value_at_path(&value, &["expression"]) else {
        panic!("declare expression should evaluate to a dictionary");
    };
    let Some(expression_spec) = expression
        .get(&*keys::SPEC)
        .map(|value| value.duplicate_for_test(&crate::compiler::test_value_factory()))
    else {
        panic!("declare expression should retain its specification");
    };
    let Value::Dict(expression_spec) = fully_evaluated_value(expression_spec) else {
        panic!("declare expression specification should evaluate to a dictionary");
    };
    assert!(expression_spec.get(&*keys::NAME).is_none());
    assert_same_value_representation(
        &resolved_value_at_path(&value, &["observed_name"]),
        &Value::Dict(Dict::new_sync()),
    );
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello!"
    );
}

#[test]
fn abstract_and_instance_extensions_choose_the_final_realization() {
    let parsed = parse(
        "language g0\nobject base with\n  text = \"Hello\"\nextend abstract base with\n  text := _text ++ \", World\"\nobject derived extends base with\n  text := _text ++ \"!\"\nabstract object protocol with\n  text = \"Again\"\nextend protocol with {}\nasm.result = derived.text ++ \" \" ++ protocol.text\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let Value::Dict(base) = resolved_value_at_path(&value, &["base"]) else {
        panic!("declare extension should leave a spec-only dictionary");
    };
    assert_eq!(base.iter().count(), 1);
    assert!(base.get(&*keys::SPEC).is_some());
    assert!(base.get(&Key::atom_from_text("text")).is_none());
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"Hello, World! Again"
    );
}

#[test]
fn nested_abstract_objects_participate_in_hierarchical_inheritance() {
    let parsed = parse(
        "language g0\nobject outer with\n  abstract object protocol with\n    text = \"Hello, World!\"\n  object concrete extends protocol\nasm.result = outer.concrete.text\n",
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
fn object_declaration_parents_are_evaluated_as_expressions() {
    let parsed = parse(
        "language g0\nselect x = x\nobject base with\n  text = \"Hello, World!\"\nobject child extends select base\nasm.result = child.text\n",
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
        "language g0\nprefix = \"Hello\"\nhello = object \"hello\" with\n  alias _h\n  target = \"World\"\n  text = prefix ++ \", \" ++ h.target ++ \"!\"\nasm.result = hello.text\n",
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
        "language g0\nprefix = \"Hello\"\nobject hello with\n  alias self\n  prefix = \"Nope\"\n  target = \"World\"\n  text = module.prefix ++ \", \" ++ self.target ++ \"!\"\nasm.result = hello.text\n",
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

/// `alias self` is an object's default and localizes its names; `alias _`
/// names nothing and leaves its names to the host scope.
#[test]
fn object_aliases_self_and_none_choose_the_name_scope() {
    let parsed = parse(concat!(
        "language g0\n",
        "greeting = \"Hello\"\n",
        "suffix = \"!\"\n",
        "object localized with\n",
        "  alias self\n",
        "  target = \"World\"\n",
        "  text = ^greeting ++ \", \" ++ target\n",
        "object hosted with\n",
        "  alias _\n",
        "  text = localized.text ++ suffix\n",
        "asm.result = hosted.text\n",
    ));
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
fn object_dependencies_reject_distinct_specs_with_the_same_name() {
    let parsed = parse(
        "language g0\nleft = object \"shared\" with\n  left = \"Left\"\nright = object \"shared\" with\n  right = \"Right\"\nobject child extends left, right\nasm.result = child\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let error = fully_evaluated_error(
        value_at_atom_path(&value, &["asm", "result"]).expect("assembly result should exist"),
    );
    assert!(
        error
            .to_string()
            .contains("identifies multiple specifications"),
        "unexpected error: {error}"
    );
}

#[test]
fn object_dependencies_can_explicitly_inherit_from_dictionaries() {
    let parsed = parse(
        "language g0\nimport 'std\nbase = { hello:\"Hello\", target:\"Base\" }\nobject child extends object_from_dict base with\n  target := \"World\"\n  text = hello ++ \", \" ++ target ++ \"!\"\nasm.result = child.text\n",
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
fn empty_dictionaries_explicitly_convert_to_empty_anonymous_objects() {
    let parsed = parse(
        "language g0\nimport 'std\nobject child extends object_from_dict {} with\n  text = \"Hello, World!\"\nasm.result = child.text\n",
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
fn object_parents_reject_implicit_dictionary_conversion() {
    for parent in ["missing", "base"] {
        let source = format!(
            "language g0\nbase = {{ text:\"not an object\" }}\nobject child extends {parent} with\n  text = \"Hello\"\nasm.result = child.text\n"
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_parsed_source(parse(&source), &context);
        assert_eq!(lowered.diagnostics, [], "{parent}");

        let value = evaluated_module_value(&context, &lowered);
        let result =
            value_at_atom_path(&value, &["asm", "result"]).expect("result binding should exist");
        let error = crate::evaluation::EvalContext::evaluate_compatibility_whnf(
            &test_eval_context(),
            &result,
        )
        .expect_err_without_debug("plain dictionary parent should fail");
        assert!(
            error
                .to_string()
                .contains("use `object_from_dict` to convert a dictionary"),
            "{parent}: {error}"
        );
    }
}

#[test]
fn object_from_dict_rejects_existing_objects() {
    let parsed =
        parse("language g0\nimport 'std\nobject base\nasm.result = object_from_dict base\n");
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    let result = value_at_atom_path(&value, &["asm", "result"]).expect("result should exist");
    let error =
        crate::evaluation::EvalContext::evaluate_compatibility_whnf(&test_eval_context(), &result)
            .expect_err_without_debug("converting an existing object should fail");
    assert_eq!(
        error.to_string(),
        "object_from_dict requires a plain dictionary, not an object"
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
        "language g0\nidx = 42\nobject hello with\n  alias h\n  .[idx] = \"Hello\"\n  text = h.[idx] ++ \", World!\"\nasm.result = hello.text\n",
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
fn hierarchical_extend_declarations_preserve_target_and_prior_scope() {
    let parsed = parse(
        "language g0\nobject parent with\n  suffix = \"!\"\n  object child with\n    text = \"Hello\"\n  extend child with\n    alias prior\n    text := _prior.text ++ suffix\nasm.result = parent.child.text\n",
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
        b"Hello!"
    );
    assert!(
        value_at_atom_path(&value, &["child"]).is_none(),
        "the nested extend target must remain under its enclosing object"
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
        "language g0\nobject base with\n  count = 0\ninc = object _ with\n  count := _count + 1\nobject child extends inc, inc, inc, base\nasm.result = child.count\n",
    );
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["asm", "result"])),
        &Value::Number(3.into()),
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
        "language g0\nobject base with\n  code = \"\"\nadd_a = object _ with\n  code := _code ++ \"A\"\nadd_b = object _ with\n  code := _code ++ \"B\"\nobject child extends add_a, add_b, base\nasm.result = child.code\n",
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
fn with_expressions_reinstantiate_adapting_objects() {
    let parsed = parse(
        "language g0\nobject environment with\n  alias self\n  adapted = self.language\nconfigured = environment with\n  .['language] = \"g0\"\nasm.result = configured.adapted\n",
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
        b"g0"
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
        "language g0\nprefix = \"Hello\"\nobject hello with\n  alias h\n  target = \"World\"\n  text = prefix ++ \", \" ++ h.target ++ \"!\"\nasm.result = hello.text\n",
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
        "language g0\nprefix = \"Hello\"\nobject hello with\n  alias _h\n  target = \"World\"\n  text = prefix ++ \", \" ++ h.target ++ \"!\"\nasm.result = hello.text\n",
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
        "language g0\nsuffix = \"!\"\nobject hello with\n  text = \"Hello, World\"\nextend hello with\n  alias h\n  text := _h.text ++ suffix\nasm.result = hello.text\n",
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
        "language g0\nsuffix = \"!\"\nobject hello with\n  text = \"Hello, World\"\nextend hello with\n  alias _h\n  text := _h.text ++ suffix\nasm.result = hello.text\n",
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
        "language g0\nsuffix = \"!\"\nbase = { text:\"Hello, World\" }\nd = base with\n  alias b\n  text := _b.text ++ suffix\n  result = b.text\nasm.result = d.result\n",
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
        "language g0\nsuffix = \"!\"\nbase = { text:\"Hello, World\" }\nd = base with\n  alias _b\n  text := _b.text ++ suffix\n  result = b.text\nasm.result = d.result\n",
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
    let parsed = parse(
        "language g0\nd = {} with\n  alias unused\n  x = 1\ne = {} with\n  alias _unused\n  x = 1\n",
    );

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
            && diag.line == 3
            && diag.message == "unused local `unused`"
    }));
}

#[test]
fn dictionary_with_self_alias_uses_object_style_scope() {
    let parsed = parse(
        "language g0\nsuffix = \"!\"\nbase = { text:\"Hello, World\" }\nd = base with\n  alias self\n  text := _text ++ ^suffix\nasm.result = d.text\n",
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
        "language g0\nasm.result = let { hello = \"Hello\"; world = \"World\" } in hello ++ \", \" ++ world ++ \"!\"\n",
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
        "language g0\nasm.result = hello ++ \", \" ++ world ++ \"!\" where { hello = \"Hello\"; world = \"World\" }\n",
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
fn empty_braced_let_and_where_groups_are_identity_sugar() {
    let parsed = parse(
        "language g0\nleft = let {} in \"Hello\"\nright = \", World!\" where {}\nasm.result = left ++ right\n",
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
fn chained_where_groups_make_later_groups_visible_to_earlier_groups() {
    let parsed = parse(
        "language g0\nasm.result = first where first = second where second = \"Hello, World!\"\n",
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

/// One `let` or `where` group is mutually recursive: a binding may name a
/// sibling defined after it, itself, or a sibling that names it back.
#[test]
fn binding_groups_are_mutually_recursive() {
    let parsed = parse(concat!(
        "language g0\n",
        "forward = let { y = x; x = 42 } in y\n",
        "behind = y where { y = x + 1; x = 1 }\n",
        "count = go 5 where go = \\n -> if n == 0 then 0 else 1 + go (n - 1)\n",
        "parity = let { even = \\n -> if n == 0 then \"even\" else odd (n - 1); ",
        "odd = \\n -> if n == 0 then \"odd\" else even (n - 1) } in even 7\n",
        "mixed = let { a = [1, b]; b = 2; c = a } in c\n",
        "asm.result = if [forward, behind, count, mixed] == [42, 2, 5, [1, 2]] ",
        "then parity else \"wrong\"\n",
    ));
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"odd"
    );
}

/// A local binding takes parameters as a module definition does: its value
/// becomes a lambda over them.
#[test]
fn local_bindings_take_parameters_as_definitions_do() {
    let parsed = parse("language g0\nsum = let { add a b = a + b } in add 1 2\n");
    assert_eq!(parsed.diagnostics, []);
    let DeclarationKind::Definition(definition) = &parsed.declarations[1].kind else {
        panic!("a definition should parse");
    };
    let Some(SyntaxExpr::Let { bindings, .. }) = &definition.expr else {
        panic!("the definition should be a let expression");
    };
    assert!(matches!(
        &bindings[..],
        [(name, SyntaxExpr::Lambda(parameters, _))]
            if name == "add" && parameters == &["a", "b"]
    ));

    let parsed = parse(concat!(
        "language g0\n",
        "count = go 5 where go k = if k == 0 then 0 else 1 + go (k - 1)\n",
        "parity = answer 7\n",
        "  where\n",
        "    even n = if n == 0 then \"even\" else odd (n - 1)\n",
        "    odd n = if n == 0 then \"odd\" else even (n - 1)\n",
        "    answer n = even n\n",
        "asm.result = if count == 5 then parity else \"wrong\"\n",
    ));
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);
    let value = evaluated_module_value(&context, &lowered);
    assert_eq!(
        output_bytes(&fully_evaluated_value(resolved_value_at_path(
            &value,
            &["asm", "result"]
        ))),
        b"odd"
    );

    for (source, message) in [
        (
            "language g0\nr = let { go (k) = k } in go 1\n",
            "local binding `go` parameters must be local names",
        ),
        (
            "language g0\nr = let { 1 x = x } in 1\n",
            "invalid local binding name `1 x`",
        ),
    ] {
        let messages = parse(source)
            .diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect::<Vec<_>>();
        assert_eq!(messages, [message], "{source}");
    }
}

/// A group whose values only name each other has no value: evaluating it
/// fails as a lazy dependency cycle.
#[test]
fn binding_group_cycles_fail_as_lazy_cycles() {
    let parsed =
        parse("language g0\nlone = let { x = x } in x\npair = let { x = y; y = x } in x\n");
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for name in ["lone", "pair"] {
        let binding = value_at_atom_path(&value, &[name]).expect("binding should exist");
        let error = fully_evaluated_error(binding).to_string();
        assert!(error.contains("lazy dependency cycle"), "{name}: {error}");
    }
}

/// A binding counts as used when the body reaches it, directly or through
/// the siblings that name it.
#[test]
fn binding_group_siblings_count_as_uses() {
    let parsed = parse("language g0\nasm.result = let { a = 1; b = a; c = d; d = 2 } in b\n");

    let warnings = parsed
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert_eq!(warnings, ["unused local `c`", "unused local `d`"]);
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
    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["asm", "sum"])),
        &Value::Number(n(50)),
    );
    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["asm", "diff"])),
        &Value::Number(n(34)),
    );
    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["asm", "full_sum"])),
        &Value::Number(n(50)),
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
        "language g0\napi = { r:(\\x -> x), seq:(\\operation continuation -> (continuation (operation.eff api)).eff api) }\nop = .r \"Hello\"\nk x = .r (x ++ \", World!\")\nf x = .r (x ++ \", World\")\ng x = .r (x ++ \"!\")\nop_unit = .r ()\nbind_section = (>>= k)\nbind_function = (>>=)\nthen_function = (=>>)\nkleisli_function = (>=>)\nasm.bind = (op >>= k).eff api\nasm.bind_section = (bind_section op).eff api\nasm.bind_function = (bind_function op k).eff api\nasm.kleisli = ((f >=> g) \"Hello\").eff api\nasm.kleisli_function = (kleisli_function f g \"Hello\").eff api\nasm.then = (op_unit =>> .r \"Hello, World!\").eff api\nasm.then_function = (then_function op_unit (.r \"Hello, World!\")).eff api\n",
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
        "api = { r:(\\value -> {value:value, trace:\"R\"}), seq:(\\operation continuation -> (\\first -> (\\second -> {value:second.value, trace: first.trace ++ second.trace}) ((continuation first.value).eff api)) (operation.eff api)) }\n",
        "marked tag value = {eff:(\\_effect_api -> {value:value, trace:tag})}\n",
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
        match crate::evaluation::EvalContext::evaluate_compatibility_whnf(
            &test_eval_context(),
            &result,
        ) {
            Ok(next @ (Value::Lazy(_) | Value::Promised(_))) => result = next,
            Ok(other) => panic!(
                "non-unit result should not evaluate to {}",
                diagnostic_kind_name(&other)
            ),
            Err(err) => break err,
        }
    };
    assert_eq!(
        err.to_string(),
        "`=>>` discarded result: unit expected, received Binary"
    );
}

#[test]
fn dict_literals_introduce_their_members_in_order() {
    // `dict-path-fusion`: a literal introduces its members in written order,
    // the inverse of a dict pattern's takes. Paths sharing a prefix fuse, a
    // dict member is a shallow union at the root, a parenthesized tag is a
    // dict member, and `path?:` introduces a list's one value or nothing.
    let parsed = parse(concat!(
        "language g0\nimport 'std\n",
        "one n = n\n",
        "d = { a:{ b:1 } }\n",
        "fused = { a.b:(one 1), a.c:2, [1, 2]:3, [1, 4]:4 }\n",
        "refined = { d, a.c:2 }\n",
        "lazy = { a:(dict.insert ['b] 1 {}), a.c:2 }\n",
        "optional = { x?:[], y?:[5] }\n",
        "alone = z?:[6]\n",
        "empty = { e:{} }\n",
        "asm.fused = list.pure ((fused.a.b == 1 and fused.a.c == 2 and fused.[1, 4] == 4) =>> .r \"F\")\n",
        "asm.refined = list.pure ((refined.a.b == 1 and refined.a.c == 2) =>> .r \"R\")\n",
        "asm.lazy = list.pure ((lazy.a.b == 1 and lazy.a.c == 2) =>> .r \"L\")\n",
        "asm.optional = list.pure ((not (dict.defines ['x] optional) and optional.y == 5) =>> .r \"O\")\n",
        "asm.alone = list.pure ((alone.z == 6) =>> .r \"A\")\n",
        "asm.empty = list.pure (dict.defines ['e] empty =>> .r \"E\")\n",
        "shallow = dict.lookup ['nested] { { nested:{ x:(one 1) } }, { nested:{ y:2 } } }\n",
        "grouped = dict.lookup ['a] { (a.b:(one 1)), (a.c:2) }\n",
        "many = { m?:[1, 2] }\n",
        "twice = dict.introduce ['a] 2 { a:(one 1) }\n",
    ));
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("fused", b"F".as_slice()),
        ("refined", b"R"),
        ("lazy", b"L"),
        ("optional", b"O"),
        ("alone", b"A"),
        ("empty", b"E"),
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
    for (path, expected) in [
        ("shallow", "both dicts define `nested`"),
        ("grouped", "both dicts define `a`"),
        ("many", "takes `[]` or `[Value]`"),
        ("twice", "`a` is already defined"),
    ] {
        let binding = value_at_atom_path(&value, &[path]).expect("binding should exist");
        let error = fully_evaluated_error(binding);
        assert!(error.to_string().contains(expected), "{path}: {error}");
    }

    // What the compiler can see conflict, it reports: a path introduced
    // twice, a path after a deeper one under it, and a pattern that takes
    // a path before a deeper one it holds.
    for (source, expected) in [
        ("x = { a:(one 1), a:2 }\n", "introduces `a` twice"),
        ("x = { a.c:2, a:(one 1) }\n", "write `a` first"),
        (
            "x = match { a:{ c:1 } } as\n  { a:a, a.c:c } => c\n  _ => 0\n",
            "take `a.c` first",
        ),
    ] {
        let parsed = parse(&format!("language g0\nimport 'std\none n = n\n{source}"));
        let lowered = lower_parsed_source(parsed, &CompileContext::default());
        assert!(
            lowered
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "{source}: {:?}",
            lowered.diagnostics
        );
    }
}

#[test]
fn std_dict_path_operations_keep_an_empty_dict_as_a_value() {
    // `dict-presence-ops`: path first, dict last; `defines` answers as
    // comparisons do; `insert` keeps `{}` as a value, and `remove` prunes
    // only the dicts it empties.
    let parsed = parse(concat!(
        "language g0\nimport 'std\n",
        "d = { a:{ b:1, c:2 }, x:3 }\n",
        "with_empty = dict.insert ['e] {} d\n",
        "only_v = dict.insert ['k, 'v] 1 d\n",
        "v_and_w = dict.insert ['k, 'w] {} only_v\n",
        "asm.lookup = list.pure ((dict.lookup ['a, 'b] d == 1) =>> .r \"L\")\n",
        "asm.lookup_root = list.pure ((dict.lookup [] d == d) =>> .r \"P\")\n",
        "asm.defined = list.pure (dict.defines ['a, 'c] d =>> .r \"D\")\n",
        "asm.undefined = list.pure (dict.defines ['a, 'z] d =>> .r \"bad\")\n",
        "asm.empty_value = list.pure (dict.defines ['e] with_empty =>> .r \"E\")\n",
        "asm.default_absent = list.pure ((dict.default ['a, 'z] 7 d == 7) =>> .r \"F\")\n",
        "asm.default_present = list.pure ((dict.default ['a, 'b] 7 d == 1) =>> .r \"G\")\n",
        "asm.insert = list.pure ((dict.lookup ['a, 'z] (dict.insert ['a, 'z] 9 d) == 9) =>> .r \"I\")\n",
        "asm.insert_deep = list.pure ((dict.lookup ['n, 'm] (dict.insert ['n, 'm] 4 d) == 4) =>> .r \"J\")\n",
        "asm.remove = list.pure ((dict.remove ['a, 'b] d == { a:{ c:2 }, x:3 }) =>> .r \"R\")\n",
        "asm.remove_absent = list.pure ((dict.remove ['a, 'z] d == d) =>> .r \"S\")\n",
        "asm.pruned = list.pure (dict.defines ['k] (dict.remove ['k, 'v] only_v) =>> .r \"bad\")\n",
        "asm.kept = list.pure (dict.defines ['k, 'w] (dict.remove ['k, 'v] v_and_w) =>> .r \"K\")\n",
        "asm.update = list.pure ((dict.lookup ['x] (dict.update ['x] (\\v -> v + 1) d) == 4) =>> .r \"U\")\n",
        "asm.union = list.pure ((dict.lookup ['q] (dict.union { p:1, q:2 } { p:1 }) == 2) =>> .r \"N\")\n",
        "lazy = { l:(dict.insert ['y] 1 {}) }\n",
        "asm.insert_lazy = list.pure ((dict.lookup ['l, 'z] (dict.insert ['l, 'z] 5 lazy) == 5) =>> .r \"Z\")\n",
        "asm.remove_lazy = list.pure (dict.defines ['l] (dict.remove ['l, 'y] lazy) =>> .r \"bad\")\n",
        "missing = dict.lookup ['a, 'z] d\n",
        "shared = dict.lookup ['p] (dict.union { p:1 } { p:1 })\n",
        "shallow = dict.lookup ['a, 'b] (dict.union { a:{ b:1 } } { a:{ c:2 } })\n",
        "through = dict.defines ['x, 'y] d\n",
    ));
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    for (path, expected) in [
        ("lookup", b"L".as_slice()),
        ("lookup_root", b"P"),
        ("defined", b"D"),
        ("undefined", b""),
        ("empty_value", b"E"),
        ("default_absent", b"F"),
        ("default_present", b"G"),
        ("insert", b"I"),
        ("insert_deep", b"J"),
        ("remove", b"R"),
        ("remove_absent", b"S"),
        ("pruned", b""),
        ("kept", b"K"),
        ("update", b"U"),
        ("union", b"N"),
        ("insert_lazy", b"Z"),
        ("remove_lazy", b""),
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
    for (path, expected) in [
        ("missing", "dict.lookup path `a.z` is undefined"),
        ("shared", "dict.union: both dicts define `p`"),
        ("shallow", "dict.union: both dicts define `a`"),
        (
            "through",
            "dict.defines path `x` traverses a non-dictionary value",
        ),
    ] {
        let binding = value_at_atom_path(&value, &[path]).expect("binding should exist");
        let error = fully_evaluated_error(binding);
        assert!(error.to_string().contains(expected), "{path}: {error}");
    }
}

#[test]
fn comparisons_and_boolean_operators_evaluate_as_effects() {
    let parsed = parse(
        "language g0\nimport 'std\ntuple_left = { tuple:[1,2] }\ntuple_right = { tuple:[1,3] }\nasm.gt = list.pure ((3 > 2) =>> .r \"G\")\nasm.ge = list.pure ((3 >= 3) =>> .r \"E\")\nasm.eq = list.pure ((3 == 3) =>> .r \"Q\")\nasm.ne = list.pure ((3 <> 4) =>> .r \"N\")\nasm.le = list.pure ((3 =< 3) =>> .r \"L\")\nasm.lt = list.pure ((2 < 3) =>> .r \"T\")\nasm.fail = list.pure ((3 < 2) =>> .r \"bad\")\nasm.chain = list.pure ((1 < 2 =< 2 <> 3) =>> .r \"H\")\nasm.chain_fail = list.pure ((1 < 3 < 2) =>> .r \"bad\")\nasm.chain_raw = 1 < (2 + 0) < 3\nasm.list = list.pure (([1,2] < [1,3]) =>> .r \"S\")\nasm.binary_list = list.pure ((\"AB\" == [65,66]) =>> .r \"B\")\nasm.string_list = list.pure (([\"A\",\"B\"] == [\"A\",\"B\"]) =>> .r \"V\")\nasm.string_order = list.pure (([\"A\",\"B\"] < [\"A\",\"C\"]) =>> .r \"W\")\nasm.nested_list = list.pure (([\"A\",\"B\"] <> \"AB\") =>> .r \"X\")\nasm.list_tuple = list.pure (([1,2] <> tuple_left) =>> .r \"Y\")\nasm.tuple = list.pure ((tuple_left < tuple_right) =>> .r \"U\")\nasm.dict = list.pure (({ a:1, b:{} } == { a:1 }) =>> .r \"D\")\nasm.and_result = list.pure ((3 > 2 and \"A\" == [65]) =>> .r \"A\")\nasm.or_result = list.pure ((3 < 2 or 3 == 3) =>> .r \"O\")\nasm.not_true = list.pure ((not (3 > 2)) =>> .r \"bad\")\nasm.not_false = list.pure ((not (3 < 2)) =>> .r \"F\")\nasm.could_true = list.pure ((could (.alt (.fail) (3 == 3))) =>> .r \"C\")\nasm.could_false = list.pure ((could (.fail)) =>> .r \"bad\")\n",
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
        ("and_result", b"A"),
        ("or_result", b"O"),
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
        "language g0\nimport 'list as list\nchoices = (.alt (.r \"A\") (.alt (.fail) (.r \"B\"))) >>= (\\x -> .r (x ++ \"!\"))\ncut = .cut (.alt (.r \"C\") (.r \"D\"))\ncut_bad = .cut (.alt (.r \"G\") 42)\ncut_seq_bad = .cut ((.alt (.r \"S\") 42) >>= (\\x -> .r (x ++ \"!\")))\nobject_effect = { eff:(.r \"E\").eff, meta:1 }\nfixed = .fix (\\loop -> .r { text:\"F\", cycle:loop })\nasm.choices = list.pure choices\nasm.cut = list.pure cut\nasm.cut_fail = list.pure (.cut (.fail))\nasm.cut_bad = list.pure cut_bad\nasm.cut_seq_bad = list.pure cut_seq_bad\nasm.object_result = list.pure object_effect\nasm.fixed = (list.head (list.pure fixed)).text\nasm.head = list.head \"Hi\"\nasm.tail = list.tail \"Hi\"\n",
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
            &["asm", "object_result"]
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
    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["asm", "head"])),
        &Value::Number(n(72)),
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
        parse("language g0\nd = { hello:\"Hello\", world: other ++ \"!\" }\nother = \"World\"\n");
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
        "constructor = (made.nested:)\n",
        "constructed = constructor \"D\"\n",
        "dynamic_constructor = ((['built] ++ tail):)\n",
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
    crate::compiler::test_value_factory().assert_same_representation_for_test(
        &value.get_atom_path(&[Atom::from_key(&Key::binary_from_text("hello"))]),
        &Some(&Value::binary_from_text("Hello")),
    );
    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["world"])),
        &Value::binary_from_text("World"),
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
    let std =
        crate::evaluation::EvalContext::evaluate_compatibility_whnf(&test_eval_context(), std)
            .expect("std import should evaluate to a dictionary");
    let floor = value
        .get_atom_path(&[Atom::from_key(&Key::binary_from_text("floor"))])
        .expect("inline math import should expose floor");
    let mod_fn = value
        .get_atom_path(&[Atom::from_key(&Key::binary_from_text("mod"))])
        .expect("inline math import should expose mod");
    let list_len_import = crate::evaluation::EvalContext::evaluate_compatibility_whnf(
        &test_eval_context(),
        &core_global_access(
            &context,
            vec![Key::atom_from_text("list"), Key::atom_from_text("len")],
        ),
    )
    .expect("list.len import should resolve");
    let list_spec = crate::evaluation::EvalContext::evaluate_compatibility_whnf(
        &test_eval_context(),
        &core_global_access(
            &context,
            vec![Key::atom_from_text("list"), Key::atom_from_text("spec")],
        ),
    )
    .expect("list.spec import should resolve");
    let list_head_import = crate::evaluation::EvalContext::evaluate_compatibility_whnf(
        &test_eval_context(),
        &core_global_access(
            &context,
            vec![Key::atom_from_text("list"), Key::atom_from_text("head")],
        ),
    )
    .expect("list.head import should resolve");
    let list_tail_import = crate::evaluation::EvalContext::evaluate_compatibility_whnf(
        &test_eval_context(),
        &core_global_access(
            &context,
            vec![Key::atom_from_text("list"), Key::atom_from_text("tail")],
        ),
    )
    .expect("list.tail import should resolve");
    let list_pure_import = crate::evaluation::EvalContext::evaluate_compatibility_whnf(
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
            assert!(
                std.get(&Key::atom_from_text("interaction_net_from_netlist"))
                    .is_none(),
                "strict semantic netlist replay must remain evaluator-private"
            );
            assert!(matches!(
                std.get(&Key::atom_from_text("net_arity")),
                Some(Value::Builtin(crate::core::Builtin::NetArity))
            ));
            assert!(matches!(
                std.get(&Key::atom_from_text("object_from_dict")),
                Some(Value::Builtin(crate::core::Builtin::ObjectFromDict))
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
            let Value::Dict(std_list) =
                crate::evaluation::EvalContext::evaluate_compatibility_whnf(
                    &test_eval_context(),
                    std_list,
                )
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
            let values = context.values();
            (
                anno.duplicate_for_test(values),
                not.duplicate_for_test(values),
                could.duplicate_for_test(values),
                len.duplicate_for_test(values),
                split.duplicate_for_test(values),
                split_end.duplicate_for_test(values),
                head.duplicate_for_test(values),
                tail.duplicate_for_test(values),
                pure.duplicate_for_test(values),
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
        crate::evaluation::EvalContext::evaluate_compatibility_whnf(&test_eval_context(), &std_not)
            .unwrap(),
        Value::Function(_) | Value::Net(_)
    ));
    assert!(matches!(
        crate::evaluation::EvalContext::evaluate_compatibility_whnf(
            &test_eval_context(),
            &std_could
        )
        .unwrap(),
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
    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["from_std"])),
        &Value::Number(n(i64::from(b'B'))),
    );
    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["from_list"])),
        &Value::Number(n(20)),
    );
}

#[test]
fn constructs_and_observes_an_interaction_net_from_source_effects() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "answer_net = interaction_net <| do\n",
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
fn interaction_net_function_bind_lists_its_result_before_its_argument() {
    // Binds join crossed, so a function bind lists `[function, result,
    // argument]` while an application lists `[application, argument,
    // result]`. A constant function distinguishes the two orders.
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "constant = interaction_net <| do\n",
            "  .bind -> function\n",
            "  .copy 0 -> drop\n",
            "  .data \"Hello, World!\" -> greeting\n",
            "  .wire (list.head (list.tail function)) (list.head greeting)\n",
            "  .wire (list.head (list.tail (list.tail function))) (list.head drop)\n",
            "  .r (list.head function)\n",
            "asm.result = net_arity 1 constant \"ignored\"\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&definitions, &["asm", "result"]);
    assert_eq!(output_bytes(&result), b"Hello, World!");
}

#[test]
fn interaction_net_applies_a_self_linked_identity_bind() {
    // The applied identity's variable is wired to its own body, so the Bind
    // join rewrite meets a link between two ports of its own active pair.
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "redex = interaction_net <| do\n",
            "  .bind -> app\n",
            "  .bind -> id\n",
            "  .data \"Hello, World!\" -> arg\n",
            "  .wire (list.head app) (list.head id)\n",
            "  .wire (list.head (list.tail id)) (list.head (list.tail (list.tail id)))\n",
            "  .wire (list.head (list.tail app)) (list.head arg)\n",
            "  .r (list.head (list.tail (list.tail app)))\n",
            "asm.result = net_arity 0 redex\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&definitions, &["asm", "result"]);
    assert_eq!(output_bytes(&result), b"Hello, World!");
}

#[test]
fn interaction_net_bind_calls_an_embedded_source_function() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "increment value = value + 1\n",
            "called = interaction_net <| do\n",
            "  .bind -> application\n",
            "  .data increment -> function\n",
            "  .data 41 -> argument\n",
            "  .wire (list.head application) (list.head function)\n",
            "  .wire (list.head (list.tail application)) (list.head argument)\n",
            "  .r (list.head (list.tail (list.tail application)))\n",
            "result = net_arity 0 called\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&definitions, &["result"]);
    assert_same_value_representation(&fully_evaluated_value(result), &Value::Number(n(42)));
}

#[test]
fn interaction_net_function_call_returns_an_ordinary_partial_function() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "sum left right = left + right\n",
            "partial_net = interaction_net <| do\n",
            "  .bind -> application\n",
            "  .data sum -> function\n",
            "  .data 19 -> argument\n",
            "  .wire (list.head application) (list.head function)\n",
            "  .wire (list.head (list.tail application)) (list.head argument)\n",
            "  .r (list.head (list.tail (list.tail application)))\n",
            "partial = net_arity 0 partial_net\n",
            "result = partial 23\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let partial = resolved_value_at_path(&definitions, &["partial"]);
    assert!(matches!(partial, Value::Function(_)));
    let result = resolved_value_at_path(&definitions, &["result"]);
    assert_same_value_representation(&fully_evaluated_value(result), &Value::Number(n(42)));
}

#[test]
fn interaction_net_function_calls_chain_through_explicit_binds() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "sum left right = left + right\n",
            "called = interaction_net <| do\n",
            "  .bind -> first\n",
            "  .bind -> second\n",
            "  .data sum -> function\n",
            "  .data 19 -> left\n",
            "  .data 23 -> right\n",
            "  .wire (list.head first) (list.head function)\n",
            "  .wire (list.head (list.tail first)) (list.head left)\n",
            "  .wire (list.head (list.tail (list.tail first))) (list.head second)\n",
            "  .wire (list.head (list.tail second)) (list.head right)\n",
            "  .r (list.head (list.tail (list.tail second)))\n",
            "result = net_arity 0 called\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&definitions, &["result"]);
    assert_same_value_representation(&fully_evaluated_value(result), &Value::Number(n(42)));
}

#[test]
fn interaction_net_function_call_preserves_captures() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "make_adder captured = \\value -> captured + value\n",
            "add_twenty = make_adder 20\n",
            "called = interaction_net <| do\n",
            "  .bind -> application\n",
            "  .data add_twenty -> function\n",
            "  .data 22 -> argument\n",
            "  .wire (list.head application) (list.head function)\n",
            "  .wire (list.head (list.tail application)) (list.head argument)\n",
            "  .r (list.head (list.tail (list.tail application)))\n",
            "result = net_arity 0 called\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let result = resolved_value_at_path(&definitions, &["result"]);
    assert_same_value_representation(&fully_evaluated_value(result), &Value::Number(n(42)));
}

#[test]
fn interaction_net_construction_is_memoized_and_preserves_initial_active_pairs() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "raw = interaction_net (.bind >>= (\\bind -> .data 0 >>= (\\data -> .copy 0 >>= (\\erase -> .wire (list.head bind) (list.head data) =>> .wire (list.head (list.tail bind)) (list.head erase) =>> .r (list.head (list.tail (list.tail bind)))))))\n",
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
    assert!(
        context.values().with_boundary_access(|access| {
            first.runtime().same_net_in(second.runtime(), &access)
        })
    );
    assert_eq!(
        first
            .runtime()
            .test_with(context.values(), |runtime| runtime.active_pairs().len()),
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
fn interaction_net_construction_preserves_standard_state_and_control_semantics() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "rollback = interaction_net (.cut (.alt ((.set ['choice] \"discarded\") =>> .data \"discarded\" >>= (\\_ports -> .fail)) (.get [] >>= (\\state -> not (dict.defines ['choice] state)) =>> .data \"clean\" >>= (\\ports -> .r (list.head ports)))))\n",
            "preserved = interaction_net (.data \"preserved\" >>= (\\ports -> .reset \"prompt\" ((.set ['visible] \"state\") =>> .shift \"prompt\" (\\continuation -> continuation (list.head ports)))))\n",
            "restored = interaction_net (.data \"restored\" >>= (\\ports -> .reset \"prompt\" (.get [] >>= (\\checkpoint -> (.set [] {}) =>> (.set [] checkpoint) =>> .shift \"prompt\" (\\continuation -> continuation (list.head ports))))))\n",
            "after_fix = interaction_net (.data \"after fix\" >>= (\\ports -> .reset \"prompt\" ((.cut (.fix (\\_loop -> .r ()))) =>> .shift \"prompt\" (\\continuation -> continuation (list.head ports)))))\n",
            "cleared = interaction_net (.data \"wrong\" >>= (\\ports -> .reset \"prompt\" ((.set [] {}) =>> .shift \"prompt\" (\\continuation -> continuation (list.head ports)))))\n",
            "hidden_by_fix = interaction_net (.data \"wrong\" >>= (\\ports -> .reset \"prompt\" (.fix (\\_loop -> .shift \"prompt\" (\\continuation -> continuation (list.head ports))))))\n",
            "rollback_result = net_arity 0 rollback\n",
            "preserved_result = net_arity 0 preserved\n",
            "restored_result = net_arity 0 restored\n",
            "after_fix_result = net_arity 0 after_fix\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    for (name, expected) in [
        ("rollback_result", b"clean".as_slice()),
        ("preserved_result", b"preserved".as_slice()),
        ("restored_result", b"restored".as_slice()),
        ("after_fix_result", b"after fix".as_slice()),
    ] {
        let result = resolved_value_at_path(&definitions, &[name]);
        assert_eq!(output_bytes(&result), expected, "unexpected `{name}`");
    }

    for name in ["cleared", "hidden_by_fix"] {
        let value = value_at_atom_path(&definitions, &[name]).unwrap();
        let error = fully_evaluated_error(value).to_string();
        assert!(
            error.contains("not in reset scope"),
            "`{name}` should report the hidden reset scope, got: {error}"
        );
    }
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
            "missing = interaction_net (.fail)\n",
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
fn interaction_net_construction_preserves_structured_effect_failures() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "import 'std\n",
            "failed = interaction_net {eff: anno context:\"net effect\" (anno 'error {msg:{text:\"construction failed\"}, detail:7})}\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);

    let definitions = evaluated_module_value(&context, &lowered);
    let halt = fully_evaluated_error(value_at_atom_path(&definitions, &["failed"]).unwrap());
    let failure = halt.into_permanent_failure();
    let Value::Dict(emission) = failure
        .emission_value()
        .expect("construction failure should retain its emission")
    else {
        panic!("construction failure emission should be a dictionary")
    };
    crate::compiler::test_value_factory().assert_same_representation_for_test(
        &emission.get(&Key::atom_from_text("detail")),
        &Some(&Value::Number(n(7))),
    );

    let net = crate::diagnostic::evaluation_context_frame("net_construction");
    let net_indices = failure
        .contexts()
        .iter()
        .enumerate()
        .filter_map(|(index, context)| same_value_representation(context, &net).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(
        net_indices.len(),
        1,
        "net construction adds one outer frame"
    );
    let source_index = failure
        .contexts()
        .iter()
        .position(|context| {
            same_value_representation(context, &Value::binary_from_text("net effect"))
        })
        .expect("the original effect context should survive");
    assert!(net_indices[0] < source_index);
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
        let failure = fully_evaluated_error(value).into_permanent_failure();
        let frame = crate::diagnostic::evaluation_context_frame("net_construction");
        assert_eq!(
            failure
                .contexts()
                .iter()
                .filter(|context| same_value_representation(context, &frame))
                .count(),
            1,
            "{name} should retain one public construction frame"
        );
        assert!(
            failure.to_string().contains(expected),
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
            error.contains("copy count must be a nonnegative integer"),
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
    crate::compiler::test_value_factory().assert_same_representation_for_test(
        &value.get_atom_path(&[Atom::from_key(&Key::binary_from_text("Foo"))]),
        &Some(&abstract_path_atom(&["pkg", "module", "Foo"])),
    );
    crate::compiler::test_value_factory().assert_same_representation_for_test(
        &value_at_atom_path(&value, &["palette", "Blue"]).as_ref(),
        &Some(&abstract_path_atom(&["pkg", "module", "palette", "Blue"])),
    );
}

#[test]
fn abstract_global_path_keyword_qualifies_static_module_names() {
    let context = CompileContext::default();
    let lowered = lower_with_module_path(
        concat!(
            "language g0\n",
            "direct = abstract_global_path foo\n",
            "nested = abstract_global_path foo.bar\n",
            "explicit = abstract_global_path module.foo.bar\n",
        ),
        &["pkg", "module"],
    );
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_same_value_representation(
        &resolved_value_at_path(&value, &["direct"]),
        &abstract_path_atom(&["pkg", "module", "foo"]),
    );
    let nested = abstract_path_atom(&["pkg", "module", "foo", "bar"]);
    assert_same_value_representation(&resolved_value_at_path(&value, &["nested"]), &nested);
    assert_same_value_representation(
        &resolved_value_at_path(&value, &["explicit"]),
        &abstract_path_atom(&["pkg", "module", "foo", "bar"]),
    );
}

#[test]
fn abstract_global_path_keyword_rejects_local_and_object_roots() {
    for (source, expected) in [
        (
            "language g0\nbad = \\foo -> abstract_global_path foo\n",
            "resolves `foo` as a local",
        ),
        (
            "language g0\nobject holder with\n  bad = abstract_global_path child\n",
            "resolves through an object or `using` namespace",
        ),
        (
            "language g0\nobject holder with\n  alias h\n  bad = abstract_global_path h\n",
            "resolves `h` as an object alias",
        ),
        (
            "language g0\nnamespace = {}\nbad = using namespace in abstract_global_path child\n",
            "resolves through an object or `using` namespace",
        ),
    ] {
        let lowered = lower_with_module_path(source, &["pkg", "module"]);
        assert!(
            lowered
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "missing `{expected}` diagnostic for `{source}`: {:?}",
            lowered.diagnostics
        );
    }
}

#[test]
fn explicit_module_abstract_global_paths_escape_object_scope() {
    let context = CompileContext::default();
    let lowered = lower_with_module_path(
        concat!(
            "language g0\n",
            "object holder with\n",
            "  path = abstract_global_path module.target\n",
            "object aliased with\n  alias _a\n",
            "  path = abstract_global_path target\n",
        ),
        &["pkg", "module"],
    );
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_same_value_representation(
        &resolved_value_at_path(&value, &["holder", "path"]),
        &abstract_path_atom(&["pkg", "module", "target"]),
    );
    assert_same_value_representation(
        &resolved_value_at_path(&value, &["aliased", "path"]),
        &abstract_path_atom(&["pkg", "module", "target"]),
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
    let message = context
        .values()
        .with_boundary_access(|access| emitted[0].1.emission.clone_core_with(&access));
    let Value::Dict(message) = &message else {
        panic!("diagnostic message must be a dictionary");
    };
    let Some(Value::Dict(interface)) = message.get(&*crate::core::keys::MSG) else {
        panic!("diagnostic message must provide msg");
    };
    let Some(Value::Dict(location)) = interface.get(&*crate::core::keys::LOCATION) else {
        panic!("diagnostic message must provide msg.location");
    };
    crate::compiler::test_value_factory().assert_same_representation_for_test(
        &location.get(&*crate::core::keys::LINE),
        &Some(&Value::Number(crate::number::Number::from_usize(2))),
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
    let math =
        crate::evaluation::EvalContext::evaluate_compatibility_whnf(&test_eval_context(), math)
            .expect("merged math binding should evaluate");

    let Value::Dict(math) = math else {
        panic!("math should evaluate to a dictionary");
    };

    crate::compiler::test_value_factory().assert_same_representation_for_test(
        &math.get(&Key::atom_from_text("answer")).map(|value| {
            fully_evaluated_value(value.duplicate_for_test(&crate::compiler::test_value_factory()))
        }),
        &Some(Value::Number(42.into())),
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

/// A module's reflection boundary reports overriding a name its journal
/// lacks and a declared name never defined; an assumed name may be
/// overridden. The definitions themselves still evaluate.
#[test]
fn module_boundaries_report_undefined_overrides_and_declarations() {
    let source = r#"language g0
import 'std
foo := 1
declare later
assume outside
outside := 2
ok = "ok"
meta.probe = anno { refl:(.heap.get [guard,'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .r ()))) } "probe"
"#;
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["journal_report_test"], &[("guard", "")]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["ok"]),
        &Value::binary_from_text("ok"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["foo"]),
        &Value::Number(n(1)),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        &Value::binary_from_text("probe"),
    );
    let messages = rendered_texts(&context, &take_reflection_diagnostics(&diagnostics));
    assert_eq!(
        messages,
        [
            "cannot override `foo` because it is not defined",
            "`later` is declared but never defined",
        ]
    );
}

/// Introducing a name the file already defined fails the module at compile
/// time, whether or not anything observes it.
#[test]
fn duplicate_introductions_fail_the_module() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse("language g0\nfoo = 1\nfoo = 2\nbar := 3\nbar = 4\nok = \"ok\"\n"),
        &context,
    );
    assert_eq!(
        lowered
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.line, diagnostic.message.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                3,
                "cannot introduce `foo` because line 2 already defines it"
            ),
            (
                5,
                "cannot introduce `bar` because line 4 already defines it"
            ),
        ]
    );

    // An object, an import's name, a nested target and an update define
    // names too, and a nested target its namespace.
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "object Point with\n    x = 1\n",
            "Point.x = 2\n",
            "import 'std as s\n",
            "s = 3\n",
            "u ::= \\_ -> 1\n",
            "u = 4\n",
            "v.w = 5\n",
            "v = 6\n",
            "assume x\n",
            "x = 7\n",
        )),
        &context,
    );
    assert_eq!(
        lowered
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.line, diagnostic.message.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                4,
                "cannot introduce `Point.x` because line 3 already defines it"
            ),
            (6, "cannot introduce `s` because line 5 already defines it"),
            (8, "cannot introduce `u` because line 7 already defines it"),
            (10, "cannot introduce `v` because line 9 already defines it"),
            (
                12,
                "cannot introduce `x` because line 11 already defines it"
            ),
        ]
    );
}

/// A fully evaluated list's items.
fn list_items(list: &crate::core::List) -> Vec<Value> {
    let items = std::cell::RefCell::new(Vec::new());
    list.try_for_each_segment(
        &mut |_| Err("journal lists hold no bytes".to_owned()),
        &mut |values| {
            items.borrow_mut().extend(
                values
                    .iter()
                    .map(|value| value.duplicate_for_test(test_eval_context().values())),
            );
            Ok(())
        },
        &mut |thunk| match crate::evaluation::EvalContext::evaluate_compatibility_whnf(
            &test_eval_context(),
            &match thunk {
                crate::core::ListThunk::Lazy(lazy) => {
                    Value::Lazy(lazy.duplicate_for_test(&crate::compiler::test_value_factory()))
                }
                crate::core::ListThunk::Promised(promise) => Value::Promised(
                    promise.duplicate_for_test(&crate::compiler::test_value_factory()),
                ),
            },
        )
        .map_err(|err| err.to_string())?
        {
            Value::List(list) => Ok(list),
            other => Err(format!(
                "unexpected list chunk {}",
                diagnostic_kind_name(&other)
            )),
        },
    )
    .expect("list should evaluate");
    items.into_inner()
}

/// Renders a journal with each entry as `op path@line`, fully evaluated.
fn journal_text(value: Value) -> String {
    let key_text = |key: &Key| match key {
        Key::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Key::Atom(atom) => match atom.key() {
            Key::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            other => format!("{other:?}"),
        },
        other => format!("{other:?}"),
    };
    let field = |dict: &Dict, name: &str| {
        fully_evaluated_value(
            dict.get(&Key::atom_from_text(name))
                .unwrap_or_else(|| panic!("journal entry should have `{name}`"))
                .duplicate_for_test(test_eval_context().values()),
        )
    };
    match fully_evaluated_value(value) {
        Value::Dict(dict) if dict.is_empty() => "{}".to_owned(),
        Value::List(list) => format!(
            "[{}]",
            list_items(&list)
                .into_iter()
                .map(journal_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Dict(entry) => {
            let Value::Dict(op) = field(&entry, "op") else {
                panic!("journal op should be a dict");
            };
            let (tag, path) = op.iter().next().expect("journal op should have a tag");
            let Value::List(path) =
                fully_evaluated_value(path.duplicate_for_test(test_eval_context().values()))
            else {
                panic!("journal path should be a list");
            };
            let path = list_items(&path)
                .into_iter()
                .map(|part| match fully_evaluated_value(part) {
                    Value::Atom(atom) => key_text(&Key::Atom(atom)),
                    other => format!("{:?}", diagnostic_kind_name(&other)),
                })
                .collect::<Vec<_>>()
                .join(".");
            let Value::Dict(at) = field(&entry, "at") else {
                panic!("journal `at` should be a dict");
            };
            let Value::Dict(g) = field(&at, "g") else {
                panic!("journal `at` should be a `g` frame");
            };
            let Value::Number(line) = field(&g, "line") else {
                panic!("journal `at` should have a line");
            };
            format!("{} {path}@{line}", key_text(tag))
        }
        other => panic!("unexpected journal value {}", diagnostic_kind_name(&other)),
    }
}

/// Each namespace journals what is introduced, overridden or updated in it,
/// appending once per stretch between imports.
#[test]
fn modules_journal_their_definitions() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "foo = 1\n",
            "import 'std as s\n",
            "foo ::= \\x -> x\n",
            "bar := 2\n",
            "f.g.h = 3\n",
            "refl.x = .r ()\n",
            "meta.a.b = 1\n",
            "object Point with\n    x = 1\n",
            "extend Point with\n    y = 2\n",
            "unique u\n",
            "declare d.e\n",
            "assume a\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);
    let module = evaluated_module_value(&context, &lowered);
    let journal = |path: &[&str]| {
        journal_text(value_at_atom_path(&module, path).expect("journal should exist"))
    };
    assert_eq!(
        journal(&["meta", "journal"]),
        concat!(
            "[update meta.refl@1, intro foo@2, intro s@3, update foo@4, over bar@5, update f@6, ",
            "intro refl.x@7, intro meta.a.b@8, intro Point@9, over Point@11, intro u@13, ",
            "declare d@14, assume a@15]"
        )
    );
    assert_eq!(journal(&["d", "meta", "journal"]), "[declare e@14]");
    assert_eq!(journal(&["f", "meta", "journal"]), "[update g@6]");
    assert_eq!(journal(&["f", "g", "meta", "journal"]), "[intro h@6]");

    // An inline built-in import updates each name it merges.
    let context = CompileContext::default();
    let lowered = lower_parsed_source(parse("language g0\nimport 'std\nfoo = 1\n"), &context);
    assert_eq!(lowered.diagnostics, []);
    let module = evaluated_module_value(&context, &lowered);
    let journal = journal_text(value_at_atom_path(&module, &["meta", "journal"]).unwrap());
    assert!(
        journal.starts_with("[update meta.refl@1, update "),
        "{journal}"
    );
    assert!(journal.contains(", update list@2, "), "{journal}");
    assert!(journal.ends_with(", intro foo@3]"), "{journal}");
}

/// An object body appends its members, declarations and assumptions to the
/// object's own journal, with whole paths; a dict `with` body keeps none and
/// cannot declare or assume.
#[test]
fn object_bodies_journal_their_members() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "object foo with\n",
            "    a = 1\n",
            "    b.c := 2\n",
            "    declare d\n",
            "    assume e, f.g\n",
            "    object h\n",
        )),
        &context,
    );
    assert_eq!(lowered.diagnostics, []);
    let module = evaluated_module_value(&context, &lowered);
    assert_eq!(
        journal_text(value_at_atom_path(&module, &["foo", "meta", "journal"]).unwrap()),
        "[intro a@3, over b.c@4, declare d@5, assume e@6, assume f.g@6, intro h@7]"
    );

    let lowered = lower_parsed_source(
        parse("language g0\nd = {} with\n    declare x\n"),
        &CompileContext::default(),
    );
    assert_eq!(
        lowered
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.line, diagnostic.message.as_str()))
            .collect::<Vec<_>>(),
        [(
            3,
            "`declare` and `assume` belong to modules and object bodies, not dict `with` bodies"
        )]
    );
}

/// A declared object's boundary replays its journal, inherited members
/// included, in place of its members' run-time assertions.
#[test]
fn object_boundaries_report_member_conflicts() {
    let source = r#"language g0
import 'std
object base with
    x = 1
    declare y
object foo extends base with
    x = 2
    z := 3
    assume w
    w := 4
    y = 5
meta.probe = anno { refl:(.heap.get [[abstract_global_path foo, 'meta, 'refl]:(),'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .r ()))) } "probe"
"#;
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["object_journal_test"], &[("guard", "")]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["foo", "x"]),
        &Value::Number(n(2)),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        &Value::binary_from_text("probe"),
    );
    let messages = rendered_texts(&context, &take_reflection_diagnostics(&diagnostics));
    assert_eq!(
        messages,
        [
            "cannot introduce `x` because it is already defined",
            "cannot override `z` because it is not defined",
        ]
    );
}

/// Module scope cannot define `spec`, which belongs to objects, except
/// within `meta` or `refl`.
#[test]
fn module_scope_rejects_spec() {
    let context = CompileContext::default();
    let lowered = lower_parsed_source(
        parse(concat!(
            "language g0\n",
            "spec = 1\n",
            "f.spec.x = 2\n",
            "meta.spec = 3\n",
            "object spec\n",
            "d = {} with\n    spec = 4\n",
        )),
        &context,
    );
    assert_eq!(
        lowered
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.line, diagnostic.message.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                2,
                "cannot define `spec` at module scope: `spec` belongs to objects"
            ),
            (
                3,
                "cannot define `f.spec.x` at module scope: `spec` belongs to objects"
            ),
            (
                5,
                "cannot define `spec` at module scope: `spec` belongs to objects"
            ),
        ]
    );
}

#[test]
fn update_definitions_observe_prior_module_state() {
    let context = CompileContext::default();
    let parsed = parse("language g0\nfoo = 1\nfoo ::= \\prior -> prior + 1\n");
    let lowered = lower_parsed_source(parsed, &context);
    assert_eq!(lowered.diagnostics, []);

    let value = evaluated_module_value(&context, &lowered);
    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["foo"])),
        &Value::Number(2.into()),
    );
}

/// A module-level definition in a declared object carries only the object's
/// reflection boundary, not the module's, as objects are self-contained.
#[test]
fn object_member_definitions_skip_the_module_boundary() {
    let source = r#"language g0
import 'std
refl.notice = .log 'info { msg:{ text:"module reflection" } }
object foo with
    a = 1
foo.b = 2
meta.module_unclaimed = anno { refl:(not (.heap.try_get [guard,'claim])) } "module unclaimed"
meta.object_claimed = anno { refl:(.heap.get [[abstract_global_path foo, 'meta, 'refl]:(),'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .r ()))) } "object claimed"
"#;
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["object_member_test"], &[("guard", "")]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["foo", "b"]),
        &Value::Number(n(2)),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "module_unclaimed"]),
        &Value::binary_from_text("module unclaimed"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "object_claimed"]),
        &Value::binary_from_text("object claimed"),
    );
    assert!(take_reflection_diagnostics(&diagnostics).is_empty());
}

#[test]
fn ordinary_module_demand_launches_final_reflection_tasks_once() {
    let source = r#"language g0
import 'std
refl.notice = .log 'info { msg:{ text:"new reflection task" } }
refl.notice := .log 'info { msg:{ text:"final reflection task" } }
meta.hidden = "metadata"
ordinary = "ordinary"
ordinary_two = "ordinary two"
probe = anno { refl:(.heap.get [guard,'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [guard,'tasks] >>= (\tasks -> .task.join (list.head tasks).task >>= (\_ -> .r ()))))) } "probe"
"#;
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["module_refl_test"], &[("guard", "")]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "hidden"]),
        &Value::binary_from_text("metadata"),
    );
    assert!(take_reflection_diagnostics(&diagnostics).is_empty());

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["ordinary"]),
        &Value::binary_from_text("ordinary"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["probe"]),
        &Value::binary_from_text("probe"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["ordinary_two"]),
        &Value::binary_from_text("ordinary two"),
    );

    let diagnostics = take_reflection_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        rendered_texts(&context, &diagnostics)[0],
        "final reflection task"
    );
}

#[test]
fn named_top_level_object_uses_object_refl_without_triggering_module_refl() {
    let source = r#"language g0
import 'std
refl.module_notice = .log 'info { msg:{ text:"module reflection task" } }
meta.probe = anno { refl:(.heap.get [[foo.spec.name, 'meta, 'refl]:(),'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [[foo.spec.name, 'meta, 'refl]:(),'tasks] >>= (\tasks -> .task.join (list.head tasks).task >>= (\_ -> .r ()))))) } "probe"
object foo with
  refl.notice = .log 'info { msg:{ text:"object reflection task" } }
  meta.hidden = "metadata"
  value = "value"
"#;
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["object_refl_test"], &[]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["foo", "meta", "hidden"]),
        &Value::binary_from_text("metadata"),
    );
    assert!(take_reflection_diagnostics(&diagnostics).is_empty());

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["foo", "value"]),
        &Value::binary_from_text("value"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        &Value::binary_from_text("probe"),
    );

    let diagnostics = take_reflection_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        rendered_texts(&context, &diagnostics)[0],
        "object reflection task"
    );
}

#[test]
fn nested_declared_object_uses_final_extended_refl() {
    let source = r#"language g0
import 'std
meta.probe = anno { refl:(.heap.get [[parent.child.spec.name, 'meta, 'refl]:(),'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [[parent.child.spec.name, 'meta, 'refl]:(),'tasks] >>= (\tasks -> .task.join (list.head tasks).task >>= (\_ -> .r ()))))) } "probe"
object parent with
  refl.parent_notice = .log 'info { msg:{ text:"parent reflection task" } }
  object child with
    refl.notice = .log 'info { msg:{ text:"original child reflection task" } }
    value = "nested value"
extend parent.child with
  refl.notice := .log 'info { msg:{ text:"extended child reflection task" } }
"#;
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["nested_object_refl_test"], &[]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["parent", "child", "value"]),
        &Value::binary_from_text("nested value"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        &Value::binary_from_text("probe"),
    );

    let diagnostics = take_reflection_diagnostics(&diagnostics);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        rendered_texts(&context, &diagnostics)[0],
        "extended child reflection task"
    );
}

#[test]
fn inherited_member_uses_derived_object_refl_and_guard() {
    let source = r#"language g0
import 'std
meta.derived_probe = anno { refl:(.heap.get [[derived.spec.name, 'meta, 'refl]:(),'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [[derived.spec.name, 'meta, 'refl]:(),'tasks] >>= (\tasks -> .task.join (list.head tasks).task >>= (\_ -> .r ()))))) } "derived probe"
meta.base_probe = anno { refl:(.heap.get [[base.spec.name, 'meta, 'refl]:(),'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [[base.spec.name, 'meta, 'refl]:(),'tasks] >>= (\tasks -> .task.join (list.head tasks).task >>= (\_ -> .r ()))))) } "base probe"
object base with
  refl.notice = .log 'info { msg:{ text:"base reflection task" } }
  inherited = "inherited value"
object derived extends base with
  refl.notice := .log 'info { msg:{ text:"derived reflection task" } }
"#;
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["inherited_object_refl_test"], &[]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["derived", "inherited"]),
        &Value::binary_from_text("inherited value"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "derived_probe"]),
        &Value::binary_from_text("derived probe"),
    );
    let derived_diagnostics = take_reflection_diagnostics(&diagnostics);
    assert_eq!(derived_diagnostics.len(), 1);
    assert_eq!(
        rendered_texts(&context, &derived_diagnostics)[0],
        "derived reflection task"
    );

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["base", "inherited"]),
        &Value::binary_from_text("inherited value"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "base_probe"]),
        &Value::binary_from_text("base probe"),
    );
    let base_diagnostics = take_reflection_diagnostics(&diagnostics);
    assert_eq!(base_diagnostics.len(), 1);
    assert_eq!(
        rendered_texts(&context, &base_diagnostics)[0],
        "base reflection task"
    );
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
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["object_expression_refl_test"], &[]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["value", "ordinary"]),
        &Value::binary_from_text("ordinary"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["declared", "meta", "ordinary"]),
        &Value::binary_from_text("excluded ordinary"),
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
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["disabled_refl_test"], &[("guard", "")]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["ordinary"]),
        &Value::binary_from_text("ordinary"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        &Value::binary_from_text("probe"),
    );
    assert!(take_reflection_diagnostics(&diagnostics).is_empty());
}

/// The language declaration installs the module's reflection operation as
/// `meta.refl`, and the boundary's task runs whatever the final `meta.refl`
/// is: overriding it replaces the `refl.*` scanner, here with nothing.
#[test]
fn overriding_meta_refl_replaces_the_reflection_operation() {
    let source = r#"language g0
import 'std
refl.notice = .log 'info { msg:{ text:"scanned reflection task" } }
meta.refl := .r ()
meta.probe = anno { refl:(.heap.get [guard,'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> not (.heap.try_get [guard,'tasks])))) } "probe"
ordinary = "ordinary"
"#;
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["meta_refl_test"], &[("guard", "")]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["ordinary"]),
        &Value::binary_from_text("ordinary"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        &Value::binary_from_text("probe"),
    );
    assert!(take_reflection_diagnostics(&diagnostics).is_empty());
}

/// The default reflection operation takes ownership of its namespace's
/// state, so it runs once: run again, as user-defined syntax might, it logs
/// an error and halts.
#[test]
fn the_reflection_scanner_runs_once_per_namespace() {
    let source = r#"language g0
import 'std
refl.again = meta.refl
meta.probe = anno { refl:(.heap.get [guard,'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [guard,'tasks] >>= (\tasks -> .task.join (list.head tasks).task >>= (\_ -> .r ()))))) } "probe"
ordinary = "ordinary"
"#;
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["scanner_once_test"], &[("guard", "")]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["ordinary"]),
        &Value::binary_from_text("ordinary"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        &Value::binary_from_text("probe"),
    );
    let messages = rendered_texts(&context, &take_reflection_diagnostics(&diagnostics));
    assert_eq!(
        messages,
        ["the reflection scanner ran again for its namespace"]
    );
}

#[test]
fn automatic_reflection_tasks_require_unit_results() {
    let source = r#"language g0
import 'std
refl.bad = .r "not unit"
meta.probe = anno { refl:(.heap.get [guard,'claim] >>= (\scanner -> .task.join scanner >>= (\_ -> .heap.get [guard,'tasks] >>= (\tasks -> .task.error (list.head tasks).task >>= (\_error -> .r ()))))) } "probe"
ordinary = "ordinary"
"#;
    let (_assembler, context, module, _module_root, diagnostics) =
        reflection_test_module(source, &["unit_refl_test"], &[("guard", "")]);

    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["ordinary"]),
        &Value::binary_from_text("ordinary"),
    );
    assert_same_value_representation(
        &resolved_value_at_path_with_context(&context, &module, &["meta", "probe"]),
        &Value::binary_from_text("probe"),
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
    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["foo"])),
        &Value::Number(2.into()),
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

    assert_same_value_representation(
        &fully_evaluated_value(resolved_value_at_path(&value, &["foo"])),
        &Value::Number(2.into()),
    );
}

fn assembly_result_text(source: &str) -> Vec<u8> {
    let context = CompileContext::from_module_path(["assembly"]);
    let lowered = lower_parsed_source(parse(source), &context);
    assert!(
        lowered
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error),
        "{source}: {:?}",
        lowered.diagnostics
    );
    let value = evaluated_module_value(&context, &lowered);
    output_bytes(&fully_evaluated_value(resolved_value_at_path(
        &value,
        &["asm", "result"],
    )))
}

/// A body's `let` group binds where it stands, for the members after it:
/// recursive within itself, reading the definitions before it, and no
/// entry of the body's result.
#[test]
fn body_lets_bind_for_the_members_after_them() {
    for (source, expected) in [
        (
            "language g0\nobject hello with\n  target = \"World\"\n  let greeting = \"Hello, \"\n      mark = \"!\"\n  text = greeting ++ target ++ mark\nasm.result = hello.text\n",
            "Hello, World!",
        ),
        (
            "language g0\nbase = { x:\"Hello\" }\nd = base with\n  alias prior\n  let tmp = _prior.x ++ \", \"\n  x := tmp ++ \"World!\"\nasm.result = d.x\n",
            "Hello, World!",
        ),
        (
            "language g0\nobject o with\n  text = \"Hello\"\nextend o with\n  let tmp = _text ++ \"!\"\n  text := tmp\nasm.result = o.text\n",
            "Hello!",
        ),
        (
            "language g0\nd = {} with { let { a = b ++ \"!\"; b = \"Hi\" }; x = a }\nasm.result = d.x\n",
            "Hi!",
        ),
        (
            "language g0\nd = {} with\n  let count n = match n as\n        0 => \"done\"\n        _ => count (n - 1)\n  x = count 3\nasm.result = d.x\n",
            "done",
        ),
        (
            "language g0\nd = {} with { let tmp = 1; x = tmp }\nasm.result = match d as { {x:_} => \"private\"; _ => \"public\" }\n",
            "private",
        ),
    ] {
        assert_eq!(
            assembly_result_text(source),
            expected.as_bytes(),
            "{source}"
        );
    }
}

/// What precedes a `let` group does not see it, and its names are locals:
/// no member, use or other local may share them.
#[test]
fn body_lets_are_sequential_private_locals() {
    for (source, expected) in [
        (
            "language g0\nd = {} with\n  x = tmp\n  let tmp = 1\n",
            "local `tmp` shadows external `tmp` used by this file on line 3",
        ),
        (
            "language g0\nobject o with\n  tmp = 2\n  let tmp = 1\n  x = tmp\n",
            "local `tmp` shadows external `tmp` defined on line 3",
        ),
        (
            "language g0\nobject o with\n  let tmp = 1\n  let tmp = 2\n  x = tmp\n",
            "local `tmp` shadows existing local `tmp`; local name shadowing is not allowed",
        ),
        (
            "language g0\nobject o with\n  let tmp = 1\n  x = _tmp\n",
            "`_tmp` names the local `tmp`, which has no prior definition",
        ),
        (
            "language g0\nobject o with\n  let\n  x = 1\n",
            "a `let` member requires at least one binding",
        ),
        (
            "language g0\nobject o with\n  let x := 1\n  y = x\n",
            "local binding `x := 1` must use `=`",
        ),
        (
            "language g0\nobject o with\n  let x = 1\n  alias o\n  y = x\n",
            "`alias` may only be the first member of its body",
        ),
    ] {
        let lowered = lower_parsed_source(parse(source), &CompileContext::default());
        assert!(
            lowered.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error && diagnostic.message == expected
            }),
            "{source}: {:?}",
            lowered.diagnostics
        );
    }
}

#[test]
fn body_lets_follow_local_unused_warning_rules() {
    for source in [
        "language g0\nobject o with\n  let unused = 1\n  let _quiet = 2\n  let { a = b; b = 1 }\n  x = a\n",
        "language g0\nd = {} with\n  let unused = 1\n  let _quiet = 2\n  let { a = b; b = 1 }\n  x = a\n",
    ] {
        let parsed = parse(source);
        let warnings = parsed
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Warning)
            .map(|diagnostic| (diagnostic.line, diagnostic.message.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(warnings, [(3, "unused local `unused`")], "{source}");
    }
}

/// `alias self`, written or an object's default, localizes a body's names,
/// as `using` does: the locals outside it take `^`, as its host's names do.
/// Other aliases leave the locals in reach.
#[test]
fn localized_bodies_reach_outer_locals_through_escapes() {
    for source in [
        "language g0\nf = \\v -> object _ with\n  text = ^v\nasm.result = (f \"outer\").text\n",
        "language g0\nf = \\v -> object _ with\n  alias self\n  text = ^v\nasm.result = (f \"outer\").text\n",
        "language g0\nf = \\v -> {} with\n  alias self\n  text = ^v\nasm.result = (f \"outer\").text\n",
        "language g0\nf = \\v -> object _ with\n  alias _\n  text = v\nasm.result = (f \"outer\").text\n",
        "language g0\nf = \\v -> object _ with\n  alias _o\n  text = v\nasm.result = (f \"outer\").text\n",
        "language g0\nf = \\v -> {} with\n  text = v\nasm.result = (f \"outer\").text\n",
        "language g0\nf = \\v -> using {} in ^v\nasm.result = f \"outer\"\n",
        "language g0\nobject o with\n  let tmp = \"outer\"\n  object inner with\n    text = ^tmp\nasm.result = o.inner.text\n",
        "language g0\nf = \\v -> object _ with\n  let own = ^v\n  g w = w ++ own\n  text = g \"\"\nasm.result = (f \"outer\").text\n",
        "language g0\nf = \\_v -> object _ with\n  v = \"outer\"\n  text = self.v\nasm.result = (f \"x\").text\n",
    ] {
        assert_eq!(assembly_result_text(source), b"outer", "{source}");
        assert_eq!(parse(source).diagnostics, [], "{source}");
    }
}

/// A localized scope is a controlled shadowing: a bare name reads the
/// scope's own, even beside a local of that name outside it.
#[test]
fn localized_scopes_shadow_outer_locals() {
    for source in [
        "language g0\nf = \\v -> object _ with\n  v = \"own\"\n  text = v ++ ^v\nasm.result = (f \"outer\").text\n",
        "language g0\nf = \\v -> {} with\n  alias self\n  v = \"own\"\n  text = v ++ ^v\nasm.result = (f \"outer\").text\n",
        "language g0\nobject o with\n  let v = \"outer\"\n  object inner with\n    v = \"own\"\n    text = v ++ ^v\nasm.result = o.inner.text\n",
        "language g0\nf = let v = \"outer\" in using {v:\"own\"} in v ++ ^v\nasm.result = f\n",
        "language g0\nlet v = \"outer\"\nobject o with\n  v = \"own\"\n  text = v ++ ^v\nasm.result = o.text\n",
        "language g0\nlet v = \"outer\"\nf = using {v:\"own\"} in v ++ ^v\nasm.result = f\n",
    ] {
        assert_eq!(assembly_result_text(source), b"ownouter", "{source}");
        assert_eq!(parse(source).diagnostics, [], "{source}");
    }
}

/// A file's `let` group is lowered once where it stands, against the
/// module's final self and the definitions visible there, and the
/// declarations after it share it.
#[test]
fn file_lets_bind_for_the_declarations_after_them() {
    for (source, expected) in [
        (
            "language g0\nlet greeting = \"Hello\"\nasm.result = greeting ++ \", World!\"\n",
            "Hello, World!",
        ),
        (
            "language g0\ntarget = \"World\"\nlet text = \"Hello, \" ++ target ++ mark\n    mark = \"!\"\nasm.result = text\n",
            "Hello, World!",
        ),
        (
            "language g0\nlet count n = if n == 0 then \"done\" else count (n - 1)\nasm.result = count 3\n",
            "done",
        ),
        (
            "language g0\nx = \"Hello, \"\nlet prior = _x\nx := \"World!\"\nasm.result = prior ++ x\n",
            "Hello, World!",
        ),
        (
            "language g0\nlet greeting = \"Hello\"\nlet text = greeting ++ \", World!\"\nasm.result = text\n",
            "Hello, World!",
        ),
        (
            "language g0\nlet greeting = \"Hello, World!\"\nobject o with\n  text = ^greeting\nasm.result = o.text\n",
            "Hello, World!",
        ),
        (
            "language g0\nlet\n  greeting = \"Hello, \"\n  target = \"World!\"\nasm.result = greeting ++ target\n",
            "Hello, World!",
        ),
    ] {
        assert_eq!(
            assembly_result_text(source),
            expected.as_bytes(),
            "{source}"
        );
    }
}

/// One file, one name, one meaning: a file's `let` names are locals that
/// no global use or definition, nor other local, may share.
#[test]
fn file_lets_are_sequential_private_locals() {
    for (source, expected) in [
        (
            "language g0\nasm.result = tmp\nlet tmp = 1\n",
            "local `tmp` shadows external `tmp` used by this file on line 2",
        ),
        (
            "language g0\nlet tmp = 1\ntmp = 2\n",
            "local `tmp` shadows external `tmp` defined on line 3",
        ),
        (
            "language g0\nlet tmp = 1\nlet tmp = 2\nasm.result = tmp\n",
            "local `tmp` shadows existing local `tmp`; local name shadowing is not allowed",
        ),
        (
            "language g0\nlet tmp = 1\nf = \\tmp -> tmp\n",
            "local `tmp` shadows existing local `tmp`; local name shadowing is not allowed",
        ),
        (
            "language g0\nlet tmp = 1\nasm.result = _tmp\n",
            "`_tmp` names the local `tmp`, which has no prior definition",
        ),
        (
            "language g0\nlet tmp = 1\nasm.result = abstract_global_path tmp\n",
            "`abstract_global_path tmp` resolves `tmp` as a local; abstract global paths must bind to the module namespace",
        ),
    ] {
        let lowered = lower_parsed_source(parse(source), &CompileContext::default());
        assert!(
            lowered.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error && diagnostic.message == expected
            }),
            "{source}: {:?}",
            lowered.diagnostics
        );
    }
}

#[test]
fn file_lets_follow_local_unused_warning_rules() {
    let lowered = lower_parsed_source(
        parse("language g0\nlet unused = 1\nlet _quiet = 2\nlet { a = b; b = 1 }\nx = a\n"),
        &CompileContext::default(),
    );
    let warnings = lowered
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Warning)
        .map(|diagnostic| (diagnostic.line, diagnostic.message.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(warnings, [(2, "unused local `unused`")]);
}

/// Only `alias self`, written or by default, binds `self`: a module, an
/// object by default, and a `using` scope. Under `alias _` or a named
/// alias, `self` keeps its host's meaning, so `alias _` leaves no way to
/// reference the body.
#[test]
fn self_names_the_innermost_scope_aliased_self() {
    for (source, expected) in [
        (
            "language g0\nx = \"module\"\nasm.result = self.x\n",
            "module",
        ),
        (
            "language g0\nx = \"module\"\nd = {x:\"dict\"} with { y = self.x }\nasm.result = d.y\n",
            "module",
        ),
        (
            "language g0\nx = \"module\"\nd = {x:\"dict\"} with { alias self; y = self.x }\nasm.result = d.y\n",
            "dict",
        ),
        (
            "language g0\nx = \"module\"\nobject o with\n  alias named\n  x = \"object\"\n  y = self.x ++ named.x\nasm.result = o.y\n",
            "moduleobject",
        ),
        (
            "language g0\nx = \"module\"\nobject o with\n  x = \"object\"\n  object p with\n    alias _\n    y = self.x\nasm.result = o.p.y\n",
            "object",
        ),
        (
            "language g0\nx = \"module\"\nd = {x:\"dict\"} with { y = _self.x }\nasm.result = d.y\n",
            "module",
        ),
        (
            "language g0\nx = \"module\"\nasm.result = using {x:\"dict\"} in self.x\n",
            "dict",
        ),
    ] {
        assert_eq!(
            assembly_result_text(source),
            expected.as_bytes(),
            "{source}"
        );
    }
}

/// `foo` is a visible local or `self.foo` in every scope, and each `^` steps
/// out of the innermost `self`: a body that binds no `self` is transparent.
/// An escape sees the locals lexically visible where it lands, and its own.
#[test]
fn escapes_step_out_of_the_innermost_self() {
    for (source, expected) in [
        (
            "language g0\nx = \"module\"\nobject o with\n  x = \"o\"\n  d = {} with { y = ^x }\nasm.result = o.d.y\n",
            "module",
        ),
        (
            "language g0\nx = \"m\"\nobject a with\n  x = \"a\"\n  object b with\n    x = \"b\"\n    d = {} with { y = ^x ++ ^^x ++ x }\nasm.result = a.b.d.y\n",
            "amb",
        ),
        (
            "language g0\nx = \"module\"\nobject o with\n  alias named\n  y = x\nasm.result = o.y\n",
            "module",
        ),
        (
            "language g0\nobject o with\n  x = \"o\"\n  object p with\n    alias q\n    object r with\n      y = ^x\nasm.result = o.p.r.y\n",
            "o",
        ),
        (
            "language g0\nobject o with\n  let t = \"o \"\n  d = object _ with\n    alias _\n    let u = \"d\"\n    object r with\n      y = ^t ++ ^u\nasm.result = o.d.r.y\n",
            "o d",
        ),
        (
            "language g0\nf = \\v -> object _ with\n  g = ^(\\w -> w ++ v)\nasm.result = (f \"a\").g \"b\"\n",
            "ba",
        ),
    ] {
        assert_eq!(
            assembly_result_text(source),
            expected.as_bytes(),
            "{source}"
        );
    }
}

#[test]
fn escapes_reach_only_what_their_target_sees() {
    for (source, expected) in [
        (
            "language g0\nx = 1\nasm.result = ^x\n",
            "scope escape depth `1` exceeds available parent scopes at level `1`",
        ),
        (
            "language g0\nx = 1\nd = {} with { y = ^x }\n",
            "scope escape depth `1` exceeds available parent scopes at level `1`",
        ),
        (
            "language g0\nobject o with\n  f = \\v -> {} with { y = ^v }\n",
            "local `v` shadows external `v` used by this file on line 3",
        ),
    ] {
        let lowered = lower_parsed_source(parse(source), &CompileContext::default());
        assert!(
            lowered.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error && diagnostic.message == expected
            }),
            "{source}: {:?}",
            lowered.diagnostics
        );
    }
}
