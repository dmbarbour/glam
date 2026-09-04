use super::*;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::core::{
    Dict, EvaluationHalt, Key, LazyValue, List, OpaqueValue, Value as CoreValue, keys,
};
use crate::diagnostic::{CompilationInvocationId, CompilationTrace, Severity};
use crate::eval;
use crate::evaluation::{EvaluationMachinePoll, EvaluationTaskMachine, ReflectionTaskProfile};
use crate::number::Number;
use crate::reflection::{ReflectionEffects, coordinator_task_launcher};
use crate::source::{SourceArtifact, SourceIdentity};

mod diagnostic_tests;
mod runtime_tests;

use runtime_tests::{decode_test_integer, input_transaction};

struct FailedReasoningTask;

fn access_path(assembler: &Assembler, root: &Value, path: &str) -> Result<Value, Error> {
    let values = assembler.values();
    let mut value = root.clone();
    for part in path.split('.') {
        value = values.access(&value, values.atom_from_text(part))?;
    }
    Ok(value)
}

fn binary_at(assembler: &Assembler, root: &Value, path: &str) -> Result<Bytes, Error> {
    assembler.to_binary(&access_path(assembler, root, path)?)
}

fn same_representation(assembler: &Assembler, left: &Value, right: &Value) -> bool {
    assembler
        .reflection()
        .same_representation(left, right)
        .expect("test values should belong to the assembler runtime")
}

fn public_value(values: &crate::core::CoreValueFactory, value: CoreValue) -> Value {
    Values::from_core_factory(values.clone()).wrap(value)
}

fn value_i64(assembler: &Assembler, value: &Value) -> Option<i64> {
    assembler.evaluator().eval(value).unwrap().as_i64().unwrap()
}

fn value_is_undefined(assembler: &Assembler, value: &Value) -> bool {
    assembler
        .evaluator()
        .eval(value)
        .unwrap()
        .same_representation(&assembler.values().empty_dict())
        .unwrap()
}

fn assert_unclaimed_lazy(value: &Value) {
    let core = value.clone_core_for_test();
    let CoreValue::Lazy(lazy) = &core else {
        panic!(
            "expected a lazy value, received {}",
            core.diagnostic_kind_name()
        );
    };
    assert!(
        lazy.cached().is_none(),
        "constructor must not cache a result"
    );
    assert!(
        lazy.source_snapshot().is_some(),
        "constructor must leave the lazy producer available"
    );
}

fn assert_unobserved_promise(value: &Value) {
    let core = value.clone_core_for_test();
    let CoreValue::Promised(promise) = &core else {
        panic!(
            "expected a promised value, received {}",
            core.diagnostic_kind_name()
        );
    };
    assert!(
        promise.assignment().is_none(),
        "constructor must not assign the promise"
    );
    assert_eq!(
        promise.exact_subscription_count(),
        0,
        "constructor must not subscribe demand to the promise"
    );
}

impl EvaluationTaskMachine for FailedReasoningTask {
    fn poll(
        &mut self,
        context: &crate::evaluation::EvaluationPollContext,
        _step_budget: usize,
    ) -> EvaluationMachinePoll {
        EvaluationMachinePoll::Failed(context.root_failure(Arc::new(
            crate::core::EvaluationFailure::message("public reasoning failure"),
        )))
    }
}

fn test_compilation_trace(source: &str) -> CompilationTrace {
    let source = SourceArtifact::new(Bytes::from_static(b"source"), SourceIdentity::file(source));
    CompilationTrace::root(
        CompilationInvocationId::new(1),
        &source,
        Arc::from(["test".to_owned()]),
    )
}

fn definition_context(value: &CoreValue) -> Option<&Dict> {
    let CoreValue::Dict(frame) = value else {
        return None;
    };
    let CoreValue::Dict(context) = frame.get(&*keys::G)? else {
        return None;
    };
    Some(context)
}

fn diagnostic_contexts(assembler: &Assembler, diagnostic: &Diagnostic) -> Vec<CoreValue> {
    let emission = eval::eval_value(
        &assembler.eval_context(),
        &diagnostic.emission().clone_core_for_test(),
    )
    .expect("diagnostic emission should evaluate");
    let CoreValue::Dict(emission) = emission else {
        panic!("diagnostic emission should be a dictionary");
    };
    let message = eval::eval_value(
        &assembler.eval_context(),
        emission
            .get(&*keys::MSG)
            .expect("diagnostic should define msg"),
    )
    .expect("diagnostic msg should evaluate");
    let CoreValue::Dict(message) = message else {
        panic!("diagnostic msg should be a dictionary");
    };
    let contexts = eval::eval_value(
        &assembler.eval_context(),
        message
            .get(&*keys::CONTEXT)
            .expect("diagnostic msg should define context"),
    )
    .expect("diagnostic context should evaluate");
    let CoreValue::List(contexts) = contexts else {
        panic!("diagnostic context should be a list");
    };
    eval::list_to_value_items(&assembler.eval_context(), &contexts)
        .expect("diagnostic contexts should be concrete values")
}

#[test]
fn runtimes_own_independent_local_identity_domains_and_value_factories() {
    let first = EvaluationRuntime::new(0).expect("first runtime should build");
    let second = EvaluationRuntime::new(0).expect("second runtime should build");
    assert_ne!(first.id(), second.id());

    let allocate_one_of_each = |runtime: &EvaluationRuntime| {
        let ids = &runtime.state.shared_resources.ids;
        (
            ids.evaluation_session().get(),
            ids.evaluation_task().unwrap().get(),
            ids.evaluation_wait().unwrap().get(),
            ids.deferred_value().get(),
            ids.reasoning_session().get(),
            ids.input_endpoint().unwrap().get(),
            ids.output_endpoint().unwrap().get(),
            ids.delivery().unwrap().get(),
        )
    };
    assert_eq!(allocate_one_of_each(&first), allocate_one_of_each(&second));

    assert_eq!(first.values().runtime_id(), first.id());
    assert_eq!(second.values().runtime_id(), second.id());
    let assembler = Assembler::builder()
        .evaluation_runtime(first.clone())
        .build()
        .expect("assembler should retain its selected runtime");
    assert_eq!(assembler.values().runtime_id(), first.id());

    let first_values = first.values().core;
    let first_unit = first_values.unit();
    let first_lazy = LazyValue::semantic_thunk(&first_values, "first runtime", move |_| {
        Ok(first_unit.clone())
    });
    let second_values = second.values().core;
    let second_unit = second_values.unit();
    let second_lazy = LazyValue::semantic_thunk(&second_values, "second runtime", move |_| {
        Ok(second_unit.clone())
    });
    assert_eq!(first_lazy.id().get(), second_lazy.id().get());
}

#[test]
fn runtime_shared_resources_do_not_retain_runtime_lifecycle_owners() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let runtime_id = runtime.id();
    let state = Arc::downgrade(&runtime.state);
    let coordinator = Arc::downgrade(&runtime.state.work);
    let executor = Arc::downgrade(&runtime.state.executor);
    let profile = Arc::downgrade(&runtime.default_reflection_profile);
    let value_domain = Arc::downgrade(runtime.state.shared_resources.values.core().value_domain());
    let resources = runtime.state.shared_resources.clone();
    let retained_resources = Arc::downgrade(&resources);

    drop(runtime);

    assert!(state.upgrade().is_none());
    assert!(coordinator.upgrade().is_none());
    assert!(executor.upgrade().is_none());
    assert!(profile.upgrade().is_none());
    assert!(value_domain.upgrade().is_some());
    assert!(resources.work.upgrade().is_none());
    assert_eq!(resources.id, runtime_id);
    assert_eq!(resources.values.core().runtime_id(), runtime_id);

    let before = resources.observations.current();
    let mutation = resources.mutation_admission.mutation_guard();
    publish_runtime_observation(&resources, mutation);
    assert!(resources.observations.current() > before);
    assert!(resources.ids.reasoning_session().get() > 0);
    let snapshot = resources
        .transactions
        .state
        .lock()
        .expect("runtime transaction mutex should not be poisoned")
        .reflection
        .snapshot();
    drop(snapshot);

    drop(resources);
    assert!(retained_resources.upgrade().is_none());
    assert!(value_domain.upgrade().is_none());
}

#[test]
fn public_values_retain_only_the_runtime_value_domain() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let state = Arc::downgrade(&runtime.state);
    let coordinator = Arc::downgrade(&runtime.state.work);
    let profile = Arc::downgrade(&runtime.default_reflection_profile);
    let values = runtime.values();
    let value_domain = Arc::downgrade(values.core.value_domain());

    drop(runtime);

    assert!(state.upgrade().is_none());
    assert!(coordinator.upgrade().is_none());
    assert!(profile.upgrade().is_none());
    assert!(value_domain.upgrade().is_some());
    assert_eq!(values.integer(42).runtime_id(), values.runtime_id());

    drop(values);
    assert!(value_domain.upgrade().is_none());
}

