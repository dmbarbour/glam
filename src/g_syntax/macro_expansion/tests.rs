use std::sync::{Arc, Mutex};

use crate::api::{
    Assembler, CompilationExecution, Diagnostic, DiagnosticEvent, DiagnosticSubscriber,
    Value as PublicValue,
};
use crate::core::{Dict, Key, List, Value, keys};
use crate::diagnostic::Severity;
use crate::eval;

use super::io::{
    MacroDelimiter, MacroInput, MacroInputElement, MacroInputKind, MacroInputLayout, MacroOutput,
};
use super::{MacroRun, run_macro_effect};

struct DiagnosticMessages(Arc<Mutex<Vec<String>>>);

impl DiagnosticSubscriber for DiagnosticMessages {
    fn receive(&self, event: DiagnosticEvent) {
        self.0
            .lock()
            .expect("diagnostic observation mutex should not be poisoned")
            .push(event.diagnostic().message().to_owned());
    }
}

fn compile_effects(source: &str) -> (Assembler, PublicValue) {
    let assembler = Assembler::default();
    let module = assembler
        .module(["macro_runner_test"])
        .script(
            "g",
            format!("language g0\nimport 'std\nmeta.effects = {source}\n"),
        )
        .build()
        .expect("macro effect fixture should compile");
    let effects = assembler
        .get(module.value(), "meta.effects")
        .expect("macro effect fixture should define `meta.effects`");
    (assembler, effects)
}

fn run(
    execution: &CompilationExecution,
    effect: &PublicValue,
    environment: Value,
) -> Result<MacroRun, Box<Diagnostic>> {
    run_macro_effect(
        execution,
        effect.as_core().clone(),
        environment,
        MacroInput::empty(),
    )
}

fn request_effect(path: &[&str], arguments: Vec<Value>) -> Value {
    eval::constant_effect(Value::Dict(Dict::new_sync().insert(
        Key::atom_from_key(&Key::abstract_global_path(path.iter().copied())),
        Value::List(List::from_values(arguments)),
    )))
}

fn return_effect(value: Value) -> Value {
    request_effect(&["reflection_runtime", "v0", "request", "r"], vec![value])
}

#[test]
fn macro_runner_exposes_environment_log_and_scoped_cases() {
    let (assembler, effect) = compile_effects(
        ".env '.flag >>= (\\_ -> .case \"flag case\" (.log 'info { msg:{ text:\"visible\" } } =>> .r ()))",
    );
    let execution = assembler.test_compilation_execution();
    let environment = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("flag"),
        Value::binary_from_text("visible"),
    ));

    let result = run(&execution, &effect, environment).expect("macro effect should succeed");
    assert_eq!(result.diagnostics().len(), 1);
    assert_eq!(result.diagnostics()[0].severity(), Severity::Info);
    assert_eq!(result.diagnostics()[0].message(), "visible");
    assert_eq!(result.visited_cases().len(), 1);
}

#[test]
fn macro_runner_selects_one_unit_branch_and_discards_other_journals() {
    let (assembler, effect) = compile_effects(
        ".alt (.log 'warn { msg:{ text:\"discarded\" } } =>> .fail) (.log 'info { msg:{ text:\"selected\" } } =>> .r ())",
    );
    let execution = assembler.test_compilation_execution();

    let result = run(&execution, &effect, Value::Dict(Dict::new_sync()))
        .expect("one successful macro branch should be accepted");
    assert_eq!(result.diagnostics().len(), 1);
    assert_eq!(result.diagnostics()[0].message(), "selected");
}

#[test]
fn macro_runner_rejects_zero_multiple_and_nonunit_results() {
    for (source, expected) in [
        (".fail", "no successful result"),
        (
            ".alt (.r ()) (.r ())",
            "multiple results; use `.cut` to select one",
        ),
        (".r 42", "terminated with Number, expected unit"),
    ] {
        let (assembler, effect) = compile_effects(source);
        let execution = assembler.test_compilation_execution();
        let error = run(&execution, &effect, Value::Dict(Dict::new_sync()))
            .expect_err("invalid macro result policy should fail");
        assert!(
            error.message().contains(expected),
            "unexpected diagnostic: {}",
            error.message()
        );
    }
}