#[test]
fn runtime_value_domain_has_no_scheduler_or_profile_backedge() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let state = Arc::downgrade(&runtime.state);
    let coordinator = Arc::downgrade(&runtime.state.work);
    let executor = Arc::downgrade(&runtime.state.executor);
    let profile = Arc::downgrade(&runtime.default_reflection_profile);
    let resources = Arc::downgrade(&runtime.state.shared_resources);
    let values = runtime.values();
    let value_domain = Arc::downgrade(values.core.value_domain());
    let root = values.core.with_managed_values(|scope| {
        let allocator = scope
            .allocator::<u64>()
            .expect("the lifecycle fixture should fit one managed slot");
        scope.root(allocator.alloc(41))
    });
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should seal the runtime reflection profile");

    drop(assembler);
    drop(runtime);

    assert!(state.upgrade().is_none());
    assert!(coordinator.upgrade().is_none());
    assert!(executor.upgrade().is_none());
    assert!(profile.upgrade().is_none());
    assert!(resources.upgrade().is_none());
    assert!(value_domain.upgrade().is_some());
    values.core.with_managed_values(|scope| {
        assert_eq!(*scope.get(&root), 41);
    });

    drop(values);
    assert!(value_domain.upgrade().is_none());
    drop(root);
}

#[test]
fn bare_public_values_do_not_retain_the_runtime_value_domain() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let values = runtime.values();
    let runtime_id = values.runtime_id();
    let value_domain = Arc::downgrade(values.core.value_domain());
    let value = values.integer(42);

    drop(values);
    drop(runtime);

    assert!(value_domain.upgrade().is_none());
    assert_eq!(value.runtime_id(), runtime_id);
}

#[test]
fn composite_construction_preserves_provenance_errors() {
    let owner = EvaluationRuntime::new(0).expect("owner runtime should build");
    let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
    let values = owner.values();
    let foreign_value = foreign.values().integer(7);

    assert!(values.list([foreign_value.clone()]).is_err());
    assert!(values.record([("value", foreign_value.clone())]).is_err());
    assert!(
        values
            .dictionary([(values.atom_from_text("key"), foreign_value.clone())])
            .is_err()
    );
    assert!(values.empty_object(foreign_value.clone()).is_err());
    assert!(
        values
            .access(&values.empty_dict(), foreign_value.clone())
            .is_err()
    );
    assert!(values.access(&foreign_value, values.text("key")).is_err());
    assert!(
        values
            .apply(&values.integer(0), [foreign_value.clone()])
            .is_err()
    );
    assert!(
        values
            .access_path(&values.empty_dict(), [foreign_value.clone()])
            .is_err()
    );
    assert!(
        values
            .access_path(&foreign_value, std::iter::empty::<Value>())
            .is_err()
    );
    assert!(values.access_names(&foreign_value, ["member"]).is_err());
    assert!(values.list_slice(&foreign_value, 0..1).is_err());
    assert!(values.anno_binary(foreign_value.clone()).is_err());
    assert!(values.anno_array(foreign_value.clone()).is_err());
    assert!(values.anno_deque(foreign_value.clone()).is_err());
    assert!(
        values
            .anno(foreign_value.clone(), values.empty_dict())
            .is_err()
    );
    assert!(
        values
            .anno(values.atom_from_text("binary"), foreign_value.clone())
            .is_err()
    );
    assert!(
        values
            .after_reflection(foreign_value.clone(), values.text("target"))
            .is_err()
    );
    assert!(
        values
            .after_reflection(values.empty_dict(), foreign_value)
            .is_err()
    );
}

#[test]
fn access_and_annotation_construction_do_not_demand_inputs() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let demanded = Arc::new(AtomicBool::new(false));
    let demanded_by_thunk = demanded.clone();
    let core_values = assembler.core_values();
    let unit = core_values.unit();
    let lazy = public_value(
        &core_values,
        CoreValue::Lazy(LazyValue::semantic_thunk(
            &core_values,
            "no-demand facade fixture",
            move |_| {
                demanded_by_thunk.store(true, Ordering::SeqCst);
                Ok(unit.clone())
            },
        )),
    );
    let (promise, resolver) = assembler.promise("no-demand facade fixture");

    let access = values
        .access(&lazy, values.atom_from_text("member"))
        .expect("same-runtime access construction should succeed");
    let annotation = values
        .anno(lazy.clone(), promise.clone())
        .expect("same-runtime annotation construction should succeed");

    assert!(!demanded.load(Ordering::SeqCst));
    assert_unclaimed_lazy(&lazy);
    assert_unobserved_promise(&promise);
    assert_unclaimed_lazy(&access);
    assert_unclaimed_lazy(&annotation);

    resolver
        .fail_message("fixture complete")
        .expect("an unobserved promise remains independently resolvable");
}

#[test]
fn values_apply_is_lazy_and_matches_source_application_order() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let module = assembler
        .module(["values_apply"])
        .script(
            "g",
            concat!(
                "language g0\n",
                "difference = \\ left right -> left - right\n",
                "direct = difference 50 8\n",
            ),
        )
        .build()
        .expect("application fixture should compile");
    let function = access_path(&assembler, module.value(), "difference")
        .expect("fixture should define its function");
    let direct = access_path(&assembler, module.value(), "direct")
        .expect("fixture should define its direct result");

    let applied = values
        .apply(&function, [values.integer(50), values.integer(8)])
        .expect("same-runtime application should construct");
    assert_unclaimed_lazy(&applied);
    let applied = assembler
        .evaluate(&applied)
        .expect("constructed application should evaluate");
    let direct_result = assembler
        .evaluate(&direct)
        .expect("source application should evaluate");
    assert!(same_representation(&assembler, &applied, &direct_result));

    let partial = values
        .apply(&function, [values.integer(50)])
        .expect("partial application should construct");
    let nested = values
        .apply(&partial, [values.integer(8)])
        .expect("nested application should construct");
    let nested = assembler
        .evaluate(&nested)
        .expect("nested application should evaluate");
    let direct_result = assembler
        .evaluate(&direct)
        .expect("source application should remain reusable");
    assert!(same_representation(&assembler, &nested, &direct_result));

    let (promise, resolver) = assembler.promise("lazy function application");
    let promised_application = values
        .apply(&promise, [values.integer(1)])
        .expect("a promised function may be applied lazily");
    assert_unobserved_promise(&promise);
    assert_unclaimed_lazy(&promised_application);
    resolver
        .fail_message("fixture complete")
        .expect("the promised function was not observed");
}

#[test]
fn value_paths_preserve_complete_names_and_empty_identity() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let root = values
        .dictionary([(values.atom_from_text("a.b"), values.integer(42))])
        .expect("immediate dictionary should construct");

    let complete_name = values
        .access_names(&root, ["a.b"])
        .and_then(|value| assembler.evaluate(&value))
        .expect("a dotted complete name should remain one atom");
    assert_eq!(value_i64(&assembler, &complete_name), Some(42));

    let split_name = values
        .access_names(&root, ["a", "b"])
        .and_then(|value| assembler.evaluate(&value))
        .expect("a missing semantic path returns undefined");
    assert!(value_is_undefined(&assembler, &split_name));

    let empty_path = values
        .access_path(&root, std::iter::empty::<Value>())
        .expect("an empty path should preserve its root");
    assert!(same_representation(&assembler, &empty_path, &root));
    let empty_application = values
        .apply(&root, std::iter::empty::<Value>())
        .expect("an empty application should preserve its function");
    assert!(same_representation(&assembler, &empty_application, &root));
}

#[test]
fn list_and_representation_constructors_are_lazy_semantic_operations() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let (promise, resolver) = assembler.promise("lazy list facade fixture");

    let slice = values
        .list_slice(&promise, 1..3)
        .expect("a promised list may be sliced lazily");
    let binary = values
        .anno_binary(promise.clone())
        .expect("binary annotation should construct lazily");
    let array = values
        .anno_array(promise.clone())
        .expect("array annotation should construct lazily");
    let deque = values
        .anno_deque(promise.clone())
        .expect("deque annotation should construct lazily");

    assert_unobserved_promise(&promise);
    for constructed in [&slice, &binary, &array, &deque] {
        assert_unclaimed_lazy(constructed);
    }
    resolver
        .fail_message("fixture complete")
        .expect("none of the constructors observed the promise");

    let compact = values.bytes(Bytes::from_static(b"abcdef"));
    let compact_slice = values
        .list_slice(&compact, 1..5)
        .and_then(|slice| values.anno_binary(slice))
        .expect("binary slice pipeline should construct");
    assert_eq!(
        assembler
            .to_binary(&compact_slice)
            .expect("binary slice pipeline should evaluate"),
        b"bcde".as_slice()
    );
    let reversed = Range { start: 5, end: 3 };
    assert!(
        values
            .list_slice(&compact, reversed)
            .and_then(|slice| assembler.evaluate(&slice))
            .is_err(),
        "range ordering remains a semantic slice failure"
    );
}