#[test]
fn macro_api_omits_heap_tasks_and_full_reflection_requests() {
    for source in [".heap.get []", ".task.status {}", ".dict_items {}"] {
        let (assembler, effect) = compile_effects(source);
        let execution = assembler.test_compilation_execution();
        run(&execution, &effect, Value::Dict(Dict::new_sync()))
            .expect_err("restricted macro API request should be unavailable");
    }
}

#[test]
fn unstarted_reflection_gate_runs_inside_the_macro_session() {
    let assembler = Assembler::default();
    let execution = assembler.test_compilation_execution();
    let reflection = return_effect((*keys::UNIT_VALUE).clone());
    let gate = Value::reflection_gate(reflection, (*keys::UNIT_VALUE).clone());
    let macro_effect = PublicValue::from_core(return_effect(gate));

    run(&execution, &macro_effect, Value::Dict(Dict::new_sync()))
        .expect("macro session should launch and complete its reflection gate");
}

#[test]
fn assembler_owned_reflection_gate_is_foreign_to_macro_execution() {
    let (assembler, reflection) = compile_effects(".cut (.heap.get '.missing >>= (\\_ -> .fail))");
    let execution = assembler.test_compilation_execution();
    let gate = Value::reflection_gate(reflection.as_core().clone(), (*keys::UNIT_VALUE).clone());
    let error = eval::eval_value(&assembler.eval_context(), &gate)
        .expect_err("assembler observation should start and block the gate");
    assert!(error.blocked_on().is_some());
    let macro_effect = PublicValue::from_core(return_effect(gate));

    let error = run(&execution, &macro_effect, Value::Dict(Dict::new_sync()))
        .expect_err("macro execution must not migrate a foreign gate");
    assert!(error.message().contains("foreign or unavailable"));
}

#[test]
fn committed_reflection_log_survives_failed_macro_alternative() {
    let (assembler, effect) = compile_effects(
        ".alt ((anno refl:(.log 'warn { msg:{ text:\"reflection survived\" } }) (.r ())) =>> .fail) (.r ())",
    );
    let execution = assembler.test_compilation_execution();

    let result = run(&execution, &effect, Value::Dict(Dict::new_sync()))
        .expect("fallback macro branch should succeed");
    assert!(result.diagnostics().is_empty());
    assert_eq!(execution.macro_diagnostic_counts().warnings(), 1);
}

#[test]
fn committed_reflection_heap_and_children_outlive_macro_alternatives() {
    let (assembler, effects) = compile_effects(
        "{ write:(.alt ((anno refl:(.heap.set '.saved \"yes\") (.r ())) =>> .fail) (.r ())), read:(anno refl:(.heap.get '.saved >>= (\\saved -> .log 'info { msg:{ text:saved } })) (.r ())), child:(.alt ((anno refl:(.task.new (.log 'warn { msg:{ text:\"child survived\" } }) >>= (\\_ -> .r ())) (.r ())) =>> .fail) (.r ())) }",
    );
    assembler.record_diagnostic(Diagnostic::new(Severity::Info, "assembler-only diagnostic"));
    let execution = assembler.test_compilation_execution();
    let empty = Value::Dict(Dict::new_sync());

    for name in ["write", "read", "child"] {
        let effect = assembler
            .get(&effects, name)
            .expect("effect fixture member should exist");
        run(&execution, &effect, empty.clone()).expect("macro effect should select its fallback");
    }
    assert!(!execution.drain_for_test());
    let counts = execution.macro_diagnostic_counts();
    assert_eq!(counts.info(), 1);
    assert_eq!(counts.warnings(), 1);
    let assembler_counts = assembler.diagnostic_bus().counts();
    assert_eq!(assembler_counts.info(), 2);
    assert_eq!(assembler_counts.warnings(), 1);
}

#[test]
fn macro_reflection_heap_is_private_to_the_compilation_execution() {
    let (assembler, effect) = compile_effects("anno refl:(.heap.set '.macro_only \"yes\") (.r ())");
    let execution = assembler.test_compilation_execution();

    run(&execution, &effect, Value::Dict(Dict::new_sync()))
        .expect("macro reflection heap write should complete");
    assert!(!execution.drain_for_test());

    let macro_value = assembler
        .get(&execution.macro_heap(), "macro_only")
        .expect("macro heap read should succeed");
    let assembler_value = assembler.get(&assembler.test_reflection_heap(), "macro_only");
    assert_eq!(macro_value.as_core(), &Value::binary_from_text("yes"));
    assert!(assembler_value.is_err());
}

#[test]
fn compilation_execution_drain_reports_detached_failure_and_deadlock() {
    for (source, expected) in [
        (
            "anno refl:(.task.new (.fail) >>= (\\_ -> .r ())) (.r ())",
            "failed",
        ),
        (
            "anno refl:(.task.new (.heap.get '.never >>= (\\_ -> .fail)) >>= (\\_ -> .r ())) (.r ())",
            "deadlocked",
        ),
    ] {
        let (assembler, effect) = compile_effects(source);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let _subscription = assembler
            .diagnostic_bus()
            .subscribe(DiagnosticMessages(observed.clone()));
        let execution = assembler.test_compilation_execution();

        run(&execution, &effect, Value::Dict(Dict::new_sync()))
            .expect("detached child should not fail its completed parent");
        assert!(execution.drain_for_test());
        assert!(
            observed
                .lock()
                .expect("diagnostic observation mutex should not be poisoned")
                .iter()
                .any(|message| message.contains(expected)),
            "expected a macro reflection diagnostic containing `{expected}`"
        );
    }
}

#[test]
fn inline_macro_readers_and_writers_are_transactional() {
    let (assembler, effect) = compile_effects(
        ".alt (.read.text \"wrong\" =>> .fail) (.read.text \"pre\" =>> .read.regex \"[a-z]+\" >>= (\\found -> .read.sep =>> .read.data >>= (\\value -> .write.data found.span =>> .write.sep =>> .write.data value =>> .r ())))",
    );
    let execution = assembler.test_compilation_execution();
    let input = MacroInput::new(
        vec![
            MacroInputElement {
                kind: MacroInputKind::Text {
                    text: Arc::from("prefix"),
                    delimiter: None,
                },
                separated: false,
                start: 10,
                end: 16,
            },
            MacroInputElement {
                kind: MacroInputKind::Data(PublicValue::integer(42)),
                separated: true,
                start: 17,
                end: 19,
            },
        ],
        10,
        19,
    );
    let layout_run = run_macro_effect(
        &execution,
        effect.as_core().clone(),
        Value::Dict(Dict::new_sync()),
        input,
    )
    .expect("fallback reader branch should succeed");

    assert_eq!(layout_run.consumed_end(), 19);
    assert!(matches!(
        layout_run.output(),
        [
            MacroOutput::Data(_),
            MacroOutput::Separator,
            MacroOutput::Data(_)
        ]
    ));
}

#[test]
fn layout_readers_require_scoped_anchors_and_leave_root_anchor_as_failure() {
    let (assembler, effect) = compile_effects(
        ".read.layout (.read.anchor =>> .read.data >>= (\\first -> .read.anchor =>> .read.data >>= (\\second -> .read.end =>> .write.data first =>> .write.data second =>> .r ())))",
    );
    let input = MacroInput::new(
        vec![
            MacroInputElement {
                kind: MacroInputKind::Data(PublicValue::integer(1)),
                separated: true,
                start: 2,
                end: 3,
            },
            MacroInputElement {
                kind: MacroInputKind::Data(PublicValue::integer(2)),
                separated: true,
                start: 6,
                end: 7,
            },
        ],
        0,
        7,
    )
    .with_layouts(vec![MacroInputLayout {
        start: 0,
        end: 2,
        items: vec![0..1, 1..2].into(),
    }]);
    let layout_run = run_macro_effect(
        &assembler.test_compilation_execution(),
        effect.as_core().clone(),
        Value::Dict(Dict::new_sync()),
        input,
    )
    .expect("anchored child layout should be consumed completely");
    assert!(matches!(
        layout_run.output(),
        [MacroOutput::Data(_), MacroOutput::Data(_)]
    ));

    for source in [
        ".alt (.read.anchor =>> .fail) (.write.data 42)",
        ".alt (.read.layout (.r ())) (.write.data 42)",
    ] {
        let (assembler, effect) = compile_effects(source);
        let run = run(
            &assembler.test_compilation_execution(),
            &effect,
            Value::Dict(Dict::new_sync()),
        )
        .expect("an unavailable layout boundary should leave the fallback");
        assert!(matches!(run.output(), [MacroOutput::Data(_)]));
    }
}