#[test]
fn array_and_deque_annotations_preserve_lazy_elements() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let core_values = assembler.core_values();
    let element = public_value(
        &core_values,
        CoreValue::Lazy(LazyValue::semantic_thunk(
            &core_values,
            "lazy list element",
            |_| Ok(CoreValue::Number(Number::integer(7))),
        )),
    );
    let list = values
        .list([element.clone(), values.integer(8)])
        .expect("strict list spine should construct");

    let array = values
        .anno_array(list.clone())
        .and_then(|array| assembler.evaluate(&array))
        .expect("array annotation should normalize the spine");
    let array = array.clone_core_for_test();
    let CoreValue::List(array) = &array else {
        panic!("array annotation must return a list");
    };
    let items = eval::list_to_value_items(&assembler.eval_context(), array)
        .expect("array representation should enumerate immediately");
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], CoreValue::Lazy(_)));
    assert_unclaimed_lazy(&element);

    let deque = values
        .anno_deque(list)
        .and_then(|deque| assembler.evaluate(&deque))
        .expect("deque annotation should normalize the spine");
    let deque = deque.clone_core_for_test();
    let CoreValue::List(deque) = &deque else {
        panic!("deque annotation must return a list");
    };
    let items = eval::list_to_value_items(&assembler.eval_context(), deque)
        .expect("balanced deque should enumerate after normalization");
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], CoreValue::Lazy(_)));
    assert_unclaimed_lazy(&element);
}

#[test]
fn dictionary_composition_is_lazy_and_rejects_foreign_members() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let (promise, resolver) = assembler.promise("lazy dictionary facade fixture");

    let singleton = values
        .dict_singleton(promise.clone(), promise.clone())
        .expect("singleton construction should remain lazy");
    let union = values
        .dict_union(promise.clone(), values.empty_dict())
        .expect("union construction should remain lazy");
    let update = values
        .dict_update(promise.clone(), promise.clone(), promise.clone())
        .expect("update construction should remain lazy");

    assert_unobserved_promise(&promise);
    for constructed in [&singleton, &union, &update] {
        assert_unclaimed_lazy(constructed);
    }
    resolver
        .fail_message("fixture complete")
        .expect("dictionary constructors did not observe the promise");

    let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
    let foreign = foreign.values().integer(1);
    assert!(
        values
            .dict_singleton(values.atom_from_text("key"), foreign.clone())
            .is_err()
    );
    assert!(
        values
            .dict_union(values.empty_dict(), foreign.clone())
            .is_err()
    );
    assert!(
        values
            .dict_update(values.empty_dict(), values.list([]).unwrap(), foreign)
            .is_err()
    );
}

#[test]
fn dictionary_composition_matches_source_literals_union_and_updates() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let module = assembler
        .module(["dictionary_facade"])
        .script(
            "g",
            concat!(
                "language g0\n",
                "left = {base:1, nested:{x:2}}\n",
                "right = {added:3, nested:{y:4}}\n",
                "source_union = {left, right}\n",
                "source_conflict = {left, {base:3}}\n",
                "source_update = left with { base := 5; nested.z = 6 }\n",
                "source_remove = source_update with { base := {} }\n",
            ),
        )
        .build()
        .expect("dictionary facade fixture should compile");

    let left = access_path(&assembler, module.value(), "left").unwrap();
    let right = access_path(&assembler, module.value(), "right").unwrap();
    let union = values
        .dict_union(left.clone(), right)
        .expect("same-runtime union should construct");
    let update = values
        .dict_update(
            left,
            values
                .list([values.atom_from_text("base")])
                .expect("static path should construct"),
            values.integer(5),
        )
        .and_then(|dictionary| {
            values.dict_update(
                dictionary,
                values.list([values.atom_from_text("nested"), values.atom_from_text("z")])?,
                values.integer(6),
            )
        })
        .expect("same-runtime nested update should construct");
    let removed = values
        .dict_update(
            update.clone(),
            values.list([values.atom_from_text("base")]).unwrap(),
            values.empty_dict(),
        )
        .expect("update to undefined should construct removal");

    let assert_field = |actual: &Value, source: &str, path: &[&str]| {
        let expected = access_path(&assembler, module.value(), source).unwrap();
        let actual = values
            .access_names(actual, path.iter().copied())
            .and_then(|value| assembler.evaluate(&value))
            .unwrap();
        let expected = values
            .access_names(&expected, path.iter().copied())
            .and_then(|value| assembler.evaluate(&value))
            .unwrap();
        assert!(
            same_representation(&assembler, &actual, &expected),
            "field path {path:?} should match"
        );
    };

    for path in [
        ["base"].as_slice(),
        ["added"].as_slice(),
        ["nested", "x"].as_slice(),
        ["nested", "y"].as_slice(),
    ] {
        assert_field(&union, "source_union", path);
    }
    let conflict = values
        .dict_union(
            access_path(&assembler, module.value(), "left").unwrap(),
            values
                .dict_singleton(values.atom_from_text("base"), values.integer(3))
                .unwrap(),
        )
        .unwrap();
    let source_conflict = access_path(&assembler, module.value(), "source_conflict").unwrap();
    assert!(
        values
            .access_names(&conflict, ["base"])
            .and_then(|value| assembler.evaluate(&value))
            .is_err()
    );
    assert!(
        values
            .access_names(&source_conflict, ["base"])
            .and_then(|value| assembler.evaluate(&value))
            .is_err()
    );
    for path in [
        ["base"].as_slice(),
        ["nested", "x"].as_slice(),
        ["nested", "z"].as_slice(),
    ] {
        assert_field(&update, "source_update", path);
    }
    assert_field(&removed, "source_remove", &["base"]);

    let singleton = values
        .dict_singleton(values.atom_from_text("only"), values.integer(9))
        .expect("singleton should construct");
    let only = values
        .access_names(&singleton, ["only"])
        .and_then(|value| assembler.evaluate(&value))
        .unwrap();
    assert_eq!(value_i64(&assembler, &only), Some(9));
    let identity = values
        .dict_union(values.empty_dict(), singleton.clone())
        .expect("empty dictionary should be union identity");
    assert_field(&identity, "source_remove", &["missing"]);
    let only = values
        .access_names(&identity, ["only"])
        .and_then(|value| assembler.evaluate(&value))
        .unwrap();
    assert_eq!(value_i64(&assembler, &only), Some(9));
}

#[test]
fn cached_defined_selection_helpers_match_glam_undefined_semantics() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let core_values = assembler.core_values();
    let defined_or = public_value(
        &core_values,
        crate::g_syntax::defined_or_value(&core_values),
    );
    let require_defined = public_value(
        &core_values,
        crate::g_syntax::require_defined_value(&core_values),
    );
    let fallback = values.integer(7);

    for undefined in [
        values.empty_dict(),
        values
            .record([("nested", values.empty_dict())])
            .expect("logical undefined fixture should construct"),
    ] {
        let selected = values
            .apply(&defined_or, [fallback.clone(), undefined])
            .and_then(|value| assembler.evaluate(&value))
            .expect("defined-or should select its fallback");
        assert_eq!(value_i64(&assembler, &selected), Some(7));
    }

    let selected = values
        .apply(&defined_or, [fallback, values.integer(9)])
        .and_then(|value| assembler.evaluate(&value))
        .expect("defined-or should preserve a defined candidate");
    assert_eq!(value_i64(&assembler, &selected), Some(9));

    let required = values
        .apply(
            &require_defined,
            [values.text("required.value"), values.integer(11)],
        )
        .and_then(|value| assembler.evaluate(&value))
        .expect("required helper should preserve a defined value");
    assert_eq!(value_i64(&assembler, &required), Some(11));
    assert!(
        values
            .apply(
                &require_defined,
                [values.text("required.value"), values.empty_dict()],
            )
            .and_then(|value| assembler.evaluate(&value))
            .is_err(),
        "required helper should reject a logically undefined value"
    );
}

#[test]
fn evaluated_values_preserve_whnf_identity_and_scalar_views() {
    let assembler = Assembler::new();
    let values = assembler.values();

    for integer in [i64::MIN, -1, 0, 1, i64::MAX] {
        let original = values.integer(integer);
        let evaluated = EvaluatedValue::from_whnf(&values, original.clone());
        assert!(same_representation(
            &assembler,
            evaluated.as_value(),
            &original
        ));
        assert!(same_representation(
            &assembler,
            &evaluated.clone().into_value(),
            &original
        ));
        assert!(same_representation(
            &assembler,
            &Value::from(evaluated.clone()),
            &original
        ));
        assert_eq!(evaluated.as_i64().unwrap(), Some(integer));
        assert_eq!(evaluated.number_text().unwrap(), Some(integer.to_string()));
        assert_eq!(evaluated.as_value().runtime_id(), values.runtime_id());
    }

    let rational = EvaluatedValue::from_whnf(
        &values,
        values
            .rational(-3, 4)
            .expect("nonzero denominator should construct"),
    );
    assert_eq!(rational.as_rational_i64().unwrap(), Some((-3, 4)));
    assert_eq!(rational.number_text().unwrap().as_deref(), Some("-3/4"));
    assert_eq!(rational.as_f64().unwrap(), Some(-0.75));

    let large = EvaluatedValue::from_whnf(
        &values,
        values
            .number_from_text("123456789012345678901234567890")
            .expect("arbitrary precision integer should parse"),
    );
    assert_eq!(large.as_i64().unwrap(), None);
    assert_eq!(
        large.number_text().unwrap().as_deref(),
        Some("123456789012345678901234567890")
    );

    let bytes = EvaluatedValue::from_whnf(&values, values.bytes(Bytes::from_static(b"bytes")));
    assert_eq!(
        bytes.as_bytes().unwrap().as_deref(),
        Some(b"bytes".as_slice())
    );
    assert_eq!(bytes.as_i64().unwrap(), None);
    values
        .core
        .collect_managed_for_test()
        .expect("completed extraction must release its temporary mutator");
}

#[test]
fn effect_tokens_are_domain_scoped_unforgeable_and_revoked_with_the_domain() {
    let runtime = EvaluationRuntime::new(0).unwrap();
    let values = runtime.values();
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .unwrap();
    let domain = EffectTokenDomain::new(&values);
    let other = EffectTokenDomain::<String>::new(&values);
    let state = Arc::downgrade(&domain.state);
    let token = domain.issue("path payload".to_owned());
    let evaluated = assembler.evaluator().eval(&token).unwrap();

    assert_eq!(
        domain.resolve(&evaluated).unwrap().as_deref(),
        Some(&"path payload".to_owned())
    );
    assert!(other.resolve(&evaluated).unwrap().is_none());
    assert!(
        domain
            .resolve(&assembler.evaluator().eval(&values.unit()).unwrap())
            .unwrap()
            .is_none()
    );

    let foreign = EvaluationRuntime::new(0).unwrap();
    let foreign_assembler = Assembler::builder()
        .evaluation_runtime(foreign.clone())
        .build()
        .unwrap();
    let foreign_value = foreign_assembler
        .evaluator()
        .eval(&foreign.values().unit())
        .unwrap();
    assert!(domain.resolve(&foreign_value).is_err());

    drop(domain);
    assert!(
        state.upgrade().is_none(),
        "escaped tokens weakly reference and do not retain their issuing domain"
    );
}

#[test]
fn evaluated_array_items_accept_only_one_strict_value_leaf() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let core_values = assembler.core_values();
    let lazy_element = public_value(
        &core_values,
        CoreValue::Lazy(LazyValue::semantic_thunk(
            &core_values,
            "unevaluated array member",
            |_| Ok(CoreValue::Number(Number::integer(1))),
        )),
    );
    let array = EvaluatedValue::from_whnf(
        &values,
        values
            .list([lazy_element.clone(), values.integer(2)])
            .expect("strict value leaf should construct"),
    );
    let annotated = values
        .anno_array(array.as_value().clone())
        .expect("strict array annotation should succeed");
    assert!(
        same_representation(&assembler, &annotated, array.as_value()),
        "an existing strict array should not allocate new demand work"
    );
    let items = array
        .array_items()
        .unwrap()
        .expect("strict value leaf should extract as an array");
    assert_eq!(items.len(), 2);
    assert!(same_representation(&assembler, &items[0], &lazy_element));
    assert_unclaimed_lazy(&items[0]);

    assert!(
        EvaluatedValue::from_whnf(&values, values.list([]).unwrap())
            .array_items()
            .unwrap()
            .is_some_and(|items| items.is_empty())
    );
    assert!(
        EvaluatedValue::from_whnf(&values, values.bytes(Bytes::from_static(b"bytes")))
            .array_items()
            .unwrap()
            .is_none()
    );

    let concatenated = public_value(
        &core_values,
        CoreValue::List(List::concat(
            List::from_values(vec![CoreValue::Number(Number::integer(1))]),
            List::from_values(vec![CoreValue::Number(Number::integer(2))]),
        )),
    );
    assert!(
        EvaluatedValue::from_whnf(&values, concatenated)
            .array_items()
            .unwrap()
            .is_none()
    );

    let deque = values
        .anno_deque(values.list([values.integer(1), values.integer(2)]).unwrap())
        .and_then(|value| assembler.evaluate(&value))
        .expect("deque should evaluate");
    assert!(
        EvaluatedValue::from_whnf(&values, deque)
            .array_items()
            .unwrap()
            .is_none()
    );

    let (promise, resolver) = assembler.promise("deferred array spine");
    let CoreValue::Promised(promise_core) = promise.clone_core_for_test() else {
        unreachable!("public promise must contain a promised core value")
    };
    let deferred_spine = public_value(
        &core_values,
        CoreValue::List(List::from_thunk(promise_core.clone().into())),
    );
    assert!(
        EvaluatedValue::from_whnf(&values, deferred_spine)
            .array_items()
            .unwrap()
            .is_none()
    );
    assert_unobserved_promise(&promise);
    resolver
        .fail_message("fixture complete")
        .expect("array inspection did not observe the deferred spine");
}

#[test]
fn value_evaluator_returns_a_runtime_rooted_whnf_witness() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let value = values.integer(42);
    let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
    assert!(
        assembler
            .evaluator()
            .eval(&foreign.values().integer(42))
            .is_err()
    );

    let evaluated = assembler
        .evaluator()
        .eval(&value)
        .expect("an immediate value should evaluate");
    assert_eq!(evaluated.as_i64().unwrap(), Some(42));
    assert_eq!(evaluated.as_value().runtime_id(), value.runtime_id());
    drop(assembler);
    assert_eq!(evaluated.as_i64().unwrap(), Some(42));
    assert!(
        values.list([evaluated.into_value()]).is_ok(),
        "the returned root remains in its original value domain"
    );
}

#[test]
fn evaluated_observer_does_not_retain_the_runtime_value_domain() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let values = runtime.values();
    let domain = Arc::downgrade(values.core.value_domain());
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should build");
    let evaluated = assembler
        .evaluator()
        .eval(&values.integer(42))
        .expect("immediate value should evaluate");
    let runtime_id = evaluated.as_value().runtime_id();

    assert_eq!(evaluated.as_i64().unwrap(), Some(42));
    drop(assembler);
    drop(values);
    drop(runtime);

    assert!(domain.upgrade().is_none());
    assert!(evaluated.as_i64().is_err());
    assert_eq!(evaluated.into_value().runtime_id(), runtime_id);
}

#[test]
fn value_evaluator_resumes_a_retained_resolver_promise_subscription() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let (promise, resolver) = assembler.promise("public evaluator wait fixture");
    let CoreValue::Promised(promise_core) = promise.clone_core_for_test() else {
        unreachable!("public promise must contain a promised core value")
    };
    let promise_core = promise_core.clone();
    let waiting = values
        .anno_binary(promise)
        .expect("binary annotation should wrap the promise without observing it");
    let error = assembler
        .evaluator()
        .eval(&waiting)
        .expect_err("a resolver-owned promise has no runtime-owned progress source");
    assert!(error.to_string().contains("blocked on wait token"));
    assert_eq!(promise_core.exact_subscription_count(), 1);
    resolver
        .resolve(values.text("resolved"))
        .expect("resolver should publish the promised value");
    let evaluated = assembler
        .evaluator()
        .eval(&waiting)
        .expect("promise completion should resume evaluation");
    assert_eq!(
        evaluated.as_bytes().unwrap().as_deref(),
        Some(b"resolved".as_slice())
    );
    assert_eq!(promise_core.exact_subscription_count(), 0);
}