#[test]
fn layout_writers_record_nested_nonempty_items() {
    let (assembler, effect) = compile_effects(
        ".write.text \"[\" =>> .write.layout (.write.anchor =>> .write.data 1 =>> .write.text \",\" =>> .write.anchor =>> .write.data 2) =>> .write.text \"]\"",
    );
    let run = run(
        &assembler.test_compilation_execution(),
        &effect,
        Value::Dict(Dict::new_sync()),
    )
    .expect("balanced layout output should succeed");
    assert!(matches!(
        run.output(),
        [
            MacroOutput::Text(_),
            MacroOutput::LayoutStart,
            MacroOutput::Anchor,
            MacroOutput::Data(_),
            MacroOutput::Text(_),
            MacroOutput::Anchor,
            MacroOutput::Data(_),
            MacroOutput::LayoutEnd,
            MacroOutput::Text(_),
        ]
    ));
}

#[test]
fn source_macro_embeds_data_through_the_ordinary_parser() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["macro_source_test"])
        .script(
            "g",
            "language g0\nimport 'std\nmeta.macro.env = {}\nmeta.literal = .read.end =>> .write.data 42 =>> .r ()\nanswer = @meta.literal\ngrouped = (@meta.literal)\n",
        )
        .build()
        .expect("inline source macro should compile");
    let answer = assembler
        .get(module.value(), "answer")
        .expect("macro output should define answer");
    assert_eq!(
        assembler.evaluate(&answer).expect("answer should evaluate"),
        PublicValue::integer(42)
    );
    let grouped = assembler
        .get(module.value(), "grouped")
        .expect("grouped macro output should exist");
    assert_eq!(
        assembler
            .evaluate(&grouped)
            .expect("grouped output should evaluate"),
        PublicValue::integer(42)
    );
}

#[test]
fn source_macros_consume_rollback_delete_and_leave_suffixes() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["macro_source_io_test"])
        .script(
            "g",
            r#"language g0
import 'std
meta.macro.env = {}
meta.consume = .read.sep =>> .read.data >>= (\value -> .write.data value =>> .r ())
meta.rollback = .alt (.write.data 0 =>> .read.text "missing") (.read.sep =>> .read.data >>= (\value -> .write.data value =>> .r ()))
meta.prefix = .write.text "40 +"
meta.language = .env '.language.base >>= (\base -> .write.data base =>> .r ())
meta.delete = .r ()
consumed = @meta.consume 41
rolled_back = @meta.rollback 42
suffix = @meta.prefix 2
language_base = @meta.language
@meta.delete
after_delete = 43
"#,
        )
        .build()
        .expect("inline source macro IO fixture should compile");

    for (name, expected) in [
        ("consumed", PublicValue::integer(41)),
        ("rolled_back", PublicValue::integer(42)),
        ("suffix", PublicValue::integer(42)),
        ("after_delete", PublicValue::integer(43)),
    ] {
        let value = assembler
            .get(module.value(), name)
            .unwrap_or_else(|error| panic!("`{name}` should exist: {error}"));
        assert_eq!(
            assembler
                .evaluate(&value)
                .unwrap_or_else(|error| panic!("`{name}` should evaluate: {error}")),
            expected
        );
    }
    let language = assembler
        .get(module.value(), "language_base")
        .expect("language macro should produce a value");
    assert_eq!(
        assembler
            .evaluate(&language)
            .expect("language should evaluate"),
        PublicValue::atom_from_text("g0")
    );
}

#[test]
fn text_span_and_end_cover_the_current_nonstructural_run() {
    let (assembler, effect) = compile_effects(
        ".read.text_span >>= (\\found -> .read.end =>> .write.data found.span =>> .r ())",
    );
    let execution = assembler.test_compilation_execution();
    let input = MacroInput::new(
        vec![MacroInputElement {
            kind: MacroInputKind::Text {
                text: Arc::from("remaining"),
                delimiter: None,
            },
            separated: false,
            start: 3,
            end: 12,
        }],
        3,
        12,
    );
    let run = run_macro_effect(
        &execution,
        effect.as_core().clone(),
        Value::Dict(Dict::new_sync()),
        input,
    )
    .expect("text-span reader should consume its complete run");
    let [MacroOutput::Data(span)] = run.output() else {
        panic!("text-span macro should emit one data value")
    };
    assert_eq!(
        assembler
            .evaluate(span)
            .expect("emitted span should evaluate"),
        PublicValue::text("remaining")
    );
}