#[test]
fn promise_resolver_drop_invokes_idempotent_retire_once() {
    let assembler = Assembler::default();
    let values = assembler.values();
    let baseline = values
        .core
        .collect_managed_for_test()
        .expect("the promise-resolver fixture should start collectible");

    let (promise, resolver) = assembler.promise("affine resolver retirement");
    let pending = values
        .core
        .collect_managed_for_test()
        .expect("the public promise and resolver roots should remain live");
    assert_eq!(pending.root_entries(), baseline.root_entries() + 2);

    resolver
        .resolve(values.integer(37))
        .expect("the fresh resolver should publish exactly once");
    let resolved = values
        .core
        .collect_managed_for_test()
        .expect("consuming the resolver should release its managed root");
    assert_eq!(resolved.root_entries(), baseline.root_entries() + 1);
    {
        let CoreValue::Promised(resolved_promise) = promise.clone_core_for_test() else {
            panic!("public promise should retain its managed promise identity")
        };
        assert_eq!(
            resolved_promise.assignment(),
            Some(Ok(CoreValue::Number(Number::integer(37))))
        );
    }

    drop(promise);
    let reclaimed = values
        .core
        .collect_managed_for_test()
        .expect("dropping the consumer should reclaim its value and promise cells");
    assert_eq!(reclaimed.root_entries(), baseline.root_entries());
    assert_eq!(reclaimed.finalized_slots(), 2);
}

#[test]
fn promise_resolver_drop_after_runtime_retirement_is_inert() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let values = runtime.values();
    let domain = Arc::downgrade(values.core.value_domain());
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should build");
    let (promise, resolver) = assembler.promise("retired runtime");

    drop(promise);
    drop(assembler);
    drop(values);
    drop(runtime);
    assert!(domain.upgrade().is_none());

    drop(resolver);
}

#[test]
fn promise_resolver_completion_after_runtime_retirement_is_rejected() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let values = runtime.values();
    let domain = Arc::downgrade(values.core.value_domain());
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should build");
    let (resolved_promise, resolved) = assembler.promise("retired resolution");
    let (failed_promise, failed) = assembler.promise("retired failure");
    let assignment = values.integer(43);

    drop((resolved_promise, failed_promise));
    drop(assembler);
    drop(values);
    drop(runtime);
    assert!(domain.upgrade().is_none());

    assert!(
        resolved
            .resolve(assignment)
            .expect_err("a retired runtime cannot accept promise assignment")
            .to_string()
            .contains("no longer available for promise completion")
    );
    assert!(
        failed
            .fail_message("late failure")
            .expect_err("a retired runtime cannot accept promise failure")
            .to_string()
            .contains("no longer available for promise completion")
    );
}

#[test]
fn value_evaluator_caches_lazy_success_and_preserves_structured_failure() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let core_values = assembler.core_values();
    let evaluations = Arc::new(AtomicUsize::new(0));
    let evaluations_by_thunk = evaluations.clone();
    let lazy = public_value(
        &core_values,
        CoreValue::Lazy(LazyValue::semantic_thunk(
            &core_values,
            "one evaluation",
            move |_| {
                evaluations_by_thunk.fetch_add(1, Ordering::SeqCst);
                Ok(CoreValue::Number(Number::integer(42)))
            },
        )),
    );
    assert_eq!(
        assembler.evaluator().eval(&lazy).unwrap().as_i64().unwrap(),
        Some(42)
    );
    assert_eq!(
        assembler.evaluator().eval(&lazy).unwrap().as_i64().unwrap(),
        Some(42)
    );
    assert_eq!(evaluations.load(Ordering::SeqCst), 1);

    let failure = values
        .anno(
            values.atom_from_text("error"),
            values
                .record([
                    (
                        "msg",
                        values
                            .record([("text", values.text("structured"))])
                            .unwrap(),
                    ),
                    ("detail", values.text("preserved")),
                ])
                .unwrap(),
        )
        .unwrap();
    let error = assembler
        .evaluator()
        .eval(&failure)
        .expect_err("error annotation should fail evaluation");
    assert_eq!(error.to_string(), "structured");
    assert!(error.structured_diagnostic().is_some());
}

#[test]
fn semantic_binary_slice_does_not_force_an_unused_poisoned_tail() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let core_values = assembler.core_values();
    let poison = LazyValue::semantic_thunk(&core_values, "unused binary tail", |_| {
        Err(EvaluationHalt::new("unused binary tail was forced"))
    });
    let source = public_value(
        &core_values,
        CoreValue::List(List::concat(
            List::from_bytes(Bytes::from_static(b"ok")),
            List::from_thunk(poison.clone().into()),
        )),
    );
    let binary = values
        .list_slice(&source, 0..2)
        .and_then(|slice| values.anno_binary(slice))
        .expect("slice and binary assertion should construct lazily");
    let evaluated = assembler
        .evaluator()
        .eval(&binary)
        .expect("prefix extraction should not observe the tail");
    assert_eq!(
        evaluated.as_bytes().unwrap().as_deref(),
        Some(b"ok".as_slice())
    );
    assert!(poison.cached().is_none());
    assert!(poison.source_snapshot().is_some());
}

#[test]
fn semantic_array_materialization_replaces_reflective_list_enumeration() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let list = values
        .list([values.integer(1), values.text("two")])
        .expect("list fixture should construct");
    let semantic = values
        .anno_array(list)
        .and_then(|array| assembler.evaluator().eval(&array))
        .expect("semantic array should evaluate")
        .array_items()
        .unwrap()
        .expect("array annotation should produce one strict value leaf");
    assert_eq!(semantic.len(), 2);
    assert_eq!(
        assembler
            .evaluator()
            .eval(&semantic[0])
            .unwrap()
            .as_i64()
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        assembler
            .evaluator()
            .eval(&semantic[1])
            .unwrap()
            .as_bytes()
            .unwrap()
            .as_deref(),
        Some(b"two".as_slice()),
    );

    let bytes = values
        .anno_array(values.bytes(Bytes::from_static(&[0, 255])))
        .and_then(|array| assembler.evaluator().eval(&array))
        .expect("compact binary should materialize as an array")
        .array_items()
        .unwrap()
        .expect("binary array should use a strict value leaf");
    assert_eq!(
        assembler
            .evaluator()
            .eval(&bytes[0])
            .unwrap()
            .as_i64()
            .unwrap(),
        Some(0)
    );
    assert_eq!(
        assembler
            .evaluator()
            .eval(&bytes[1])
            .unwrap()
            .as_i64()
            .unwrap(),
        Some(255)
    );
}

#[test]
fn reflection_kind_observes_an_unresolved_promise_without_demand() {
    let assembler = Assembler::new();
    let (promise, resolver) = assembler.promise("reflection kind fixture");
    assert_eq!(
        assembler.reflection().kind(&promise).unwrap(),
        ValueKind::Lazy
    );
    assert_unobserved_promise(&promise);

    let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
    assert!(
        assembler
            .reflection()
            .kind(&foreign.values().integer(1))
            .is_err()
    );
    resolver
        .fail_message("fixture complete")
        .expect("representation inspection did not observe the promise");
}

#[test]
fn assembler_boundaries_reject_foreign_values_before_evaluation_or_storage() {
    let runtime = EvaluationRuntime::new(0).expect("owner runtime should build");
    let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("owner assembler should build");
    let foreign_value = foreign.values().text("foreign");

    assert!(assembler.evaluator().eval(&foreign_value).is_err());
    assert!(
        assembler
            .values()
            .apply(&assembler.values().integer(0), [foreign_value.clone()])
            .is_err()
    );
    assert!(
        assembler
            .values()
            .access_names(&foreign_value, ["member"])
            .is_err()
    );
    assert!(
        assembler
            .values()
            .anno_binary(foreign_value.clone())
            .is_err()
    );
    assert!(assembler.values().list_slice(&foreign_value, 0..1).is_err());
    assert!(access_path(&assembler, &foreign_value, "member").is_err());
    assert!(assembler.create_volume(foreign_value.clone()).is_err());
    assert!(
        assembler
            .net(|builder| builder.data(foreign_value.clone()))
            .is_err()
    );
    assert!(
        assembler
            .module(["foreign_initial_definitions"])
            .initial_definitions(foreign.values().empty_dict())
            .build()
            .is_err()
    );

    let (promise, resolver) = assembler.promise("foreign assignment");
    assert!(resolver.resolve(foreign_value.clone()).is_err());
    let CoreValue::Promised(unassigned) = promise.clone_core_for_test() else {
        panic!("public promise should retain its core promise cell")
    };
    assert!(
        unassigned.assignment().is_none(),
        "rejecting a foreign value must not terminalize the promise"
    );
    assert!(
        assembler
            .evaluate(&promise)
            .expect_err("a rejected foreign resolution must leave the promise pending")
            .to_string()
            .contains("before initialization")
    );
    let (failed, resolver) = assembler.promise("foreign failure");
    assert!(resolver.fail(foreign_value).is_err());
    let CoreValue::Promised(unassigned) = failed.clone_core_for_test() else {
        panic!("public promise should retain its core promise cell")
    };
    assert!(
        unassigned.assignment().is_none(),
        "rejecting a foreign failure must not terminalize the promise"
    );
}

#[test]
fn runtime_event_boundaries_reject_foreign_converted_and_output_values() {
    let runtime = EvaluationRuntime::new(0).expect("owner runtime should build");
    let foreign = EvaluationRuntime::new(0).expect("foreign runtime should build");
    let foreign_values = foreign.values();
    let input = runtime
        .input_endpoint(move |_: ()| Ok(foreign_values.integer(1)))
        .expect("input endpoint should register");
    assert!(input.sender().admit(()).is_err());

    let output = runtime
        .output_endpoint(decode_test_integer(runtime.values()), |_: i64| Ok(()))
        .expect("output endpoint should register");
    let (_, mut events) = input_transaction(&runtime);
    assert!(
        events
            .write(&output.writer(), foreign.values().integer(2))
            .is_err()
    );
    assert!(!runtime.has_delivery_activity());
}

#[test]
fn public_error_contexts_prepend_without_rewriting_the_message() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let inner = values.record([("inner", values.text("first"))]).unwrap();
    let outer = values.record([("outer", values.text("second"))]).unwrap();

    let error = Error::new("original")
        .with_context(&assembler.values(), inner.clone())
        .unwrap()
        .with_context(&assembler.values(), outer.clone())
        .unwrap();

    assert_eq!(error.to_string(), "original");
    assert_eq!(
        diagnostic_contexts(&assembler, &error.diagnostic(&values).unwrap()),
        [
            values.clone_core(&outer).unwrap(),
            values.clone_core(&inner).unwrap()
        ]
    );
}

#[test]
fn binary_annotation_preserves_a_nested_failure_context() {
    let assembler = Assembler::new();
    let module = assembler
        .module(["binary_context"])
        .script(
            "g",
            concat!(
                "language g0\n",
                "import 'std\n",
                "result = anno 'error {msg:{text:\"original\"}, detail:\"kept\"}\n",
            ),
        )
        .build()
        .expect("binary context fixture should compile");

    let error = binary_at(&assembler, module.value(), "result")
        .expect_err("binary observation should demand the failed definition");

    assert_eq!(error.to_string(), "original");
    let contexts = diagnostic_contexts(&assembler, &error.diagnostic(&assembler.values()).unwrap());
    assert!(
        contexts.first().and_then(definition_context).is_some(),
        "semantic binary conversion should preserve the target's source context without a host-only frame"
    );
    let detail = assembler
        .get(
            error.diagnostic(&assembler.values()).unwrap().emission(),
            "detail",
        )
        .expect("ad hoc diagnostic fields should survive contextualization");
    assert_eq!(assembler.to_binary(&detail).unwrap(), b"kept".as_slice());
}

#[test]
fn callers_can_attach_path_context_to_semantic_access() {
    let assembler = Assembler::new();
    let values = assembler.values();
    let root = public_value(
        &assembler.core_values(),
        CoreValue::Dict(Dict::new_sync().insert(
            Key::atom_from_text("broken"),
            CoreValue::error(&assembler.core_values(), "path target failed"),
        )),
    );

    let frame = values
        .record([(
            "eval",
            values
                .record([
                    ("op", values.atom_from_text("path_lookup")),
                    (
                        "args",
                        values
                            .record([("path", values.text("broken.member"))])
                            .unwrap(),
                    ),
                ])
                .unwrap(),
        )])
        .unwrap();
    let candidate = values
        .access_names(&root, ["broken", "member"])
        .and_then(|candidate| {
            values.anno(
                values.record([("context", frame.clone())]).unwrap(),
                candidate,
            )
        })
        .unwrap();
    let error = assembler
        .evaluator()
        .eval(&candidate)
        .expect_err("forcing an intermediate path value should fail");
    assert_eq!(error.to_string(), "path target failed");
    assert_eq!(
        diagnostic_contexts(&assembler, &error.diagnostic(&assembler.values()).unwrap()),
        [values.clone_core(&frame).unwrap()]
    );

    let missing = values
        .access_names(&root, ["missing", "member"])
        .expect("an absent path remains an ordinary semantic access");
    assert!(value_is_undefined(&assembler, &missing));
}

#[test]
fn semantic_binary_conversion_preserves_structured_failures() {
    let assembler = Assembler::new();
    let missing = binary_at(&assembler, &assembler.values().empty_dict(), "missing")
        .expect_err("missing binary path should fail");
    assert!(missing.to_string().contains("requires a list or binary"));
    assert!(missing.structured_diagnostic().is_some());

    let invalid = assembler
        .to_binary(&assembler.values().integer(42))
        .expect_err("a number is not binary text data");
    assert!(invalid.to_string().contains("requires a list or binary"));
    assert!(invalid.structured_diagnostic().is_some());

    let invalid_item = assembler
        .to_binary(
            &assembler
                .values()
                .list([assembler.values().integer(256)])
                .expect("invalid byte fixture should still be a list"),
        )
        .expect_err("an out-of-range list member is not binary text data");
    assert!(
        invalid_item
            .to_string()
            .contains("cannot encode number `256`")
    );
    assert!(invalid_item.structured_diagnostic().is_some());
}

#[test]
fn reflection_environment_explicitly_projects_compilation_origins() {
    let assembler = Assembler::new();
    let trace = test_compilation_trace("/workspace/source.g");
    let origin = crate::diagnostic::opaque_compilation_origin(&assembler.core_values(), &trace);
    assert_eq!(
        assembler
            .reflection()
            .kind(&public_value(&assembler.core_values(), origin.clone()))
            .unwrap(),
        ValueKind::Opaque
    );

    let inspect = assembler
        .get(&assembler.reflection_environment(), "glam.origin.inspect")
        .expect("the reflection environment should expose origin inspection");
    let projected = assembler
        .apply(&inspect, [public_value(&assembler.core_values(), origin)])
        .and_then(|value| assembler.evaluate(&value))
        .expect("the origin capability should inspect compilation origins");

    assert_eq!(projected.clone_core_for_test(), trace.origin_value());
}

#[test]
fn public_values_describe_metadata_carriers_only_as_sealed() {
    let assembler = Assembler::new();
    let value = public_value(
        &assembler.core_values(),
        CoreValue::metadata_carrier(CoreValue::binary_from_text("private trace")),
    );

    assert_eq!(
        assembler.reflection().kind(&value).unwrap(),
        ValueKind::Sealed
    );
    assert_eq!(format!("{value:?}"), "Value");
}

#[test]
fn origin_inspection_rejects_unrelated_opaque_values() {
    let assembler = Assembler::new();
    let inspect = assembler
        .get(&assembler.reflection_environment(), "glam.origin.inspect")
        .expect("the reflection environment should expose origin inspection");
    let unrelated = public_value(
        &assembler.core_values(),
        CoreValue::Opaque(OpaqueValue::new(&assembler.core_values(), Arc::new(42_u64))),
    );

    let error = assembler
        .apply(&inspect, [unrelated])
        .and_then(|value| assembler.evaluate(&value))
        .expect_err("unrelated opaque values must not be disclosed");
    assert!(
        error
            .to_string()
            .contains("origin inspection requires an opaque compilation origin"),
        "{error}"
    );
}