#[test]
fn source_macros_read_next_line_hanging_and_nested_layouts() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["macro_layout_reader_test"])
        .script(
            "g",
            r#"language g0
import 'std
meta.macro.env = {}
meta.one = .read.layout (.read.anchor =>> .read.data >>= (\value -> .read.end =>> .write.data value =>> .r ()))
meta.sum = .read.layout (.read.anchor =>> .read.data >>= (\left -> .read.anchor =>> .read.data >>= (\right -> .read.end =>> .write.data (left + right) =>> .r ())))
meta.nested = .read.layout (.read.anchor =>> .read.data >>= (\outer -> .read.layout (.read.anchor =>> .read.data >>= (\left -> .read.anchor =>> .read.data >>= (\right -> .read.end =>> .write.data (outer + left + right) =>> .r ()))) =>> .read.end))
meta.boundary = .alt (.read.sep =>> .read.text "peer" =>> .write.text "own = 0") (.write.text "own = 42")
hanging = @meta.one 42
sum = @meta.sum
  20
  22
nested = @meta.nested
  1
    20
    21
object bounded with
  @meta.boundary
  peer = 99
"#,
        )
        .build()
        .expect("layout-aware source macros should compile");

    for (name, expected) in [("hanging", 42), ("sum", 42), ("nested", 42)] {
        let value = assembler
            .get(module.value(), name)
            .unwrap_or_else(|error| panic!("`{name}` should exist: {error}"));
        assert_eq!(
            assembler
                .evaluate(&value)
                .unwrap_or_else(|error| panic!("`{name}` should evaluate: {error}")),
            PublicValue::integer(expected),
            "unexpected value for `{name}`",
        );
    }
    let bounded = assembler
        .get(module.value(), "bounded")
        .expect("bounded object should exist");
    let own = assembler
        .get(&bounded, "own")
        .expect("macro-generated member should exist");
    assert_eq!(
        assembler.evaluate(&own).expect("own should evaluate"),
        PublicValue::integer(42),
        "the root macro cursor must not cross into its peer layout item",
    );
}

#[test]
fn source_macros_write_nested_and_same_anchor_layouts() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["macro_layout_writer_test"])
        .script(
            "g",
            r#"language g0
import 'std
meta.macro.env = {}
meta.list = .write.text "[" =>> .write.layout (.write.anchor =>> .write.data 1 =>> .write.text "," =>> .write.anchor =>> .write.data 2) =>> .write.text "]"
meta.declarations = .read.end =>> .write.anchor =>> .write.text "first = " =>> .write.data 40 =>> .write.anchor =>> .write.text "second = " =>> .write.data 42
meta.members = .read.end =>> .write.anchor =>> .write.text "first = " =>> .write.data 40 =>> .write.anchor =>> .write.text "second = " =>> .write.data 42
meta.steps = .read.end =>> .write.anchor =>> .write.text ".r ()" =>> .write.anchor =>> .write.text ".r " =>> .write.data 42
meta.consume_comma = .read.text "," =>> .read.sep =>> (.read.data >>= (\_ -> .read.end =>> .write.data 42))
meta.delete = .r ()
list_second = list.at 1 @meta.list
comma_is_macro_text = list.at 0 [@meta.consume_comma, 99]
@meta.declarations
object values with
  @meta.delete
  @meta.members
object hanging_values with @meta.members
do_effect = do
  @meta.steps
do_value = list.head (list.pure do_effect)
hanging_do_effect = do @meta.steps
hanging_do_value = list.head (list.pure hanging_do_effect)
parenthesized_do_value = list.head (list.pure (do @meta.steps))
tuple_effects = ((do @meta.steps), do .r 3)
"#,
        )
        .build()
        .expect("anchored source macro output should compile");

    for (name, expected) in [
        ("list_second", 2),
        ("comma_is_macro_text", 42),
        ("first", 40),
        ("second", 42),
        ("do_value", 42),
        ("hanging_do_value", 42),
        ("parenthesized_do_value", 42),
    ] {
        let value = assembler
            .get(module.value(), name)
            .unwrap_or_else(|error| panic!("`{name}` should exist: {error}"));
        assert_eq!(
            assembler
                .evaluate(&value)
                .unwrap_or_else(|error| panic!("`{name}` should evaluate: {error}")),
            PublicValue::integer(expected),
            "unexpected value for `{name}`",
        );
    }
    for object_name in ["values", "hanging_values"] {
        let values = assembler
            .get(module.value(), object_name)
            .unwrap_or_else(|error| {
                panic!("generated object `{object_name}` should exist: {error}")
            });
        for (name, expected) in [("first", 40), ("second", 42)] {
            let value = assembler.get(&values, name).unwrap_or_else(|error| {
                panic!("object member `{object_name}.{name}` should exist: {error}")
            });
            assert_eq!(
                assembler.evaluate(&value).unwrap_or_else(|error| {
                    panic!("object member `{object_name}.{name}` should evaluate: {error}")
                }),
                PublicValue::integer(expected),
            );
        }
    }
}