#[test]
fn source_definitions_add_shallow_opaque_origin_context() {
    let assembler = Assembler::new();
    let module = assembler
        .module(["definition_context"])
        .script(
            "g",
            concat!(
                "language g0\n",
                "import 'std\n",
                "broken = 1 / 0\n",
                "later x = x / 0\n",
                "object container with\n",
                "  broken = 1 / 0\n",
                "manual = anno context:{manual:module_origin} (1 / 0)\n",
            ),
        )
        .build()
        .expect("definition context fixture should compile");

    let broken =
        access_path(&assembler, module.value(), "broken").expect("fixture should define broken");
    let error = eval::eval_value(&assembler.eval_context(), &broken.clone_core_for_test())
        .expect_err("the broken definition should fail");
    let failure = error.into_permanent_failure();
    let context = failure
        .contexts()
        .iter()
        .find_map(definition_context)
        .expect("definition initialization should carry source context");
    assert_eq!(
        context.get(&*keys::DEFINITION),
        Some(&CoreValue::binary_from_text("broken"))
    );
    assert_eq!(
        context.get(&*keys::LINE),
        Some(&CoreValue::Number(Number::from_usize(3)))
    );
    let automatic_origin = context
        .get(&*keys::ORIGIN)
        .expect("source context should contain an origin")
        .clone();
    assert!(
        matches!(&automatic_origin, CoreValue::Opaque(_)),
        "source origins should remain opaque until a reflection capability inspects them"
    );

    let later =
        access_path(&assembler, module.value(), "later").expect("fixture should define later");
    let call = assembler
        .apply(&later, [assembler.values().integer(1)])
        .expect("calling a source function should remain lazy");
    let error = eval::eval_value(&assembler.eval_context(), &call.clone_core_for_test())
        .expect_err("the function body should fail when called");
    let failure = error.into_permanent_failure();
    assert!(
        failure
            .contexts()
            .iter()
            .all(|context| definition_context(context).is_none()),
        "shallow definition context must not capture arguments or follow later calls"
    );

    let object_member = access_path(&assembler, module.value(), "container.broken")
        .expect("fixture should define the nested object member");
    let error = eval::eval_value(
        &assembler.eval_context(),
        &object_member.clone_core_for_test(),
    )
    .expect_err("the nested object member should fail");
    let failure = error.into_permanent_failure();
    let context = failure
        .contexts()
        .iter()
        .find_map(definition_context)
        .expect("object member initialization should carry source context");
    assert_eq!(
        context.get(&*keys::DEFINITION),
        Some(&CoreValue::binary_from_text("broken"))
    );
    assert_eq!(
        context.get(&*keys::LINE),
        Some(&CoreValue::Number(Number::from_usize(6)))
    );

    let manual = access_path(&assembler, module.value(), "manual")
        .expect("fixture should define a manual context");
    let error = eval::eval_value(&assembler.eval_context(), &manual.clone_core_for_test())
        .expect_err("the manually contextualized expression should fail");
    let failure = error.into_permanent_failure();
    let manual_origin = failure.contexts().iter().find_map(|frame| {
        let CoreValue::Dict(frame) = eval::eval_value(&assembler.eval_context(), frame).ok()?
        else {
            return None;
        };
        frame.get(&Key::atom_from_text("manual")).cloned()
    });
    assert_eq!(
        manual_origin.as_ref(),
        Some(&automatic_origin),
        "module_origin should expose the same opaque token used by automatic frames; contexts: {:?}",
        failure.contexts()
    );
}

#[test]
fn assembler_clones_share_one_evaluation_session() {
    let assembler = Assembler::new();
    let clone = assembler.clone();

    assert!(
        assembler
            .eval_context()
            .shares_session_with(&clone.eval_context())
    );
    assert!(
        !assembler
            .eval_context()
            .shares_session_with(&Assembler::new().eval_context())
    );
}

#[test]
fn builder_seals_the_environment_into_one_reasoning_session() {
    let assembler = Assembler::builder()
        .reflection_environment(|environment| {
            let values = environment.values();
            values.record([("client", values.text("new environment"))])
        })
        .expect("configured environment should be valid");
    let assembler = assembler.build().expect("assembler should build");

    let client = assembler
        .get(&assembler.reflection_environment(), "client")
        .expect("configured environment should be installed");
    assert_eq!(
        assembler.to_binary(&client).unwrap(),
        b"new environment".as_slice()
    );
}

#[test]
fn builder_environment_promise_can_resolve_after_early_observation() {
    let mut resolver = None;
    let assembler = Assembler::builder()
        .reflection_environment(|environment| {
            let (value, promise_resolver) = environment.promise("late environment value");
            resolver = Some(promise_resolver);
            environment.values().record([("late", value)])
        })
        .expect("environment should build")
        .build()
        .expect("assembler should build");
    let promised = access_path(&assembler, &assembler.reflection_environment(), "late")
        .expect("promise should be present");

    assert!(assembler.evaluate(&promised).is_err());
    resolver
        .take()
        .expect("resolver should escape the builder")
        .resolve(assembler.values().text("ready"))
        .expect("promise should resolve once");
    let resolved = assembler
        .evaluate(&promised)
        .expect("resolved promise should evaluate");
    assert_eq!(assembler.to_binary(&resolved).unwrap(), b"ready".as_slice());
}

#[test]
fn dropped_builder_environment_resolver_fails_its_promise() {
    let assembler = Assembler::builder()
        .reflection_environment(|environment| {
            let (value, resolver) = environment.promise("abandoned environment value");
            drop(resolver);
            environment.values().record([("abandoned", value)])
        })
        .expect("environment should build")
        .build()
        .expect("assembler should build");
    let promised = access_path(&assembler, &assembler.reflection_environment(), "abandoned")
        .expect("promise should be present");

    assert!(
        assembler
            .evaluate(&promised)
            .expect_err("dropped resolver must fail its promise")
            .to_string()
            .contains("was dropped before completion")
    );
}

#[test]
fn builder_environment_promise_does_not_complete_through_self_dependency() {
    let mut resolver = None;
    let assembler = Assembler::builder()
        .reflection_environment(|environment| {
            let (value, promise_resolver) = environment.promise("self-dependent value");
            resolver = Some(promise_resolver);
            environment.values().record([("self", value)])
        })
        .expect("environment should build")
        .build()
        .expect("assembler should build");
    let promised = access_path(&assembler, &assembler.reflection_environment(), "self")
        .expect("promise should be present");
    resolver
        .take()
        .expect("resolver should escape the builder")
        .resolve(promised.clone())
        .expect("the host may assign a self-dependent value");

    let error = assembler
        .evaluate(&promised)
        .expect_err("self dependency cannot reach weak head normal form");
    assert!(
        error.to_string().contains("blocked on wait token"),
        "{error}"
    );
}

#[test]
fn synchronous_assembler_evaluation_waits_for_a_worker_claim() {
    let runtime = EvaluationRuntime::new(1).expect("worker runtime should build");
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime)
        .build()
        .expect("assembler should build");
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let producer_release = release.clone();
    let (started_sender, started_receiver) = std::sync::mpsc::channel();
    let lazy = crate::core::LazyValue::semantic_thunk(
        &assembler.core_values(),
        "worker-claimed public value",
        move |_| {
            started_sender
                .send(())
                .expect("test should still await the worker claim");
            let (lock, changed) = &*producer_release;
            let mut released = lock.lock().expect("test release lock was poisoned");
            while !*released {
                released = changed
                    .wait(released)
                    .expect("test release lock was poisoned");
            }
            Ok(CoreValue::Number(42.into()))
        },
    );
    let value = public_value(&assembler.core_values(), CoreValue::Lazy(lazy));
    assembler.eval_context().spark(value.clone_core_for_test());
    started_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker should claim the sparked value");

    let (result_sender, result_receiver) = std::sync::mpsc::channel();
    let evaluator = std::thread::spawn({
        let assembler = assembler.clone();
        let value = value.clone();
        move || {
            result_sender
                .send(assembler.evaluate(&value))
                .expect("test should still await the result");
        }
    });
    assert!(
        result_receiver
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err(),
        "synchronous evaluation must wait while the worker owns the value"
    );

    let (lock, changed) = &*release;
    *lock.lock().expect("test release lock was poisoned") = true;
    changed.notify_all();
    let result = result_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker completion should wake synchronous evaluation")
        .expect("worker-computed value should succeed");
    assert!(same_representation(
        &assembler,
        &result,
        &assembler.values().number_from_text("42").unwrap()
    ));
    evaluator.join().expect("evaluator thread should finish");
}

#[test]
fn builder_fixes_conflict_analysis_before_reasoning_starts() {
    let assembler = Assembler::builder()
        .conflict_analysis(Arc::new(crate::reflection::CoarseConflictAnalysis))
        .build()
        .expect("assembler should build");

    assert_eq!(assembler.conflict_analysis().name(), "coarse");
}

#[test]
fn attached_runtime_conflict_analysis_cannot_be_replaced() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let result = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .conflict_analysis(Arc::new(crate::reflection::CoarseConflictAnalysis))
        .build();
    let Err(error) = result else {
        panic!("an attached runtime must retain its conflict policy")
    };

    assert!(error.to_string().contains("already owns"));
    assert_eq!(runtime.conflict_analysis().name(), "exact");
}

#[test]
fn attached_runtime_default_reflection_profile_cannot_be_replaced() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let error = runtime
        .new_evaluation_session()
        .expect_err("an unsealed runtime must not expose a runnable session");
    assert!(error.to_string().contains("must be sealed"));
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("first assembler should seal the runtime profile");
    let replacement = Arc::new(AssemblerReflectionHost::new_unsealed(
        &runtime,
        DiagnosticBus::new(),
    ));
    replacement
        .seal_environment(
            authoritative_reflection_environment(
                &runtime.values(),
                runtime.values().empty_dict(),
                "replacement",
            )
            .unwrap()
            .0,
        )
        .unwrap();

    let error = runtime
        .seal_default_reflection_profile(coordinator_task_launcher(ReflectionEffects, replacement))
        .expect_err("a sealed runtime profile must reject replacement");
    assert!(error.to_string().contains("already sealed"));

    let runtime_state = Arc::downgrade(&runtime.state);
    drop(assembler);
    drop(runtime);
    assert!(
        runtime_state.upgrade().is_none(),
        "the sealed launcher must not form an Arc cycle through its host"
    );
}