#[test]
fn source_macro_anchor_contract_rejects_ambiguous_or_empty_items() {
    for (body, invocation, expected) in [
        (
            ".write.anchor =>> .write.text \"generated = 42\"",
            "answer = @meta.bad",
            "start of its logical item",
        ),
        (
            ".write.anchor =>> .write.text \"generated = 42\"",
            "answer = consume @meta.bad",
            "start of its logical item",
        ),
        (
            ".write.anchor =>> .write.data 42",
            "answer = [@meta.bad]",
            "start of its logical item",
        ),
        (
            ".write.anchor =>> .write.text \"generated = 42\"",
            "@meta.bad 1",
            "complete input item",
        ),
        (
            ".write.anchor =>> .write.text \".r 42\"",
            "answer = (do @meta.bad where x = 1)",
            "complete input item",
        ),
        (
            ".write.anchor =>> .write.text \".r 42\"",
            "answer = (do @meta.bad, do .r 1)",
            "complete input item",
        ),
        (
            ".write.anchor =>> .write.anchor",
            "@meta.bad",
            "empty expansion item",
        ),
        (
            ".write.text \"generated = 42\" =>> .write.anchor",
            "@meta.bad",
            "first output operation",
        ),
        (
            ".write.anchor =>> .write.text \"generated = 42\" =>> .write.anchor",
            "@meta.bad",
            "empty or unclosed layout item",
        ),
        (
            ".write.layout (.r ())",
            "answer = @meta.bad",
            "requires at least one anchored item",
        ),
    ] {
        let assembler = Assembler::default();
        let error = assembler
            .module(["invalid_macro_layout_test"])
            .script(
                "g",
                format!(
                    "language g0\nimport 'std\nmeta.macro.env = {{}}\nmeta.bad = {body}\n{invocation}\n"
                ),
            )
            .build()
            .expect_err("invalid anchored output should reject its module");
        assert!(
            error
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message().contains(expected)),
            "unexpected diagnostics for `{body}` at `{invocation}`: {:?}",
            error.diagnostics()
        );
    }
}

#[test]
fn source_macro_rejects_reserved_or_unbalanced_generated_text() {
    for (body, expected) in [
        (".write.text \"@next\"", "cannot emit `@`"),
        (".write.text \"(\"", "unclosed delimiter"),
        (
            ".write.text \"first\nsecond\"",
            "use `.write.layout` for layout",
        ),
    ] {
        let assembler = Assembler::default();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let _subscription = assembler
            .diagnostic_bus()
            .subscribe(DiagnosticMessages(observed.clone()));
        let result = assembler
            .module(["invalid_macro_source_test"])
            .script(
                "g",
                format!(
                    "language g0\nimport 'std\nmeta.macro.env = {{}}\nmeta.bad = {body}\nanswer = @meta.bad\n"
                ),
            )
            .build();
        result.expect_err("invalid generated text should reject the module");
        let messages = observed
            .lock()
            .expect("diagnostic observation mutex should not be poisoned");
        assert!(
            messages.iter().any(|message| message.contains(expected)),
            "unexpected diagnostics for {body}: {messages:?}"
        );
    }
}

#[test]
fn macro_reader_must_balance_only_the_delimiters_it_opens() {
    let input = || {
        MacroInput::new(
            vec![
                MacroInputElement {
                    kind: MacroInputKind::Text {
                        text: Arc::from("("),
                        delimiter: Some((MacroDelimiter::Parenthesis, true)),
                    },
                    separated: false,
                    start: 0,
                    end: 1,
                },
                MacroInputElement {
                    kind: MacroInputKind::Text {
                        text: Arc::from("value"),
                        delimiter: None,
                    },
                    separated: false,
                    start: 1,
                    end: 6,
                },
                MacroInputElement {
                    kind: MacroInputKind::Text {
                        text: Arc::from(")"),
                        delimiter: Some((MacroDelimiter::Parenthesis, false)),
                    },
                    separated: false,
                    start: 6,
                    end: 7,
                },
            ],
            0,
            7,
        )
    };
    let (assembler, balanced) = compile_effects(".read.text \"(value)\" =>> .read.end =>> .r ()");
    run_macro_effect(
        &assembler.test_compilation_execution(),
        balanced.as_core().clone(),
        Value::Dict(Dict::new_sync()),
        input(),
    )
    .expect("balanced input delimiters should be accepted");

    let (assembler, unbalanced) = compile_effects(".read.text \"(\" =>> .r ()");
    let error = run_macro_effect(
        &assembler.test_compilation_execution(),
        unbalanced.as_core().clone(),
        Value::Dict(Dict::new_sync()),
        input(),
    )
    .expect_err("a successful branch may not leave its input delimiter open");
    assert!(error.message().contains("input delimiter"));
}

#[test]
fn declaration_macros_expand_right_to_left_and_share_the_evolving_view() {
    let assembler = Assembler::default();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let _subscription = assembler
        .diagnostic_bus()
        .subscribe(DiagnosticMessages(observed.clone()));
    let module = assembler
        .module(["reverse_macro_test"])
        .script(
            "g",
            r#"language g0
import 'std
meta.macro.env = {}
meta.pass = .read.sep =>> .read.data >>= (\value -> .write.data value =>> .r ())
meta.choose = .alt (.read.text "never" =>> .fail) (.read.sep =>> .read.data >>= (\value -> .write.data value =>> .r ()))
meta.inner = .log 'info { msg:{ text:"inner once" } } =>> .write.data 42
meta.left = .log 'info { msg:{ text:"left source order" } } =>> .write.text "40"
meta.right = .log 'info { msg:{ text:"right source order" } } =>> .write.data 2
forwarded = @meta.pass @meta.inner
chosen = @meta.choose @meta.inner
sum = @meta.left + @meta.right
nested = list.at 0 [(@meta.right)]
layout = list.at 1 [
  @meta.left,
  @meta.right]
"#,
        )
        .build()
        .expect("right-to-left source macros should compile");

    for (name, expected) in [
        ("forwarded", 42),
        ("chosen", 42),
        ("sum", 42),
        ("nested", 2),
        ("layout", 2),
    ] {
        let value = assembler
            .get(module.value(), name)
            .unwrap_or_else(|error| panic!("`{name}` should exist: {error}"));
        assert_eq!(
            assembler
                .evaluate(&value)
                .unwrap_or_else(|error| panic!("`{name}` should evaluate: {error}")),
            PublicValue::integer(expected),
        );
    }

    let messages = observed
        .lock()
        .expect("diagnostic observation mutex should not be poisoned");
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.as_str() == "inner once")
            .count(),
        2,
        "each original inner invocation should execute exactly once"
    );
    let source_order = messages
        .iter()
        .filter(|message| message.contains("source order"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        source_order,
        [
            "left source order",
            "right source order",
            "right source order",
            "left source order",
            "right source order",
        ],
        "direct macro diagnostics should publish in source order per declaration"
    );
}

#[test]
fn rightward_replacement_size_and_deletion_do_not_invalidate_left_invocations() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["stable_macro_worklist_test"])
        .script(
            "g",
            r#"language g0
import 'std
meta.macro.env = {}
meta.left = .write.text "40"
meta.wide = .write.text "(1 + 1)"
meta.delete = .r ()
sum = @meta.left + @meta.wide
second = list.at 1 [@meta.left, @meta.delete @meta.wide]
"#,
        )
        .build()
        .expect("rightward edits should leave left invocation IDs stable");

    for (name, expected) in [("sum", 42), ("second", 2)] {
        let value = assembler
            .get(module.value(), name)
            .unwrap_or_else(|error| panic!("`{name}` should exist: {error}"));
        assert_eq!(
            assembler
                .evaluate(&value)
                .unwrap_or_else(|error| panic!("`{name}` should evaluate: {error}")),
            PublicValue::integer(expected),
        );
    }
}