#[test]
fn retained_reflection_profile_keeps_only_shared_resources_alive() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let state = Arc::downgrade(&runtime.state);
    let coordinator = Arc::downgrade(&runtime.state.work);
    let executor = Arc::downgrade(&runtime.state.executor);
    let default_profile = Arc::downgrade(&runtime.default_reflection_profile);
    let resources = Arc::downgrade(&runtime.state.shared_resources);
    let value_domain = Arc::downgrade(runtime.state.shared_resources.values.core().value_domain());
    let host = Arc::new(AssemblerReflectionHost::new_unsealed(
        &runtime,
        DiagnosticBus::for_runtime(&runtime),
    ));
    host.seal_environment(
        authoritative_reflection_environment(
            &runtime.values(),
            runtime.values().empty_dict(),
            "retained",
        )
        .unwrap()
        .0,
    )
    .unwrap();
    let profile = Arc::new(ReflectionTaskProfile::sealed(coordinator_task_launcher(
        ReflectionEffects,
        host.clone(),
    )));
    drop(host);
    drop(runtime);

    assert!(state.upgrade().is_none());
    assert!(coordinator.upgrade().is_none());
    assert!(executor.upgrade().is_none());
    assert!(default_profile.upgrade().is_none());

    let retained = resources
        .upgrade()
        .expect("the retained profile host should keep runtime resources alive");
    assert!(value_domain.upgrade().is_some());
    let (_, snapshot) = retained.reflection_snapshot();
    assert_eq!(
        retained.values().clone_core(snapshot.root()).unwrap(),
        retained
            .values()
            .clone_core(&retained.values().empty_dict())
            .unwrap()
    );
    let initial = retained.values().empty_dict();
    let volume = retained
        .create_volume(initial.clone())
        .expect("retained resources should still create volumes");
    assert_eq!(
        retained
            .values()
            .clone_core(&retained.revoke_volume(volume).unwrap())
            .unwrap(),
        retained.values().clone_core(&initial).unwrap()
    );
    drop(snapshot);
    drop(retained);

    drop(profile);
    assert!(resources.upgrade().is_none());
    assert!(value_domain.upgrade().is_none());
}

#[test]
fn evaluation_context_retains_runtime_cache_and_profile_without_a_cycle() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let state = Arc::downgrade(&runtime.state);
    let resources = Arc::downgrade(&runtime.state.shared_resources);
    let profile = Arc::downgrade(&runtime.default_reflection_profile);
    let value_domain = Arc::downgrade(runtime.state.shared_resources.values.core().value_domain());
    let assembler = Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .expect("assembler should seal the runtime profile");
    let context = assembler.eval_context();
    let unit = context.values().unit();

    drop(assembler);
    drop(runtime);
    assert!(state.upgrade().is_none());
    assert!(resources.upgrade().is_some());
    assert!(profile.upgrade().is_some());
    assert!(value_domain.upgrade().is_some());
    assert_eq!(eval::eval_value(&context, &unit).unwrap(), unit);

    drop(context);
    assert!(resources.upgrade().is_none());
    assert!(profile.upgrade().is_none());
    assert!(value_domain.upgrade().is_none());
}

#[test]
fn compiler_cache_does_not_form_a_value_domain_cycle() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let values = runtime.state.shared_resources.values.core();
    let value_domain = Arc::downgrade(values.value_domain());

    crate::g_syntax::initialize_cached_compiler_values(values);
    assert!(matches!(
        crate::g_syntax::default_diagnostic_formatter(values),
        CoreValue::Function(_)
    ));

    drop(runtime);
    assert!(value_domain.upgrade().is_none());
}

#[test]
fn built_module_retains_its_published_value_after_construction_scope_exits() {
    let assembler = Assembler::new();
    let domain = EffectTokenDomain::new(&assembler.values());
    let payload = Arc::new(());
    let retained = Arc::downgrade(&payload);
    let module = assembler
        .module(["published_root"])
        .initial_definitions(domain.issue(payload))
        .build()
        .expect("an already-closed value should publish as a module result");

    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(retained.upgrade().is_some());
    drop(module);
    domain.collect_and_drain_retired_external_owners_for_test();
    assert!(
        retained.upgrade().is_none(),
        "the published module value must retire with its last public root"
    );
}

#[test]
fn closed_runtime_cache_builders_do_not_register_scheduler_demand() {
    let runtime = EvaluationRuntime::new(0).expect("runtime should build");
    let before = runtime.state.work.cache_builder_scheduler_snapshot();
    let values = runtime.state.shared_resources.values.core();

    crate::g_syntax::initialize_cached_compiler_values(values);
    let formatter = crate::g_syntax::default_diagnostic_formatter(values);

    assert!(matches!(formatter, CoreValue::Function(_)));
    assert_eq!(
        runtime.state.work.cache_builder_scheduler_snapshot(),
        before,
        "closed cache construction must not alter runtime work generation, demand registrations, or work records"
    );
}

#[test]
fn builder_selects_runtime_before_exposing_runtime_bound_state() {
    let mut builder = Assembler::builder();
    let initial = builder.runtime.values().empty_dict();
    let _volume = builder
        .create_volume(initial)
        .expect("the initial runtime should create the volume");
    let replacement = EvaluationRuntime::new(0).expect("replacement runtime should build");
    let result = builder.evaluation_runtime(replacement).build();
    let Err(error) = result else {
        panic!("runtime replacement after state construction must be rejected")
    };

    assert!(error.to_string().contains("must be selected before"));
}

#[test]
fn reflection_annotations_launch_tasks_and_return_their_targets() {
    let assembler = Assembler::new();
    let module = assembler
        .module(["annotation_test"])
        .script(
            "g",
            "language g0\nimport 'std\neffect = .r ()\nresult = anno { refl:effect } \"ready\"\n",
        )
        .build()
        .expect("reflection annotation fixture should compile");
    let result =
        access_path(&assembler, module.value(), "result").expect("fixture should define result");

    assert_eq!(
        assembler
            .to_binary(&assembler.evaluate(&result).unwrap())
            .unwrap(),
        b"ready".as_slice()
    );
}

#[test]
fn reflection_annotations_require_their_tasks_to_return_unit() {
    let assembler = Assembler::new();
    let module = assembler
            .module(["annotation_test"])
            .script(
                "g",
                "language g0\nimport 'std\neffect = .r \"not unit\"\nresult = anno { refl:effect } \"unreachable\"\n",
            )
            .build()
            .expect("reflection annotation fixture should compile");
    let result =
        access_path(&assembler, module.value(), "result").expect("fixture should define result");

    assert!(
        assembler
            .to_binary(&result)
            .unwrap_err()
            .to_string()
            .contains("reflection annotation result: unit expected, received Binary")
    );
}

#[test]
fn reflection_annotation_logs_use_the_assembler_diagnostic_bus() {
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let received = diagnostics.clone();
    let assembler = Assembler::new().with_diagnostic_callback(move |diagnostic| {
        received
            .lock()
            .expect("diagnostic collection mutex should not be poisoned")
            .push(diagnostic);
    });
    let module = assembler
            .module(["annotation_test"])
            .script(
                "g",
                "language g0\nimport 'std\neffect = .log 'warn { msg:{ text:\"from annotation\" } }\nresult = anno { refl:effect } \"ready\"\n",
            )
            .build()
            .expect("reflection annotation fixture should compile");
    let result = assembler
        .get(module.value(), "result")
        .expect("fixture should define result");

    assert_eq!(
        assembler
            .to_binary(&result)
            .expect("logging annotation should complete"),
        b"ready".as_slice()
    );
    let diagnostics = diagnostics
        .lock()
        .expect("diagnostic collection mutex should not be poisoned");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity(), Severity::Warning);
    assert_eq!(diagnostics[0].message(), "from annotation");
}

#[test]
fn failed_reflection_branch_does_not_publish_its_diagnostic() {
    let assembler = Assembler::new();
    let module = assembler
            .module(["annotation_test"])
            .script(
                "g",
                "language g0\nimport 'std\neffect = .cut (.alt ((.log 'error { msg:{ text:\"discarded\" } }) =>> .fail) (.r ()))\nresult = anno { refl:effect } \"ready\"\n",
            )
            .build()
            .expect("reflection annotation fixture should compile");

    assert_eq!(
        binary_at(&assembler, module.value(), "result")
            .expect("winning reflection branch should complete"),
        b"ready".as_slice()
    );
    assert_eq!(assembler.diagnostic_bus().counts().total(), 0);
}
