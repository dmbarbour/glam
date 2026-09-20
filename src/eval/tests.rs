use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::core::{
    Dict, EvaluatedValue, EvaluationFailure, FixpointComputation, Key, LazyValue, ListThunk, Value,
    keys,
};
use crate::core_net::CoreRuntimeNet;
use crate::evaluation::{
    EvaluationMachinePoll, EvaluationPumpOutcome, EvaluationTaskMachine, EvaluationWaitPoll,
    ReflectionTaskLauncher, ReflectionTaskResultPolicy,
};
use crate::number::Number;

use super::*;

fn set_promise(context: &EvalContext, promise: &PromisedValue, value: Value) -> Result<(), Value> {
    crate::core::set_test_promise(context.values(), promise, value)
}

fn fail_promise(
    context: &EvalContext,
    promise: &PromisedValue,
    failure: Arc<EvaluationFailure>,
) -> Result<(), Arc<EvaluationFailure>> {
    crate::core::fail_test_promise(context.values(), promise, failure)
}

fn fail_promise_message(
    context: &EvalContext,
    promise: &PromisedValue,
    message: impl Into<Arc<str>>,
) -> Result<(), Arc<EvaluationFailure>> {
    crate::core::fail_test_promise_message(context.values(), promise, message)
}

fn unit_value() -> Value {
    crate::core::test_value_factory().unit()
}

fn initial_metadata() -> Value {
    Value::initial_metadata_carrier(&crate::core::test_value_factory())
}

fn closed_net(build: impl FnOnce(&mut NetBuilder<CoreSpecialization>) -> Port) -> NetValue {
    closed_net_in(&crate::core::test_value_factory(), build)
}

fn closed_net_in(
    values: &CoreValueFactory,
    build: impl FnOnce(&mut NetBuilder<CoreSpecialization>) -> Port,
) -> NetValue {
    let mut builder = NetBuilder::new();
    let exposed = build(&mut builder);
    let template = builder.finish(exposed);
    NetValue::new(values.instantiate_core_net(&template))
}

fn fixture_computation(expr: TestExpr) -> Value {
    lower_test_computation_value(expr)
}

fn apply_test_values(function: Value, arguments: impl IntoIterator<Item = Value>) -> Value {
    apply_values(&test_context(), function, arguments.into_iter().collect())
        .expect("test application should accept a callable value")
}

fn cached_value(lazy: &LazyValue) -> Value {
    lazy.cached(&crate::core::test_value_factory())
        .expect("lazy value should be cached")
        .expect("lazy value should succeed")
        .into_value()
}

fn list_return_effect(value: Value) -> Value {
    test_effect_value(closed_function_value(
        1,
        TestExpr::Apply(
            Arc::new(TestExpr::Access(
                Arc::new(TestExpr::Local(0)),
                Arc::from([TestKey::Key((*keys::R).clone())]),
            )),
            Arc::new(TestExpr::Value(value)),
        ),
    ))
}

fn test_effect_value(function: Value) -> Value {
    crate::core::test_value_factory()
        .with_runtime_value_access(|access| effect_value(&access, function))
}

fn wrapper_returning_function_computation(context: &EvalContext) -> LazyValue {
    let returned_code = Arc::new(lower_test_function_code_in(
        context.values(),
        1,
        TestExpr::Local(0),
    ));
    let make = closed_function_value_in(
        context.values(),
        1,
        TestExpr::Function {
            code: returned_code,
            captures: Arc::from([]),
        },
    );
    let wrapper = closed_function_value_in(
        context.values(),
        1,
        TestExpr::Apply(
            Arc::new(TestExpr::Value(make)),
            Arc::new(TestExpr::Local(0)),
        ),
    );
    let expression = TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Value(wrapper)),
            Arc::new(TestExpr::Value(unit_value())),
        )),
        Arc::new(TestExpr::Value(n(42))),
    );

    let code = lower_test_function_code_in(context.values(), 0, expression);
    LazyValue::from_net_computation(
        context.values(),
        NetValue::new(code.runtime().duplicate_for_test(context.values())),
    )
}

fn isolated_test_context() -> OwnedEvalContext {
    EvalContext::isolated(CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    ))
}

fn net_computation_runtime(lazy: &LazyValue, context: &EvalContext) -> CoreRuntimeNet {
    let Some(crate::core::LazySource::NetComputation(net)) = lazy.source_snapshot(context.values())
    else {
        panic!("the W4E fixture must retain its net-computation source");
    };
    net.into_runtime()
}

#[test]
fn wrapper_returning_function_then_accepts_remaining_application() {
    let context = isolated_test_context();
    let computation_lazy = wrapper_returning_function_computation(&context);
    let computation_runtime = net_computation_runtime(&computation_lazy, &context);
    let computation = Value::Lazy(computation_lazy.clone());

    #[cfg(feature = "interaction-net-profiling")]
    context.values().set_net_driver_work_item_limit(128);

    let demand = context
        .demand_whnf(crate::runtime::RuntimeValueRoot::new(
            context.values(),
            computation.clone(),
        ))
        .expect("bounded wrapper demand should be admitted");
    let mut machine_polls = 0;
    while demand.poll().is_none() {
        assert!(
            machine_polls < 128,
            "wrapper demand exceeded its deterministic machine-poll budget"
        );
        assert!(
            context.poll_one_runtime_work_for_test(),
            "wrapper demand retained no runnable producer"
        );
        machine_polls += 1;
        #[cfg(feature = "interaction-net-profiling")]
        assert!(
            !context.values().net_driver_work_item_limit_reached(),
            "wrapper demand exceeded its deterministic net-work budget: {:?}",
            context.values().interaction_net_profile_snapshot()
        );
    }

    assert_eq!(eval_value(&context, &computation).unwrap(), n(42));
    assert_eq!(context.client_demand_count_for_test(), 0);
    assert!(computation_lazy.source_snapshot(context.values()).is_none());
    assert!(computation_lazy.cached(context.values()).is_some());
    assert_eq!(
        computation_runtime.active_normalization_batch(context.values()),
        None
    );
    computation_runtime.test_with(context.values(), |net| {
        assert!(!net.has_in_flight_claims());
    });
    #[cfg(feature = "interaction-net-profiling")]
    {
        use crate::interaction_net::profiling::{NetDriverCounts, NetReductionCounts};

        let profile = context.values().interaction_net_profile_snapshot();
        assert_eq!(machine_polls, 17);
        assert_eq!(
            profile.reductions,
            NetReductionCounts {
                bind_join: 9,
                fan_join: 0,
                fan_commute: 0,
                fan_data: 0,
                fan_bind: 0,
                fan_operator: 0,
                erase: 0,
                call: 3,
                operator_call: 6,
                cursor_materialized: 6,
                cursor_joined: 2,
            },
            "over-application through a returned function must not replay semantic work"
        );
        assert_eq!(
            profile.driver,
            NetDriverCounts {
                machine_polls: 4,
                work_items: 71,
                interface_polls: 27,
                cursor_steps: 11,
                active_pair_steps: 27,
                cursor_dependencies: 6,
                blocked_retries: 0,
                contentions: 0,
                disturbances: 0,
                request_root_restarts: 0,
                ..NetDriverCounts::default()
            }
        );
    }
}

#[cfg(feature = "interaction-net-profiling")]
#[test]
fn wrapper_application_budget_probe_yields_without_publishing_a_cache() {
    let context = isolated_test_context();
    let computation_lazy = wrapper_returning_function_computation(&context);
    let computation_runtime = net_computation_runtime(&computation_lazy, &context);
    let computation = Value::Lazy(computation_lazy.clone());
    context.values().set_net_driver_work_item_limit(16);

    let demand = context
        .demand_whnf(crate::runtime::RuntimeValueRoot::new(
            context.values(),
            computation,
        ))
        .expect("bounded wrapper demand should be admitted");
    for _ in 0..128 {
        assert!(
            context.poll_one_runtime_work_for_test(),
            "budget probe retained no runnable producer"
        );
        if context.values().net_driver_work_item_limit_reached() {
            break;
        }
    }

    let profile = context.values().interaction_net_profile_snapshot();
    assert_eq!(profile.driver.work_items, 16);
    assert!(demand.poll().is_none());
    assert_eq!(context.client_demand_count_for_test(), 1);
    assert!(computation_lazy.source_snapshot(context.values()).is_none());
    context.values().with_runtime_value_access(|access| {
        let checkpoint = computation_lazy
            .access(&access)
            .checkpoint_snapshot()
            .expect("bounded net work must remain in its managed checkpoint");
        assert_eq!(
            checkpoint.kind(),
            crate::eval::lazy_checkpoint::ManagedLazyCheckpointKindTag::Whnf
        );
    });
    assert!(computation_lazy.cached(context.values()).is_none());
    assert_eq!(
        computation_runtime.active_normalization_batch(context.values()),
        None
    );
    computation_runtime.test_with(context.values(), |net| {
        assert!(!net.has_in_flight_claims());
    });

    demand.abandon();
    assert_eq!(context.client_demand_count_for_test(), 0);
}

#[test]
fn object_local_name_resumes_a_lazy_parts_tail_without_replaying_its_name() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (tail, _tail_task, _tail_owner) = owner
        .task_owned_promise(Arc::from("object local-name parts tail"))
        .expect("the owner should allocate a promised parts tail");
    let name_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&name_demands);
    let name = Value::semantic_thunk(observer.values(), "instrumented object name", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::binary_from_text("root"))
    });
    let host = Value::Dict(Dict::new_sync().insert(
        (*keys::SPEC).clone(),
        Value::Dict(Dict::new_sync().insert((*keys::NAME).clone(), name)),
    ));
    let parts = Value::List(List::concat(
        List::from_values(vec![n(1)]),
        List::from_thunk(tail.clone().into()),
    ));
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::ObjectLocalName),
        vec![host, parts],
    )
    .expect("object local-name application should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated object local-name builtin should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the lazy parts tail should suspend local-name construction");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(name_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the object local-name checkpoint must retain its completed prefix");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact lazy parts tail");
    assert_eq!(name_demands.load(Ordering::SeqCst), 1);
    set_promise(&owner, &tail, Value::List(List::from_values(vec![n(2)])))
        .expect("the parts tail should accept its assignment");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned parts tail must remain live beneath the checkpoint");

    let Value::List(name) =
        eval_value(&observer, &application).expect("local-name construction should resume")
    else {
        panic!("object local name should produce a list")
    };
    assert_eq!(
        list_to_value_items(&observer, &name).expect("the local name should be readable"),
        [Value::binary_from_text("root"), n(1), n(2)]
    );
    assert_eq!(name_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

#[test]
fn object_with_defs_resumes_a_promised_spec_without_replaying_its_object() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (spec, _spec_task, _spec_owner) = owner
        .task_owned_promise(Arc::from("object extension specification"))
        .expect("the owner should allocate a promised object specification");
    let object_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&object_demands);
    let object_spec = spec.clone();
    let object = Value::semantic_thunk(observer.values(), "instrumented object", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Dict(Dict::new_sync().insert(
            (*keys::SPEC).clone(),
            Value::Promised(object_spec.clone()),
        )))
    });
    let extension = closed_function_value_in(
        observer.values(),
        2,
        TestExpr::Value(Value::Dict(
            Dict::new_sync().insert(Key::binary_from_text("extended"), n(42)),
        )),
    );
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::ObjectWithDefs),
        vec![object, extension],
    )
    .expect("object extension should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated object extension builtin should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the promised specification should suspend object extension");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(object_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the object-extension checkpoint must retain its completed object demand");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact specification demand");
    assert_eq!(object_demands.load(Ordering::SeqCst), 1);
    let resolved_spec = Value::Dict(
        Dict::new_sync()
            .insert((*keys::NAME).clone(), Value::binary_from_text("root"))
            .insert((*keys::DEPS).clone(), Value::List(List::empty()))
            .insert(
                (*keys::DEFS).clone(),
                Value::Builtin(Builtin::ObjectDefaultDefs),
            ),
    );
    set_promise(&owner, &spec, resolved_spec)
        .expect("the object specification should accept its assignment");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned specification must remain live beneath the checkpoint");

    let Value::Dict(result) =
        eval_value(&observer, &application).expect("object extension should resume")
    else {
        panic!("object extension should produce an object dictionary")
    };
    assert_eq!(result.get(&Key::binary_from_text("extended")), Some(&n(42)));
    assert_eq!(object_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

#[test]
fn composed_object_defs_resume_the_extension_without_replaying_prior_defs() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (extension, _extension_task, _extension_owner) = owner
        .task_owned_promise(Arc::from("composed object extension"))
        .expect("the owner should allocate a promised extension");
    let prior_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&prior_demands);
    let prior = Value::semantic_thunk(
        observer.values(),
        "instrumented prior defs",
        move |context| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(closed_function_value_in(
                context.context().values(),
                2,
                TestExpr::Value(Value::Dict(
                    Dict::new_sync().insert(Key::binary_from_text("prior"), n(19)),
                )),
            ))
        },
    );
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::ObjectComposedDefs),
        vec![
            prior,
            Value::Promised(extension.clone()),
            Value::Dict(Dict::new_sync()),
            Value::Dict(Dict::new_sync()),
        ],
    )
    .expect("composed definitions should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated composed-definitions builtin should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the promised extension should suspend composition");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(prior_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the composed-definitions checkpoint must retain its completed prior stage");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact extension stage");
    assert_eq!(prior_demands.load(Ordering::SeqCst), 1);
    let extension_defs = closed_function_value_in(
        owner.values(),
        2,
        TestExpr::Value(Value::Dict(
            Dict::new_sync().insert(Key::binary_from_text("extended"), n(42)),
        )),
    );
    set_promise(&owner, &extension, extension_defs)
        .expect("the composed extension should accept its assignment");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned extension must remain live beneath the checkpoint");

    let Value::Dict(result) =
        eval_value(&observer, &application).expect("composed definitions should resume")
    else {
        panic!("composed definitions should produce a dictionary")
    };
    assert_eq!(result.get(&Key::binary_from_text("extended")), Some(&n(42)));
    assert_eq!(prior_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

#[test]
fn object_override_resumes_a_nested_prior_without_replaying_completed_prefix() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (nested_prior, _prior_task, _prior_owner) = owner
        .task_owned_promise(Arc::from("nested object override prior"))
        .expect("the owner should allocate a promised nested prior value");
    let update_demands = Arc::new(AtomicUsize::new(0));
    let observed_updates = Arc::clone(&update_demands);
    let updates =
        Value::semantic_thunk(
            observer.values(),
            "instrumented object override updates",
            move |_| {
                observed_updates.fetch_add(1, Ordering::SeqCst);
                Ok(Value::Dict(
                    Dict::new_sync()
                        .insert(Key::binary_from_text("a_early"), n(42))
                        .insert(
                            Key::binary_from_text("z_nested"),
                            Value::Dict(Dict::new_sync().insert(
                                Key::binary_from_text("new"),
                                Value::binary_from_text("new"),
                            )),
                        ),
                ))
            },
        );
    let base_demands = Arc::new(AtomicUsize::new(0));
    let observed_base = Arc::clone(&base_demands);
    let nested_prior_value = nested_prior.clone();
    let base = Value::semantic_thunk(
        observer.values(),
        "instrumented object override base",
        move |_| {
            observed_base.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Dict(
                Dict::new_sync()
                    .insert(Key::binary_from_text("a_early"), n(19))
                    .insert(
                        Key::binary_from_text("z_nested"),
                        Value::Promised(nested_prior_value.clone()),
                    ),
            ))
        },
    );
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::ObjectOverrideDefs),
        vec![updates, base, unit_value()],
    )
    .expect("object override definitions should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated object-override builtin should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the nested promised prior should suspend object override");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(update_demands.load(Ordering::SeqCst), 1);
    assert_eq!(base_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the override checkpoint must retain its completed outer prefix and stack");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact nested prior demand");
    assert_eq!(update_demands.load(Ordering::SeqCst), 1);
    assert_eq!(base_demands.load(Ordering::SeqCst), 1);
    set_promise(
        &owner,
        &nested_prior,
        Value::Dict(
            Dict::new_sync().insert(Key::binary_from_text("old"), Value::binary_from_text("old")),
        ),
    )
    .expect("the nested prior should accept its assignment");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned nested prior must remain live beneath the override stack");

    let Value::Dict(result) =
        eval_value(&observer, &application).expect("object override should resume")
    else {
        panic!("object override should produce a dictionary")
    };
    assert_eq!(result.get(&Key::binary_from_text("a_early")), Some(&n(42)));
    let Some(Value::Dict(nested)) = result.get(&Key::binary_from_text("z_nested")) else {
        panic!("the nested object override should remain a dictionary")
    };
    assert_eq!(
        nested.get(&Key::binary_from_text("old")),
        Some(&Value::binary_from_text("old"))
    );
    assert_eq!(
        nested.get(&Key::binary_from_text("new")),
        Some(&Value::binary_from_text("new"))
    );
    assert_eq!(update_demands.load(Ordering::SeqCst), 1);
    assert_eq!(base_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

#[test]
fn object_instance_from_parts_builds_through_the_resumable_builtin_owner() {
    let context = test_context();
    let application = apply_values(
        &context,
        Value::Builtin(Builtin::ObjectInstanceFromParts),
        vec![
            Value::binary_from_text("root"),
            Value::List(List::empty()),
            Value::Builtin(Builtin::ObjectDefaultDefs),
        ],
    )
    .expect("parts-based object construction should build");

    let Value::Dict(object) =
        eval_value(&context, &application).expect("parts-based object construction should finish")
    else {
        panic!("parts-based object construction should produce a dictionary")
    };
    let Some(Value::Dict(spec)) = object.get(&*keys::SPEC) else {
        panic!("the constructed object should publish its specification")
    };
    assert_eq!(
        spec.get(&*keys::NAME),
        Some(&Value::binary_from_text("root"))
    );
}

#[test]
fn object_dict_defs_resume_the_dict_without_replaying_the_base() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (dict, _dict_task, _dict_owner) = owner
        .task_owned_promise(Arc::from("dictionary object definitions"))
        .expect("the owner should allocate a promised definitions dictionary");
    let base_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&base_demands);
    let base = Value::semantic_thunk(
        observer.values(),
        "instrumented object definitions base",
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Dict(
                Dict::new_sync().insert(Key::binary_from_text("base"), n(19)),
            ))
        },
    );
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::ObjectDictDefs),
        vec![Value::Promised(dict.clone()), base, unit_value()],
    )
    .expect("dictionary object definitions should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated object dictionary-definitions builtin should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("dictionary definitions should wait after demanding their base");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(base_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the dictionary-definitions checkpoint must retain its completed base");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact definitions dictionary");
    assert_eq!(base_demands.load(Ordering::SeqCst), 1);
    set_promise(
        &owner,
        &dict,
        Value::Dict(Dict::new_sync().insert(Key::binary_from_text("dict"), n(42))),
    )
    .expect("the definitions dictionary should accept its assignment");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned definitions dictionary must remain live beneath the checkpoint");

    let Value::Dict(result) =
        eval_value(&observer, &application).expect("dictionary definitions should resume")
    else {
        panic!("dictionary definitions should produce a dictionary")
    };
    assert_eq!(result.get(&Key::binary_from_text("base")), Some(&n(19)));
    assert_eq!(result.get(&Key::binary_from_text("dict")), Some(&n(42)));
    assert_eq!(base_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

#[test]
fn object_from_dict_resumes_a_promised_spec_without_replaying_its_dictionary() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (spec, _spec_task, _spec_owner) = owner
        .task_owned_promise(Arc::from("plain-dictionary specification"))
        .expect("the owner should allocate a promised specification");
    let dictionary_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&dictionary_demands);
    let promised_spec = spec.clone();
    let dictionary = Value::semantic_thunk(
        observer.values(),
        "instrumented plain dictionary",
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Dict(
                Dict::new_sync()
                    .insert(
                        (*keys::SPEC).clone(),
                        Value::Promised(promised_spec.clone()),
                    )
                    .insert(Key::binary_from_text("answer"), n(42)),
            ))
        },
    );
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::ObjectFromDict),
        vec![dictionary],
    )
    .expect("plain-dictionary conversion should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated object-from-dictionary builtin should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the promised specification should suspend plain-dictionary conversion");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(dictionary_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the object-from-dictionary checkpoint must retain the dictionary");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact dictionary specification");
    assert_eq!(dictionary_demands.load(Ordering::SeqCst), 1);
    set_promise(&owner, &spec, Value::Dict(Dict::new_sync()))
        .expect("the specification should accept its undefined assignment");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned object specification must remain live beneath the checkpoint");

    let Value::Dict(object) =
        eval_value(&observer, &application).expect("plain-dictionary conversion should resume")
    else {
        panic!("plain-dictionary conversion should produce an object dictionary")
    };
    assert_eq!(object.get(&Key::binary_from_text("answer")), Some(&n(42)));
    assert!(matches!(object.get(&*keys::SPEC), Some(Value::Dict(_))));
    assert_eq!(dictionary_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

struct DropSignal(Arc<AtomicBool>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
enum GateFailureStage {
    LauncherConstruction,
    TaskPoll,
}

struct GateFailureLauncher {
    failure: Arc<EvaluationFailure>,
    stage: GateFailureStage,
    builds: Arc<AtomicUsize>,
}

impl ReflectionTaskLauncher for GateFailureLauncher {
    fn build(
        &self,
        _context: EvalContext,
        _effect: Value,
        _result_policy: ReflectionTaskResultPolicy,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        match self.stage {
            GateFailureStage::LauncherConstruction => Err(self.failure.clone()),
            GateFailureStage::TaskPoll => Ok(Box::new(GateFailureMachine(self.failure.clone()))),
        }
    }
}

struct GateFailureMachine(Arc<EvaluationFailure>);

impl EvaluationTaskMachine for GateFailureMachine {
    fn poll(
        &mut self,
        context: &crate::evaluation::EvaluationPollContext,
        _step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        EvaluationMachinePoll::Failed(context.root_failure(self.0.clone()))
    }
}

#[derive(Clone)]
enum FixtureTaskTerminal {
    Complete(Value),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
}

struct FixtureTaskLauncher {
    terminal: FixtureTaskTerminal,
    builds: Arc<AtomicUsize>,
    result_policies: Arc<Mutex<Vec<ReflectionTaskResultPolicy>>>,
}

struct ScopedReflectionLauncher {
    values: crate::core::CoreValueFactory,
    builds: Arc<AtomicUsize>,
}

impl ReflectionTaskLauncher for ScopedReflectionLauncher {
    fn build(
        &self,
        _context: EvalContext,
        _effect: Value,
        _result_policy: ReflectionTaskResultPolicy,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>> {
        self.values
            .collect_managed_for_test()
            .expect("reflection launcher construction must run without a mutator");
        self.builds.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FixtureTaskMachine {
            terminal: Some(FixtureTaskTerminal::Complete(unit_value())),
        }))
    }
}

impl ReflectionTaskLauncher for FixtureTaskLauncher {
    fn build(
        &self,
        _context: EvalContext,
        _effect: Value,
        result_policy: ReflectionTaskResultPolicy,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        self.result_policies
            .lock()
            .expect("fixture result policies were poisoned")
            .push(result_policy);
        Ok(Box::new(FixtureTaskMachine {
            terminal: Some(self.terminal.clone()),
        }))
    }
}

struct FixtureTaskMachine {
    terminal: Option<FixtureTaskTerminal>,
}

impl EvaluationTaskMachine for FixtureTaskMachine {
    fn poll(
        &mut self,
        _context: &crate::evaluation::EvaluationPollContext,
        _step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        match self
            .terminal
            .take()
            .expect("a terminal fixture task must be polled only once")
        {
            FixtureTaskTerminal::Complete(value) => {
                EvaluationMachinePoll::Complete(_context.root_value(value))
            }
            FixtureTaskTerminal::Failed(error) => {
                EvaluationMachinePoll::Failed(_context.root_failure(error))
            }
            FixtureTaskTerminal::Cancelled => EvaluationMachinePoll::Cancelled,
        }
    }
}

#[test]
fn terminal_lazy_evaluation_releases_successful_and_failed_sources() {
    let context = test_context();
    let success_dropped = Arc::new(AtomicBool::new(false));
    let success_signal = DropSignal(success_dropped.clone());
    let success = LazyValue::semantic_thunk(
        &crate::core::test_value_factory(),
        "successful source release",
        move |_| {
            let _keep_signal_captured = &success_signal;
            Ok(unit_value())
        },
    );

    assert_eq!(
        eval_lazy(&context, &success).expect("lazy source should succeed"),
        unit_value()
    );
    assert!(success.source_snapshot(context.values()).is_none());
    assert!(
        success_dropped.load(Ordering::Acquire),
        "successful production should release its source captures"
    );

    let failure_dropped = Arc::new(AtomicBool::new(false));
    let failure_signal = DropSignal(failure_dropped.clone());
    let failure = LazyValue::semantic_thunk(
        &crate::core::test_value_factory(),
        "failed source release",
        move |_| {
            let _keep_signal_captured = &failure_signal;
            Err(EvaluationHalt::new("expected lazy failure"))
        },
    );

    let error = eval_lazy(&context, &failure).expect_err("lazy source should fail");
    assert_eq!(error.to_string(), "expected lazy failure");
    assert!(failure.source_snapshot(context.values()).is_none());
    assert!(
        failure_dropped.load(Ordering::Acquire),
        "failed production should release its source captures"
    );
}

#[test]
fn evaluation_context_frames_use_an_atom_operation_and_optional_named_arguments() {
    assert_eq!(
        evaluation_context_frame("list_index"),
        Value::Dict(Dict::new_sync().insert(
            (*keys::EVAL).clone(),
            Value::Dict(Dict::new_sync().insert(
                (*keys::OP).clone(),
                Key::atom_from_text("list_index").to_value_with(&crate::core::test_value_factory()),
            )),
        ))
    );

    let args = Dict::new_sync().insert(
        Key::atom_from_text("path"),
        Value::binary_from_text("conf.env"),
    );
    assert_eq!(
        evaluation_context_frame_with_args("path_lookup", args.clone()),
        Value::Dict(
            Dict::new_sync().insert(
                (*keys::EVAL).clone(),
                Value::Dict(
                    Dict::new_sync()
                        .insert(
                            (*keys::OP).clone(),
                            Key::atom_from_text("path_lookup")
                                .to_value_with(&crate::core::test_value_factory()),
                        )
                        .insert((*keys::ARGS).clone(), Value::Dict(args)),
                ),
            )
        )
    );
}

#[test]
fn immediate_diagnostic_shell_operations_share_one_root_neutral_access_region() {
    // Root-registration counts belong to one heap. Use a private value domain
    // so unrelated parallel tests using the shared fixture cannot perturb the
    // before/after probe.
    let context = EvalContext::isolated(CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    ));
    let values = context.values();
    let before = values.managed_root_registrations_for_test();
    let detail = Key::binary_from_text("detail");

    values.with_runtime_value_access(|access| {
        let frame = evaluation_context_frame_in(&access, "regional_diagnostic");
        let failure =
            EvaluationFailure::emission(Value::Dict(Dict::new_sync().insert(detail.clone(), n(7))))
                .with_context_in(&access, frame);
        let Value::Dict(diagnostic) = failure_diagnostic_value_in(&access, &failure) else {
            panic!("a dictionary emission should remain a diagnostic dictionary")
        };
        assert_eq!(diagnostic.get(&detail), Some(&n(7)));

        let Value::Dict(split) = split_result_value(&access, n(1), n(2)) else {
            panic!("a split result should be a dictionary")
        };
        assert_eq!(split.get(&*keys::LEFT), Some(&n(1)));
        assert_eq!(split.get(&*keys::RIGHT), Some(&n(2)));
        assert!(is_undefined_dict_value(
            &access,
            &Value::Dict(Dict::new_sync())
        ));
        assert!(!is_deferred_value(&access, &n(3)));
    });

    assert_eq!(
        values.managed_root_registrations_for_test(),
        before,
        "immediate shell projection must not register compatibility roots"
    );
}

#[test]
fn raw_net_values_are_opaque_while_net_computations_expose_data() {
    let net = closed_net(|builder| builder.data(n(42)));
    let raw = Value::Net(net.clone());

    assert_eq!(eval_value(&test_context(), &raw).unwrap(), raw);

    let computation = Value::Lazy(LazyValue::from_net_computation(
        &crate::core::test_value_factory(),
        net,
    ));
    assert_eq!(eval_value(&test_context(), &computation).unwrap(), n(42));
    assert_eq!(eval_value(&test_context(), &computation).unwrap(), n(42));
}

#[test]
fn net_arity_functions_attach_to_applications_through_cursors() {
    let identity = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    let expression = TestExpr::Apply(
        Arc::new(TestExpr::Value(apply_test_values(
            Value::Builtin(Builtin::NetArity),
            [n(1), Value::Net(identity)],
        ))),
        Arc::new(TestExpr::Value(n(42))),
    );

    assert_eq!(eval_closed_expr(&expression).unwrap(), n(42));
}

#[test]
fn net_arity_contextualizes_failure_while_demanding_its_arity() {
    let net = closed_net(|builder| builder.data(n(42)));
    let application = apply_values(
        &test_context(),
        Value::Builtin(Builtin::NetArity),
        vec![
            Value::error(
                &crate::core::test_value_factory(),
                "arity computation failed",
            ),
            Value::Net(net),
        ],
    )
    .expect("net-arity application construction should remain lazy");
    let error = eval_value(&test_context(), &application)
        .expect_err("failure while evaluating net arity must propagate");
    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("net_arity")]
    );
}

#[test]
fn net_arity_does_not_demand_the_net_before_its_arity() {
    let context = test_context();
    let arity = PromisedValue::new(context.values(), "net arity");
    let net_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&net_demands);
    let net = closed_net(|builder| builder.data(n(42)));
    let net = Value::semantic_thunk(context.values(), "instrumented net", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Net(net.clone()))
    });
    let application = apply_values(
        &context,
        Value::Builtin(Builtin::NetArity),
        vec![Value::Promised(arity.clone()), net],
    )
    .expect("net-arity application should build");
    let blocked = eval_value(&context, &application)
        .expect_err("net arity must suspend at its first operand");
    assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
    assert_eq!(net_demands.load(Ordering::SeqCst), 0);

    set_promise(&context, &arity, n(0)).expect("the arity should accept its assignment");
    assert_eq!(
        eval_value(&context, &application).expect("net arity should resume in source order"),
        n(42)
    );
    assert_eq!(net_demands.load(Ordering::SeqCst), 1);
}

#[test]
fn net_arity_resumes_its_net_without_replaying_the_completed_arity() {
    // This fixture performs explicit collection. Keep its heap private so it
    // cannot collect raw values held by another parallel shared-fixture test.
    let context = isolated_test_context();
    let arity_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&arity_demands);
    let arity = Value::semantic_thunk(context.values(), "instrumented arity", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(n(1))
    });
    let net = PromisedValue::new(context.values(), "net-arity net");
    let application = apply_values(
        &context,
        Value::Builtin(Builtin::NetArity),
        vec![arity, Value::Promised(net.clone())],
    )
    .expect("net-arity application should build");
    let Value::Lazy(application_lazy) = &application else {
        unreachable!("a saturated net-arity builtin must remain lazy")
    };
    let application_root = application_lazy.root(context.values());

    let blocked = eval_value(&context, &application)
        .expect_err("net arity must suspend at its unresolved net");
    assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
    assert_eq!(arity_demands.load(Ordering::SeqCst), 1);
    context
        .values()
        .collect_managed_for_test()
        .expect("the net checkpoint must retain its completed arity and pending net demand");
    eval_value(&context, &application)
        .expect_err("a later route must resume the same net dependency");
    assert_eq!(arity_demands.load(Ordering::SeqCst), 1);

    let identity = closed_net_in(context.values(), |builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    set_promise(&context, &net, Value::Net(identity))
        .expect("the net operand should accept its assignment");
    context
        .values()
        .collect_managed_for_test()
        .expect("the assigned net must remain live beneath the checkpoint");
    let Value::Function(function) =
        eval_value(&context, &application).expect("net arity should resume at its net")
    else {
        panic!("positive net arity should produce a function")
    };
    assert_eq!(function.remaining_arity(), 1);
    assert_eq!(arity_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

#[test]
fn observing_a_function_net_preserves_the_net_value() {
    let identity = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    let expected = identity.clone();

    assert_eq!(
        eval_value(&test_context(), &Value::Net(identity)).unwrap(),
        Value::Net(expected)
    );
}

#[test]
fn net_backed_lazy_values_require_an_exposed_data_node() {
    let identity = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    let value = Value::Lazy(LazyValue::from_net_computation(
        &crate::core::test_value_factory(),
        identity,
    ));

    let error = eval_value(&test_context(), &value)
        .expect_err("a net computation must expose data rather than a bind");
    assert_eq!(
        error.to_string(),
        "lazy net computation exposed a bind instead of data"
    );
    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("net_computation")]
    );
}

#[test]
fn net_backed_lazy_values_reject_non_data_normal_forms() {
    let inert = closed_net(|builder| builder.copy(0).input);
    let value = Value::Lazy(LazyValue::from_net_computation(
        &crate::core::test_value_factory(),
        inert,
    ));

    let error = eval_value(&test_context(), &value)
        .expect_err("an inert net computation must not produce a value");
    assert_eq!(
        error.to_string(),
        "lazy net computation reached a non-data normal form"
    );
    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("net_computation")]
    );
}

#[test]
fn early_function_data_is_left_to_ordinary_stuck_net_semantics() {
    let one_argument_stage = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        let erase = builder.copy(0);
        builder.wire(argument, erase.input);
        let data = builder.data(n(42));
        builder.wire(result, data);
        application
    });
    let function = FunctionValue::new(one_argument_stage, 2);
    let partial = apply_function_values(&test_context(), function, vec![n(0)])
        .expect("partial application should not inspect the staged interface");
    let partial = eval_value(&test_context(), &partial)
        .expect("partial application should produce a function value");
    let Value::Function(partial) = partial else {
        panic!("partial application should retain an ordinary function value")
    };
    assert_eq!(partial.remaining_arity(), 1);

    assert_eq!(
        eval_value(
            &test_context(),
            &apply_function_values(&test_context(), partial, vec![n(1)]).unwrap(),
        )
        .unwrap_err()
        .to_string(),
        "application requires a function value, received Number"
    );
}

#[test]
fn net_arity_bridges_opaque_nets_to_computations_and_functions() {
    let data_net = closed_net(|builder| builder.data(n(42)));
    let computation = apply_test_values(
        Value::Builtin(Builtin::NetArity),
        [n(0), Value::Net(data_net)],
    );
    assert_eq!(eval_value(&test_context(), &computation).unwrap(), n(42));

    let identity = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    let function = apply_test_values(
        Value::Builtin(Builtin::NetArity),
        [n(1), Value::Net(identity)],
    );
    let Value::Function(function) = eval_value(&test_context(), &function).unwrap() else {
        panic!("positive net arity should produce a function value")
    };
    assert_eq!(function.remaining_arity(), 1);
    let result = apply_test_values(Value::Function(function), [n(43)]);
    assert_eq!(eval_value(&test_context(), &result).unwrap(), n(43));
}

#[test]
fn saturated_function_calls_reject_a_remaining_bind() {
    let two_argument_stage = closed_net(|builder| {
        let spine = builder.bind_spine(2);
        for argument in &spine.arguments {
            let eraser = builder.copy(0);
            builder.wire(*argument, eraser.input);
        }
        let result = builder.data(n(42));
        builder.wire(spine.result, result);
        spine.input
    });
    let malformed = FunctionValue::new(two_argument_stage, 1);
    let result = apply_function_values(&test_context(), malformed, vec![n(0)]).unwrap();

    assert_eq!(
        eval_value(&test_context(), &result)
            .unwrap_err()
            .to_string(),
        "function call exposed a bind instead of data"
    );
}

#[test]
fn zero_arity_apply_operator_is_data_identity() {
    let data = n(42);
    let context = test_context();
    let operator = context
        .values()
        .with_runtime_value_access(|access| apply_arity_operator(&access, 0, Arc::from([])));

    assert_eq!(
        with_direct_evaluator(&context, |evaluator| {
            evaluator.with_value_access(|access| apply_core_operator(&access, &operator, &data))
        })
        .unwrap(),
        OperatorYield::Data(data)
    );
}

#[test]
fn compiled_function_values_reuse_one_shared_interaction_net() {
    let function = closed_function_value(1, TestExpr::Local(0));
    let (Value::Function(first), Value::Function(second)) = (
        eval_value(&test_context(), &function).unwrap(),
        eval_value(&test_context(), &function).unwrap(),
    ) else {
        panic!("closed functions should evaluate to shared function stages");
    };
    crate::core::test_value_factory().with_runtime_value_access(|access| {
        assert!(
            first
                .stage()
                .runtime()
                .same_net_in(second.stage().runtime(), &access)
        );
    });
}

#[test]
fn curried_function_partial_application_retains_a_shared_stage() {
    let function = closed_function_value(3, TestExpr::Local(2));
    let partially_applied = eval_value(&test_context(), &apply_test_values(function, [n(11)]))
        .expect("first application should construct the remaining function stage");
    let Value::Function(first_stage) = &partially_applied else {
        panic!("partial application should produce another function stage");
    };
    assert_eq!(first_stage.remaining_arity(), 2);
    let cloned_stage = partially_applied.clone();
    let Value::Function(cloned_stage) = cloned_stage else {
        unreachable!()
    };
    crate::core::test_value_factory().with_runtime_value_access(|access| {
        assert!(
            first_stage
                .stage()
                .runtime()
                .same_net_in(cloned_stage.stage().runtime(), &access,)
        );
    });

    let result = apply_test_values(partially_applied, [n(22), n(33)]);
    assert_eq!(eval_value(&test_context(), &result).unwrap(), n(11));
}

#[test]
fn function_application_accepts_a_cursor_backed_function_argument_without_forcing_it() {
    let ignores_first = closed_function_value(2, TestExpr::Local(0));
    let forwards_argument = closed_function_value(
        1,
        TestExpr::Apply(
            Arc::new(TestExpr::Value(ignores_first)),
            Arc::new(TestExpr::Local(0)),
        ),
    );
    let unresolved_function = closed_function_value(1, TestExpr::Local(0));

    let partial = eval_value(
        &test_context(),
        &apply_test_values(forwards_argument, [unresolved_function]),
    )
    .expect("net attachment must not demand a callable argument as embedded data");
    assert!(matches!(partial, Value::Function(_)));

    assert_eq!(
        eval_value(&test_context(), &apply_test_values(partial, [n(42)])).unwrap(),
        n(42)
    );
}

#[test]
fn batched_application_spine_keeps_unused_arguments_lazy() {
    let forced = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lazy_argument = |label: &'static str| {
        let forced = forced.clone();
        Value::semantic_thunk(&crate::core::test_value_factory(), label, move |_| {
            forced.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(99))
        })
    };
    let function = closed_function_value(3, TestExpr::Local(2));
    let application = apply_test_values(
        function,
        [n(11), lazy_argument("second"), lazy_argument("third")],
    );

    assert_eq!(eval_value(&test_context(), &application).unwrap(), n(11));
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn batched_application_preserves_captured_access() {
    let key = Key::atom_from_text("answer");
    let function = closed_function_value(
        2,
        TestExpr::Access(
            Arc::new(TestExpr::Local(1)),
            Arc::from([TestKey::Key(key.clone())]),
        ),
    );
    let dict = Value::Dict(Dict::new_sync().insert(key, n(42)));
    let application = apply_test_values(function, [dict, n(0)]);

    assert_eq!(eval_value(&test_context(), &application).unwrap(), n(42));
}

#[test]
fn compiling_a_function_does_not_evaluate_its_body() {
    let function = closed_function_value(
        1,
        TestExpr::Value(Value::error(
            &crate::core::test_value_factory(),
            "unreached body",
        )),
    );

    assert!(matches!(function, Value::Function(_)));
}

fn n(value: i64) -> Value {
    Value::Number(value.into())
}

fn same_runtime_contexts() -> (
    OwnedEvalContext,
    OwnedEvalContext,
    Arc<crate::evaluation::EvaluationExecutor>,
) {
    let (coordinator, executor) = crate::evaluation::test_execution_resources(0)
        .expect("same-runtime evaluator resources should build");
    let owner_session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let observer_session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let owner = OwnedEvalContext::new(owner_session);
    let observer = OwnedEvalContext::new(observer_session);
    (owner, observer, executor)
}

#[test]
fn promised_values_fail_fast_without_poisoning_later_assignment() {
    let context = test_context();
    let promised = PromisedValue::new(&crate::core::test_value_factory(), "test promised value");
    let value = Value::Promised(promised.clone());

    assert_eq!(
        eval_value(&context, &value).unwrap_err().to_string(),
        "promised value was observed before initialization"
    );
    assert_eq!(promised.assignment(context.values()), None);
    set_promise(&context, &promised, n(42)).unwrap();
    assert_eq!(eval_value(&context, &value).unwrap(), n(42));
}

#[test]
fn deferred_computation_blockage_does_not_poison_its_lazy_cache() {
    let session = test_context();
    let (promise, _owner_task, _owner) = session
        .task_owned_promise(Arc::from("deferred computation input"))
        .unwrap();
    let observer = session.with_new_task().unwrap();
    let promised_value = Value::Promised(promise.clone());
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted_attempts = attempts.clone();
    let lazy = LazyValue::semantic_thunk(
        &crate::core::test_value_factory(),
        "promise-demanding deferred computation",
        move |context| {
            counted_attempts.fetch_add(1, Ordering::SeqCst);
            eval_value_in(context, &promised_value)
        },
    );
    let value = Value::Lazy(lazy.clone());

    let blocked =
        eval_value(&observer, &value).expect_err("the unresolved input promise should block");
    assert!(blocked.blocked_on().is_some());
    assert!(
        lazy.cached(session.values()).is_none(),
        "a retryable deferred error must not poison the terminal lazy cache"
    );
    assert!(
        session.lazy_failure(&lazy).is_none(),
        "the scheduler must not record a permanent lazy failure while its input may change"
    );

    set_promise(&session, &promise, n(42)).unwrap();
    assert_eq!(eval_value(&observer, &value).unwrap(), n(42));
    assert!(
        attempts.load(Ordering::SeqCst) >= 2,
        "resuming the producer should retry the deferred computation"
    );

    let completed_attempts = attempts.load(Ordering::SeqCst);
    assert_eq!(eval_value(&observer, &value).unwrap(), n(42));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        completed_attempts,
        "the successful terminal cache should prevent another thunk invocation"
    );
}

#[test]
fn deferred_computation_caches_one_structured_failure() {
    let context = test_context();
    let detail = Key::atom_from_text("detail");
    let emission = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::MSG).clone(),
                Value::Dict(Dict::new_sync().insert(
                    (*keys::TEXT).clone(),
                    Value::binary_from_text("structured deferred failure"),
                )),
            )
            .insert(detail.clone(), n(7)),
    );
    let frame = evaluation_context_frame("deferred_test");
    let thunk_emission = emission.clone();
    let thunk_frame = frame.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted_attempts = attempts.clone();
    let lazy = LazyValue::semantic_thunk(
        &crate::core::test_value_factory(),
        "structured deferred failure",
        move |context| {
            counted_attempts.fetch_add(1, Ordering::SeqCst);
            Err(context
                .context()
                .values()
                .with_runtime_value_access(|access| {
                    EvaluationHalt::from_value(&access, thunk_emission.clone())
                        .with_context(&access, thunk_frame.clone())
                }))
        },
    );
    let value = Value::Lazy(lazy.clone());

    let error =
        eval_value(&context, &value).expect_err("the deferred computation should fail permanently");
    let observed_failure = error.into_permanent_failure();
    let cached_failure = lazy
        .cached(context.values())
        .expect("a permanent deferred failure should be cached")
        .expect_err("the cached result should be the failure");

    assert!(Arc::ptr_eq(&observed_failure, &cached_failure));
    assert_eq!(cached_failure.emission_value(), Some(&emission));
    assert_eq!(cached_failure.contexts(), [frame]);
    let Value::Dict(diagnostic) = failure_diagnostic_value(&cached_failure) else {
        panic!("a structured failure should project to a diagnostic dictionary")
    };
    assert_eq!(diagnostic.get(&detail), Some(&n(7)));

    assert_eq!(
        eval_value(&context, &value)
            .expect_err("the cached value should remain failed")
            .into_permanent_failure(),
        cached_failure
    );
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "a cached permanent failure must not invoke its thunk again"
    );
}

#[test]
fn deferred_list_effect_work_blocks_and_resumes() {
    let session = test_context();
    let (promise, _owner_task, _owner) = session
        .task_owned_promise(Arc::from("deferred list effect input"))
        .unwrap();
    let observer = session.with_new_task().unwrap();
    let handled = apply_builtin(
        &observer,
        Builtin::ListEffect,
        Vec::new(),
        Value::Promised(promise.clone()),
    )
    .expect("constructing the lazy list-effect result should not demand its operation");
    let Value::List(results) = handled else {
        panic!("the list effect handler should return a list")
    };

    let blocked = list_to_value_items(&observer, &results)
        .expect_err("observing the list should block on its unresolved effect");
    assert!(blocked.blocked_on().is_some());

    let return_effect = list_return_effect(n(42));
    set_promise(&session, &promise, return_effect).unwrap();

    assert_eq!(
        list_to_value_items(&observer, &results)
            .expect("the list effect should resume after its operation is assigned"),
        vec![n(42)]
    );
}

#[test]
fn list_effect_recipes_resume_at_sequence_cut_and_fix_boundaries() {
    let session = test_context();

    let (continuation, _continuation_task, _continuation_owner) = session
        .task_owned_promise(Arc::from("list effect sequence continuation"))
        .unwrap();
    let Value::List(sequence) = apply_values(
        &session,
        Value::Builtin(Builtin::ListEffectSeq),
        vec![
            list_return_effect(n(1)),
            Value::Promised(continuation.clone()),
        ],
    )
    .and_then(|value| eval_value(&session, &value))
    .expect("sequence construction should remain lazy") else {
        panic!("list effect sequence must construct a list")
    };
    assert!(
        list_to_value_items(&session, &sequence)
            .expect_err("sequence must wait at its continuation application")
            .blocked_on()
            .is_some()
    );
    set_promise(
        &session,
        &continuation,
        closed_function_value(1, TestExpr::Value(list_return_effect(n(42)))),
    )
    .expect("the sequence continuation should accept its assignment");
    assert_eq!(list_to_value_items(&session, &sequence).unwrap(), [n(42)]);

    let (cut_operation, _cut_task, _cut_owner) = session
        .task_owned_promise(Arc::from("list effect cut operation"))
        .unwrap();
    let Value::List(cut) = apply_values(
        &session,
        Value::Builtin(Builtin::ListEffectCut),
        vec![Value::Promised(cut_operation.clone())],
    )
    .and_then(|value| eval_value(&session, &value))
    .expect("cut construction should remain lazy") else {
        panic!("list effect cut must construct a list")
    };
    assert!(
        list_to_value_items(&session, &cut)
            .expect_err("cut must wait for its operation")
            .blocked_on()
            .is_some()
    );
    set_promise(&session, &cut_operation, list_return_effect(n(43)))
        .expect("the cut operation should accept its assignment");
    assert_eq!(list_to_value_items(&session, &cut).unwrap(), [n(43)]);

    let (fix_operation, _fix_task, _fix_owner) = session
        .task_owned_promise(Arc::from("list effect fix operation"))
        .unwrap();
    let fix_function =
        closed_function_value(1, TestExpr::Value(Value::Promised(fix_operation.clone())));
    let Value::List(fixed) = apply_values(
        &session,
        Value::Builtin(Builtin::ListEffectFix),
        vec![fix_function],
    )
    .and_then(|value| eval_value(&session, &value))
    .expect("fix construction should remain lazy") else {
        panic!("list effect fix must construct a list")
    };
    assert!(
        list_to_value_items(&session, &fixed)
            .expect_err("fix must wait for its operation")
            .blocked_on()
            .is_some()
    );
    set_promise(&session, &fix_operation, list_return_effect(n(44)))
        .expect("the fix operation should accept its assignment");
    assert_eq!(list_to_value_items(&session, &fixed).unwrap(), [n(44)]);
}

#[test]
fn list_effect_fix_defers_function_demand_and_resumes_without_replay() {
    let session = test_context();
    let (function_promise, _function_task, _function_owner) = session
        .task_owned_promise(Arc::from("list effect fix function"))
        .expect("the owner should allocate the fix function promise");
    let demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&demands);
    let promised_function = function_promise.clone();
    let function = Value::semantic_thunk(
        session.values(),
        "instrumented list effect fix function",
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Promised(promised_function.clone()))
        },
    );

    let Value::List(fixed) = apply_values(
        &session,
        Value::Builtin(Builtin::ListEffectFix),
        vec![function],
    )
    .and_then(|value| eval_value(&session, &value))
    .expect("list effect fix construction should not demand its function") else {
        panic!("list effect fix must construct a list")
    };
    assert_eq!(demands.load(Ordering::SeqCst), 0);

    let blocked = list_to_value_items(&session, &fixed)
        .expect_err("observing fix results should wait for its function");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(demands.load(Ordering::SeqCst), 1);

    set_promise(
        &session,
        &function_promise,
        closed_function_value(1, TestExpr::Value(list_return_effect(n(45)))),
    )
    .expect("the fix function should accept its assignment");
    assert_eq!(
        list_to_value_items(&session, &fixed).expect("fix should resume at its function"),
        [n(45)]
    );
    assert_eq!(demands.load(Ordering::SeqCst), 1);
}

#[test]
fn interaction_net_construction_dependency_does_not_poison_its_lazy_value() {
    let session = test_context();
    let (promise, _owner_task, _owner) = session
        .task_owned_promise(Arc::from("pending net effect"))
        .unwrap();
    let observer = session.with_new_task().unwrap();
    let lazy = LazyValue::from_net_construction(
        &crate::core::test_value_factory(),
        Value::Promised(promise),
    );
    let value = Value::Lazy(lazy.clone());

    let blocked = eval_value(&observer, &value)
        .expect_err("net construction should block on its unresolved effect");
    assert!(blocked.blocked_on().is_some());
    assert!(
        lazy.cached(session.values()).is_none(),
        "a retryable construction dependency must not become a cached failure"
    );
}

#[test]
fn interaction_net_builtin_dispatches_into_the_construction_owner() {
    let session = test_context();
    let (promise, _owner_task, _owner) = session
        .task_owned_promise(Arc::from("pending builtin net effect"))
        .expect("the construction effect should have an owner");
    let observer = session
        .with_new_task()
        .expect("the construction effect should have an observer");
    let construction = apply_values(
        &observer,
        Value::Builtin(Builtin::InteractionNet),
        vec![Value::Promised(promise)],
    )
    .expect("interaction-net application should build");

    let blocked = eval_value(&observer, &construction)
        .expect_err("builtin dispatch should reach the net-construction demand");
    assert!(blocked.blocked_on().is_some());
}

#[test]
fn deferred_computation_caches_one_text_failure() {
    let context = test_context();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted_attempts = attempts.clone();
    let lazy = LazyValue::semantic_thunk(
        &crate::core::test_value_factory(),
        "text deferred failure",
        move |_| {
            counted_attempts.fetch_add(1, Ordering::SeqCst);
            Err(EvaluationHalt::new("text deferred failure"))
        },
    );
    let value = Value::Lazy(lazy.clone());

    let first = eval_value(&context, &value)
        .expect_err("the deferred computation should fail")
        .into_permanent_failure();
    let cached = lazy
        .cached(context.values())
        .expect("the deferred failure should be cached")
        .expect_err("the terminal cache should contain a failure");
    assert!(Arc::ptr_eq(&first, &cached));

    let second = eval_value(&context, &value)
        .expect_err("the cached deferred computation should remain failed")
        .into_permanent_failure();
    assert!(Arc::ptr_eq(&cached, &second));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn deferred_computation_preserves_context_annotation_frames() {
    let context = test_context();
    let frame = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("deferred"),
        Value::binary_from_text("context annotation"),
    ));
    let annotation = Value::Dict(Dict::new_sync().insert((*keys::CONTEXT).clone(), frame.clone()));
    let lazy = LazyValue::semantic_thunk(
        &crate::core::test_value_factory(),
        "context-annotated deferred failure",
        move |context| {
            context.with_value_access(|access| {
                apply_builtin_in(
                    &access,
                    Builtin::Anno,
                    vec![annotation.clone()],
                    Value::error(
                        &crate::core::test_value_factory(),
                        "annotated deferred failure",
                    ),
                )
            })
        },
    );

    let failure = eval_value(&context, &Value::Lazy(lazy))
        .expect_err("the annotated deferred computation should fail")
        .into_permanent_failure();
    assert_eq!(failure.contexts(), [frame]);
}

#[test]
fn computed_lazy_waits_on_an_empty_promise_without_caching_its_error() {
    let context = test_context();
    let promise = PromisedValue::new(&crate::core::test_value_factory(), "late assignment");
    let lazy = LazyValue::from_access(
        &crate::core::test_value_factory(),
        Arc::from([]),
        Arc::from([Value::Promised(promise.clone())]),
    );
    let value = Value::Lazy(lazy.clone());

    let blocked = eval_value(&context, &value).expect_err("empty promise should block its lazy");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(promise.exact_subscription_count(context.values()), 1);
    assert!(lazy.cached(context.values()).is_none());
    assert_eq!(promise.assignment(context.values()), None);

    set_promise(&context, &promise, n(42)).unwrap();
    assert_eq!(promise.exact_subscription_count(context.values()), 0);
    assert_eq!(eval_value(&context, &value).unwrap(), n(42));
    assert_eq!(
        lazy.cached(context.values()),
        Some(Ok(EvaluatedValue::try_from(n(42)).unwrap()))
    );
}

#[test]
fn resolver_failure_exactly_wakes_its_deferred_follower() {
    let context = test_context();
    let promise = PromisedValue::new(context.values(), "failed resolver input");
    let lazy = LazyValue::from_access(
        context.values(),
        Arc::from([]),
        Arc::from([Value::Promised(promise.clone())]),
    );
    let value = Value::Lazy(lazy);

    let blocked = eval_value(&context, &value)
        .expect_err("an unresolved resolver promise should block its follower");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(promise.exact_subscription_count(context.values()), 1);

    fail_promise_message(&context, &promise, "resolver failed deliberately")
        .expect("the unresolved resolver promise should fail once");
    assert_eq!(promise.exact_subscription_count(context.values()), 0);
    assert_eq!(
        eval_value(&context, &value)
            .expect_err("the woken follower should expose the resolver failure")
            .to_string(),
        "resolver failed deliberately"
    );
}

#[test]
fn resolver_completion_wakes_only_its_cross_session_deferred_follower() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let promise_a = PromisedValue::new(owner.values(), "cross-session promise A");
    let promise_b = PromisedValue::new(owner.values(), "cross-session promise B");
    let lazy_for = |promise: &PromisedValue| {
        Value::Lazy(LazyValue::from_access(
            observer.values(),
            Arc::from([]),
            Arc::from([Value::Promised(promise.clone())]),
        ))
    };
    let lazy_a = lazy_for(&promise_a);
    let lazy_b = lazy_for(&promise_b);

    assert!(eval_value(&observer, &lazy_a).is_err());
    assert!(eval_value(&observer, &lazy_b).is_err());
    assert_eq!(promise_a.exact_subscription_count(owner.values()), 1);
    assert_eq!(promise_b.exact_subscription_count(owner.values()), 1);

    set_promise(&owner, &promise_a, n(41)).expect("promise A should accept its assignment");
    assert_eq!(promise_a.exact_subscription_count(owner.values()), 0);
    assert_eq!(promise_b.exact_subscription_count(owner.values()), 1);
    assert_eq!(eval_value(&observer, &lazy_a).unwrap(), n(41));

    set_promise(&owner, &promise_b, n(42)).expect("promise B should accept its assignment");
    assert_eq!(promise_b.exact_subscription_count(owner.values()), 0);
    assert_eq!(eval_value(&observer, &lazy_b).unwrap(), n(42));
}

#[test]
fn promised_assignment_follows_a_lazy_without_resolving_the_raw_assignment() {
    let context = test_context();
    let target =
        LazyValue::semantic_thunk(&crate::core::test_value_factory(), "promise target", |_| {
            Ok(n(42))
        });
    let promise = PromisedValue::new(&crate::core::test_value_factory(), "forwarding promise");
    set_promise(&context, &promise, Value::Lazy(target.clone())).unwrap();

    assert_eq!(
        eval_value(&context, &Value::Promised(promise.clone())).unwrap(),
        n(42)
    );
    assert_eq!(
        promise.assignment(context.values()),
        Some(Ok(Value::Lazy(target.clone())))
    );
    assert_eq!(
        target.cached(context.values()),
        Some(Ok(EvaluatedValue::try_from(n(42)).unwrap()))
    );
}

#[test]
fn promised_failure_preserves_structured_diagnostic_and_identity() {
    let session = test_context();
    let (promise, _owner_task, _owner) = session
        .task_owned_promise(Arc::from("structured promise failure"))
        .unwrap();
    let observer = session.with_new_task().unwrap();
    let wait = promise
        .task(session.values())
        .expect("task-owned promise should expose its wait")
        .wait()
        .clone();
    let detail = Key::atom_from_text("detail");
    let emission = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::MSG).clone(),
                Value::Dict(Dict::new_sync().insert(
                    (*keys::TEXT).clone(),
                    Value::binary_from_text("structured promise failure"),
                )),
            )
            .insert(detail.clone(), n(7)),
    );
    let frame = evaluation_context_frame("promise_test");
    let failure =
        Arc::new(EvaluationFailure::emission(emission.clone()).with_context(frame.clone()));

    fail_promise(&session, &promise, failure.clone())
        .expect("new promise should accept one permanent failure");

    let observed = eval_value(&observer, &Value::Promised(promise))
        .expect_err("failed promise should expose its permanent failure")
        .into_permanent_failure();
    assert!(Arc::ptr_eq(&failure, &observed));
    assert_eq!(observed.emission_value(), Some(&emission));
    assert_eq!(observed.contexts(), [frame]);

    let EvaluationWaitPoll::Failed(wait_failure) = session.poll_wait(&wait) else {
        panic!("the promise wait should publish the same permanent failure")
    };
    assert!(Arc::ptr_eq(&failure, wait_failure.as_failure()));

    let Value::Dict(diagnostic) = failure_diagnostic_value(&observed) else {
        panic!("a structured promise failure should project to a diagnostic dictionary")
    };
    assert_eq!(diagnostic.get(&detail), Some(&n(7)));
}

#[test]
fn promise_only_cycle_remains_blocked_without_poisoning_its_assignment() {
    let context = test_context();
    let promise = PromisedValue::new(&crate::core::test_value_factory(), "promise cycle");
    set_promise(&context, &promise, Value::Promised(promise.clone()))
        .expect("promise should accept its own named assignment");

    let error = eval_value(&context, &Value::Promised(promise.clone()))
        .expect_err("strict promise recursion should remain blocked");
    assert!(error.blocked_on().is_some());
    assert!(context.promise_failure(&promise).is_none());
    assert!(matches!(
        promise.assignment(context.values()),
        Some(Ok(Value::Promised(assigned))) if assigned == promise
    ));
}

#[test]
fn mixed_promise_lazy_cycle_remains_retryable_without_poisoning_the_lazy() {
    let context = test_context();
    let promise = PromisedValue::new(&crate::core::test_value_factory(), "mixed promise");
    let lazy = LazyValue::from_access(
        &crate::core::test_value_factory(),
        Arc::from([]),
        Arc::from([Value::Promised(promise.clone())]),
    );
    set_promise(&context, &promise, Value::Lazy(lazy.clone())).unwrap();

    let error = eval_value(&context, &Value::Promised(promise.clone()))
        .expect_err("strict mixed recursion should remain blocked");
    assert!(error.blocked_on().is_some());
    assert!(context.promise_failure(&promise).is_none());
    assert!(context.lazy_failure(&lazy).is_none());
    assert!(lazy.cached(context.values()).is_none());
    assert!(matches!(
        promise.assignment(context.values()),
        Some(Ok(Value::Lazy(assigned))) if assigned == lazy
    ));
}

#[test]
fn task_owned_fixpoint_rejects_recursive_demand_and_blocks_other_tasks() {
    let session = test_context();
    let (fixpoint, _owner_task, owner) = session
        .task_owned_promise(Arc::from("test fixpoint"))
        .unwrap();
    let observer = session.with_new_task().unwrap();
    let wait = fixpoint
        .task(session.values())
        .expect("task-owned fixpoint should expose its wait")
        .wait()
        .clone();
    let value = Value::Promised(fixpoint.clone());

    let recursive = eval_value(&owner, &value).unwrap_err();
    assert!(
        recursive
            .to_string()
            .contains("recursively observed itself")
    );

    let blocked = eval_value(&observer, &value).unwrap_err();
    assert!(blocked.blocked_on().is_some());
    assert_eq!(fixpoint.exact_subscription_count(session.values()), 1);
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 1);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 1);

    set_promise(&session, &fixpoint, n(42)).unwrap();
    assert_eq!(fixpoint.exact_subscription_count(session.values()), 0);
    assert_eq!(eval_value(&observer, &value).unwrap(), n(42));
    assert_eq!(
        session.poll_wait(&wait),
        EvaluationWaitPoll::Complete(Box::new(crate::runtime::RuntimeValueRoot::new(
            session.values(),
            n(42),
        ))),
        "the retired promise wait must preserve late terminal observation"
    );
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 0);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 0);
}

#[test]
fn failed_task_fails_its_unresolved_fixpoint_promises() {
    let session = test_context();
    let (fixpoint, owner_task, _owner) = session
        .task_owned_promise(Arc::from("test fixpoint"))
        .unwrap();
    let observer = session.with_new_task().unwrap();
    let wait = fixpoint
        .task(session.values())
        .expect("task-owned fixpoint should expose its wait")
        .wait()
        .clone();
    let value = Value::Promised(fixpoint.clone());

    assert!(
        eval_value(&observer, &value)
            .unwrap_err()
            .blocked_on()
            .is_some()
    );
    assert_eq!(fixpoint.exact_subscription_count(session.values()), 1);
    session.fail_wait(owner_task.wait(), "producer failed deliberately");
    assert_eq!(fixpoint.exact_subscription_count(session.values()), 0);
    assert_eq!(
        eval_value(&observer, &value).unwrap_err().to_string(),
        "producer failed deliberately"
    );
    assert!(matches!(
        session.poll_wait(&wait),
        EvaluationWaitPoll::Failed(error)
            if error.to_string() == "producer failed deliberately"
    ));
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 0);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 0);
}

#[test]
fn explicitly_failed_task_promise_retires_its_wait_record() {
    let session = test_context();
    let (fixpoint, _owner_task, _owner) = session
        .task_owned_promise(Arc::from("test fixpoint"))
        .unwrap();
    let wait = fixpoint
        .task(session.values())
        .expect("task-owned fixpoint should expose its wait")
        .wait()
        .clone();

    fail_promise_message(&session, &fixpoint, "fixpoint failed deliberately").unwrap();

    assert!(matches!(
        session.poll_wait(&wait),
        EvaluationWaitPoll::Failed(error)
            if error.to_string() == "fixpoint failed deliberately"
    ));
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 0);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 0);
}

#[test]
fn producer_failure_retires_every_owned_promise_wait() {
    let session = test_context();
    let (promises, owner_task, _owner) = session
        .task_owned_promises([Arc::from("first fixpoint"), Arc::from("second fixpoint")])
        .unwrap();
    let waits: Vec<_> = promises
        .iter()
        .map(|promise| {
            promise
                .task(session.values())
                .expect("task-owned fixpoint should expose its wait")
                .wait()
        })
        .collect();

    session.fail_wait(owner_task.wait(), "producer failed all fixpoints");

    for wait in waits {
        assert!(matches!(
            session.poll_wait(&wait),
            EvaluationWaitPoll::Failed(error)
                if error.to_string() == "producer failed all fixpoints"
        ));
    }
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 0);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 0);
}

#[test]
fn producer_completion_retires_a_dropped_task_promise() {
    let session = test_context();
    let (wait, owner_task) = {
        let (promise, owner_task, _owner) = session
            .task_owned_promise(Arc::from("abandoned fixpoint"))
            .unwrap();
        let wait = promise
            .task(session.values())
            .expect("task-owned fixpoint should expose its wait")
            .wait()
            .clone();
        (wait, owner_task)
    };
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 1);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 1);

    session.complete_wait(owner_task.wait());

    assert!(matches!(
        session.poll_wait(&wait),
        EvaluationWaitPoll::Failed(error)
            if error.to_string().contains("completed without fulfilling")
    ));
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 0);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 0);
}

#[test]
fn value_fixpoint_reports_its_strict_lazy_dependency_cycle() {
    let context = test_context();
    let function = closed_function_value(1, TestExpr::Local(0));
    let fixpoint = Value::Lazy(LazyValue::computed_fixpoint(
        &crate::core::test_value_factory(),
        "recursive value fixpoint",
        FixpointComputation::Function(function),
    ));

    let error = eval_value(&context, &fixpoint).unwrap_err();
    assert!(
        error.to_string().contains("lazy dependency cycle"),
        "{error}"
    );
    assert!(error.blocked_on().is_none());

    let Value::Lazy(fixpoint_lazy) = &fixpoint else {
        unreachable!("test fixture is a lazy fixpoint")
    };
    let failure = context
        .lazy_failure(fixpoint_lazy)
        .expect("the fixpoint task should retain its structured failure");
    let cycle = failure
        .dependency_cycle_value()
        .expect("strict recursion should retain a dependency cycle");
    assert!(
        cycle
            .members
            .iter()
            .any(|member| member.id == fixpoint_lazy.id(context.values()))
    );

    let observer = context.with_new_task().unwrap();
    assert_eq!(
        eval_value(&observer, &fixpoint).unwrap_err().to_string(),
        error.to_string()
    );
}

#[test]
fn fixpoint_builtin_reports_a_strict_lazy_dependency_cycle() {
    let expression = builtin1_expr(Builtin::Fixpoint, function_expr(1, TestExpr::Local(0)));

    let error = eval_closed_expr(&expression).unwrap_err();
    assert!(
        error.to_string().contains("lazy dependency cycle"),
        "{error}"
    );
    assert!(error.blocked_on().is_none());
}

#[test]
fn fixpoint_builtin_resumes_from_its_exact_function_operand() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (function, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("fixpoint function"))
        .expect("the owner should allocate a promised function");
    let fixpoint = apply_values(
        &observer,
        Value::Builtin(Builtin::Fixpoint),
        vec![Value::Promised(function.clone())],
    )
    .expect("fixpoint application should build");

    let blocked = eval_value(&observer, &fixpoint)
        .expect_err("the unresolved function should suspend fixpoint construction");
    assert!(blocked.blocked_on().is_some());

    set_promise(
        &owner,
        &function,
        closed_function_value_in(observer.values(), 1, TestExpr::Value(n(42))),
    )
    .expect("the owner should resolve the promised function");
    assert_eq!(
        eval_value(&observer, &fixpoint).expect("fixpoint construction should resume"),
        n(42)
    );
}

#[test]
fn suspended_value_fixpoint_keeps_one_knot_for_concurrent_observers() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let observer = session.with_new_task().unwrap();
    let gate = reflection_annotation(&owner, n(0), n(42));
    let function = closed_function_value(1, TestExpr::Value(gate));
    let fixpoint = Value::Lazy(LazyValue::computed_fixpoint(
        &crate::core::test_value_factory(),
        "suspended value fixpoint",
        FixpointComputation::Function(function),
    ));

    let producer_block = eval_value(&owner, &fixpoint).unwrap_err();
    let producer_wait = producer_block
        .blocked_on()
        .expect("producer should suspend on its reflection gate");
    let observer_block = eval_value(&observer, &fixpoint).unwrap_err();
    let fixpoint_wait = observer_block
        .blocked_on()
        .expect("observer should wait on the fixpoint itself");
    assert_eq!(
        producer_wait, fixpoint_wait,
        "all observers should wait on the session-owned lazy task"
    );

    owner.complete_wait(&producer_wait.0);
    assert_eq!(eval_value(&owner, &fixpoint).unwrap(), n(42));
    assert_eq!(eval_value(&observer, &fixpoint).unwrap(), n(42));
}

#[test]
fn computed_fixpoint_uses_session_local_waits_while_sharing_its_result() {
    let first = test_context();
    let second = test_context();
    let promise = PromisedValue::new(
        &crate::core::test_value_factory(),
        "cross-session fixpoint input",
    );
    let function = closed_function_value(1, TestExpr::Value(Value::Promised(promise.clone())));
    let lazy = LazyValue::computed_fixpoint(
        &crate::core::test_value_factory(),
        "cross-session value fixpoint",
        FixpointComputation::Function(function),
    );
    let fixpoint = Value::Lazy(lazy.clone());

    let first_block =
        eval_value(&first, &fixpoint).expect_err("the first session should wait for the promise");
    let second_block = eval_value(&second, &fixpoint)
        .expect_err("the second session should own an independent wait");
    assert!(first_block.blocked_on().is_some());
    assert!(second_block.blocked_on().is_some());
    assert!(lazy.cached(first.values()).is_none());

    set_promise(&first, &promise, n(42)).unwrap();
    assert_eq!(eval_value(&first, &fixpoint).unwrap(), n(42));
    assert_eq!(eval_value(&second, &fixpoint).unwrap(), n(42));
    assert_eq!(cached_value(&lazy), n(42));
}

#[test]
fn computed_fixpoint_preserves_a_forwarded_structured_failure() {
    let context = test_context();
    let source = LazyValue::error(&crate::core::test_value_factory(), "fixpoint source failed");
    let function = closed_function_value(1, TestExpr::Value(Value::Lazy(source.clone())));
    let fixpoint = LazyValue::computed_fixpoint(
        &crate::core::test_value_factory(),
        "failed value fixpoint",
        FixpointComputation::Function(function),
    );

    let error = eval_value(&context, &Value::Lazy(fixpoint.clone()))
        .expect_err("the source failure should fail the fixpoint");
    assert_eq!(error.to_string(), "fixpoint source failed");

    let source_failure = source.cached(context.values()).unwrap().unwrap_err();
    let fixpoint_failure = fixpoint.cached(context.values()).unwrap().unwrap_err();
    assert!(Arc::ptr_eq(&source_failure, &fixpoint_failure));
}

#[test]
fn deferred_values_use_the_context_that_forces_them() {
    let context = test_context();
    let expected_context = context.clone();
    let value = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "context-sensitive test value",
        move |actual_context| {
            assert!(
                actual_context
                    .context()
                    .shares_session_with(&expected_context)
            );
            Ok(n(42))
        },
    );

    assert_eq!(eval_value(&context, &value).unwrap(), n(42));
}

#[test]
fn forcing_a_lazy_value_reaches_outer_whnf_without_forcing_lazy_fields() {
    let context = test_context();
    let field_forces = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted_field_forces = field_forces.clone();
    let field = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "lazy dictionary field",
        move |_| {
            counted_field_forces.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(42))
        },
    );
    let expected_field = field.clone();
    let forwarded = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "forwarded dictionary",
        move |_| {
            Ok(Value::Dict(
                Dict::new_sync().insert(Key::atom_from_text("field"), field.clone()),
            ))
        },
    );
    let root = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "forwarding root",
        move |_| Ok(forwarded.clone()),
    );

    let forced = eval_value(&context, &root).expect("root should reach dictionary WHNF");
    let Value::Dict(dict) = forced else {
        panic!("forcing should expose the outer dictionary")
    };
    assert_eq!(
        dict.get(&Key::atom_from_text("field")),
        Some(&expected_field)
    );
    assert_eq!(field_forces.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn guarded_lazy_self_reference_reaches_dictionary_whnf() {
    let context = test_context();
    let self_reference = Arc::new(std::sync::OnceLock::<LazyValue>::new());
    let captured = self_reference.clone();
    let lazy = LazyValue::semantic_thunk(
        &crate::core::test_value_factory(),
        "guarded self reference",
        move |_| {
            Ok(Value::Dict(
                Dict::new_sync().insert(
                    Key::atom_from_text("tail"),
                    Value::Lazy(
                        captured
                            .get()
                            .expect("guarded self reference should be installed")
                            .clone(),
                    ),
                ),
            ))
        },
    );
    self_reference
        .set(lazy.clone())
        .expect("guarded self reference should be installed once");

    let forced = eval_value(&context, &Value::Lazy(lazy.clone()))
        .expect("a lazy reference under a dictionary constructor is guarded");
    let Value::Dict(dict) = forced else {
        panic!("guarded recursive value should expose a dictionary")
    };
    assert_eq!(
        dict.get(&Key::atom_from_text("tail")),
        Some(&Value::Lazy(lazy))
    );
}

#[test]
fn lazy_aliases_share_and_cache_their_final_whnf() {
    let context = test_context();
    let target = Value::semantic_thunk(&crate::core::test_value_factory(), "alias target", |_| {
        Ok(n(42))
    });
    let Value::Lazy(target_lazy) = &target else {
        unreachable!()
    };
    let target_lazy = target_lazy.clone();
    let root = LazyValue::semantic_thunk(
        &crate::core::test_value_factory(),
        "shallow alias",
        move |_| Ok(target.clone()),
    );
    let value = Value::Lazy(root.clone());

    assert_eq!(context.deferred_task_count(), 0);
    assert_eq!(eval_value(&context, &value).unwrap(), n(42));
    assert_eq!(
        context.deferred_task_count(),
        0,
        "completed alias producers should retire from the session"
    );
    assert_eq!(cached_value(&target_lazy), n(42));
    assert_eq!(cached_value(&root), n(42));
    assert_eq!(eval_value(&context, &value).unwrap(), n(42));
    assert_eq!(
        context.deferred_task_count(),
        0,
        "cached observation should not register new deferred work"
    );
}

#[test]
fn demanded_forwarding_chain_caches_whnf_in_every_lazy_member() {
    let context = test_context();
    let identity = closed_function_value(1, TestExpr::Local(0));
    let leaf = LazyValue::semantic_thunk(
        &crate::core::test_value_factory(),
        "forwarding leaf",
        |_| Ok(n(42)),
    );
    let middle = LazyValue::from_application(
        &crate::core::test_value_factory(),
        identity.clone(),
        Arc::from([Value::Lazy(leaf.clone())]),
    );
    let root = LazyValue::from_application(
        &crate::core::test_value_factory(),
        identity,
        Arc::from([Value::Lazy(middle.clone())]),
    );

    assert_eq!(
        eval_value(&context, &Value::Lazy(root.clone())).unwrap(),
        n(42)
    );
    assert_eq!(cached_value(&leaf), n(42));
    assert_eq!(cached_value(&middle), n(42));
    assert_eq!(cached_value(&root), n(42));
}

#[test]
fn lazy_whnf_checkpoint_survives_yield_and_dependency_until_terminal_cache() {
    let context = test_context();
    let promise = PromisedValue::new(context.values(), "lazy checkpoint dependency");
    let lazy = LazyValue::from_application(
        context.values(),
        closed_function_value(1, TestExpr::Local(0)),
        Arc::from([Value::Promised(promise.clone())]),
    );
    let root = lazy.root(context.values());
    let wait = lazy_root_wait(&context, &root).expect("lazy producer should be admitted");

    assert_eq!(
        context.pump_wait(&wait, 1),
        EvaluationPumpOutcome::BudgetExhausted,
        "one bounded transition should publish resumable progress"
    );
    assert!(lazy.source_snapshot(context.values()).is_none());
    assert!(lazy.cached(context.values()).is_none());
    assert!(context.values().with_runtime_value_access(|access| {
        root.access(&access)
            .expect("lazy root and access should share one runtime")
            .checkpoint_snapshot()
            .is_some()
    }));

    context
        .values()
        .collect_managed_for_test()
        .expect("the lazy-owned checkpoint should survive collection while blocked");
    set_promise(&context, &promise, n(42)).expect("the dependency should accept its assignment");
    for _ in 0..32 {
        if !matches!(context.poll_wait(&wait), EvaluationWaitPoll::Pending(_)) {
            break;
        }
        assert_ne!(
            context.pump_wait(&wait, 64),
            EvaluationPumpOutcome::NoProgress,
            "assigned dependency should resume the exact checkpoint"
        );
    }

    assert!(matches!(
        context.poll_wait(&wait),
        EvaluationWaitPoll::Complete(_)
    ));
    assert_eq!(cached_value(&lazy), n(42));
    assert!(context.values().with_runtime_value_access(|access| {
        root.access(&access)
            .expect("lazy root and access should share one runtime")
            .checkpoint_snapshot()
            .is_none()
    }));
}

#[test]
fn forwarding_chain_preserves_one_structured_failure() {
    let context = test_context();
    let identity = closed_function_value(1, TestExpr::Local(0));
    let leaf = LazyValue::error(&crate::core::test_value_factory(), "shared failure");
    let root = LazyValue::from_application(
        &crate::core::test_value_factory(),
        identity,
        Arc::from([Value::Lazy(leaf.clone())]),
    );

    let error = eval_value(&context, &Value::Lazy(root.clone()))
        .expect_err("forwarding into an error should fail");
    assert_eq!(error.to_string(), "shared failure");

    let leaf_failure = leaf.cached(context.values()).unwrap().unwrap_err();
    let root_failure = root.cached(context.values()).unwrap().unwrap_err();
    assert!(Arc::ptr_eq(&leaf_failure, &root_failure));
}

#[test]
fn concurrent_host_calls_share_one_rooted_producer_without_parking() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let context = EvalContext::isolated(values.clone());
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let producer_release = release.clone();
    let (started_sender, started_receiver) = std::sync::mpsc::channel();
    let producer_values = values.clone();
    let producer_runs = Arc::new(AtomicUsize::new(0));
    let counted_runs = producer_runs.clone();
    let lazy = LazyValue::host_call(&values, "contended host call", move |_| {
        producer_values
            .collect_managed_for_test()
            .expect("the host call must inherit no evaluator mutator");
        counted_runs.fetch_add(1, Ordering::SeqCst);
        started_sender
            .send(())
            .expect("test should still be waiting for its producer");
        let (lock, changed) = &*producer_release;
        let mut released = lock.lock().expect("test release lock was poisoned");
        while !*released {
            released = changed
                .wait(released)
                .expect("test release lock was poisoned");
        }
        Ok(crate::runtime::RuntimeValueRoot::new(
            &producer_values,
            n(42),
        ))
    });
    let value = Value::Lazy(lazy);
    let producer_context = context.clone();
    let producer_value = value.clone();
    let producer = std::thread::spawn(move || eval_value(&producer_context, &producer_value));
    started_receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("producer should claim the lazy task");

    let (observed_sender, observed_receiver) = std::sync::mpsc::channel();
    let observer_context = context.clone();
    let observer_value = value.clone();
    let observer = std::thread::spawn(move || {
        observed_sender
            .send(eval_value(&observer_context, &observer_value))
            .expect("test should still be waiting for its observer");
    });
    let observed = observed_receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("a contending observer must return instead of parking");
    let first_wait = observed
        .expect_err("contending observation should block cooperatively")
        .blocked_on()
        .expect("contending observation should expose the lazy task wait");
    let second_wait = eval_value(&context, &value)
        .expect_err("a second contending observation should also block")
        .blocked_on()
        .expect("all contending observations should expose the wait");
    assert_eq!(first_wait, second_wait);
    assert_eq!(
        context.pump_wait(&first_wait.0, 256),
        crate::evaluation::EvaluationPumpOutcome::Busy,
        "a claimed producer is busy rather than quiescent"
    );

    let (lock, changed) = &*release;
    *lock.lock().expect("test release lock was poisoned") = true;
    changed.notify_all();
    observer.join().expect("observer should finish");
    assert_eq!(
        producer.join().expect("producer should finish").unwrap(),
        n(42)
    );
    assert_eq!(producer_runs.load(Ordering::SeqCst), 1);
}

#[test]
fn host_call_rejects_a_foreign_runtime_root() {
    let context = test_context();
    let foreign = CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let lazy = LazyValue::host_call(context.values(), "foreign host result", move |_| {
        Ok(crate::runtime::RuntimeValueRoot::new(&foreign, n(42)))
    });

    let error = eval_value(&context, &Value::Lazy(lazy))
        .expect_err("a host call cannot publish another runtime's value");
    assert!(error.to_string().contains("host call returned a value"));
}

#[test]
fn ready_lazy_errors_fail_when_observed() {
    let value = Value::error(&crate::core::test_value_factory(), "deliberate failure");

    assert_eq!(
        eval_value(&test_context(), &value).unwrap_err().to_string(),
        "deliberate failure"
    );
}

fn function_expr(arity: usize, body: TestExpr) -> TestExpr {
    function_expr_in(&crate::core::test_value_factory(), arity, body)
}

fn function_expr_in(values: &CoreValueFactory, arity: usize, body: TestExpr) -> TestExpr {
    let code = Arc::new(lower_test_function_code_in(values, arity, body));
    let captures = (0..code.capture_count())
        .map(TestExpr::Local)
        .map(Arc::new)
        .collect::<Vec<_>>();
    TestExpr::Function {
        code,
        captures: Arc::from(captures),
    }
}

fn k(value: i64) -> Key {
    Key::Number(value.into())
}

fn builtin2_expr(builtin: Builtin, left: TestExpr, right: TestExpr) -> TestExpr {
    TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(builtin))),
            Arc::new(left),
        )),
        Arc::new(right),
    )
}

fn builtin1_expr(builtin: Builtin, value: TestExpr) -> TestExpr {
    TestExpr::Apply(
        Arc::new(TestExpr::Value(Value::Builtin(builtin))),
        Arc::new(value),
    )
}

fn run_pattern_builtin(builtin: Builtin, value: Value) -> Result<Vec<Value>, EvaluationHalt> {
    let handled = eval_closed_expr(&builtin1_expr(
        Builtin::ListEffect,
        builtin1_expr(builtin, TestExpr::Value(value)),
    ))?;
    let Value::List(results) = handled else {
        panic!("the list effect handler should return a list");
    };
    list_to_value_items(&test_context(), &results)
}

fn run_pattern_builtin2(
    builtin: Builtin,
    first: Value,
    second: Value,
) -> Result<Vec<Value>, EvaluationHalt> {
    let handled = eval_closed_expr(&builtin1_expr(
        Builtin::ListEffect,
        builtin2_expr(builtin, TestExpr::Value(first), TestExpr::Value(second)),
    ))?;
    let Value::List(results) = handled else {
        panic!("the list effect handler should return a list");
    };
    list_to_value_items(&test_context(), &results)
}

fn run_pattern_equal(expected: Value, value: Value) -> Result<Vec<Value>, EvaluationHalt> {
    run_pattern_builtin2(Builtin::PatternEqual, expected, value)
}

fn builtin3_expr(builtin: Builtin, first: TestExpr, second: TestExpr, third: TestExpr) -> TestExpr {
    TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Apply(
                Arc::new(TestExpr::Value(Value::Builtin(builtin))),
                Arc::new(first),
            )),
            Arc::new(second),
        )),
        Arc::new(third),
    )
}

fn singleton_expr(key: Value, value: TestExpr) -> TestExpr {
    builtin2_expr(Builtin::DictSingleton, TestExpr::Value(key), value)
}

fn dict_union_expr(left: TestExpr, right: TestExpr) -> TestExpr {
    builtin2_expr(Builtin::DictUnion, left, right)
}

fn dict_update_expr(path: TestExpr, new_value: TestExpr, dict: TestExpr) -> TestExpr {
    builtin3_expr(Builtin::DictUpdate, path, new_value, dict)
}

fn global_access(path: Vec<TestKey>) -> TestExpr {
    TestExpr::Access(Arc::new(TestExpr::Local(0)), Arc::from(path))
}

fn key_value(key: &Key) -> Value {
    match key {
        Key::Atom(atom) => Value::Atom(*atom),
        Key::Number(number) => Value::Number(number.clone()),
        Key::Binary(bytes) => Value::Binary(bytes.clone()),
        Key::AbstractGlobalPath(parts) => Value::Atom(crate::core::Atom::from_key(
            &Key::AbstractGlobalPath(parts.clone()),
        )),
        Key::List(items) => Value::List(List::from_values(items.iter().map(key_value).collect())),
        Key::Dict(entries) => Value::Dict(
            entries
                .iter()
                .fold(crate::core::Dict::new_sync(), |dict, (key, value)| {
                    dict.insert(key.clone(), key_value(value))
                }),
        ),
    }
}

fn key_path_expr(path: Vec<Key>) -> TestExpr {
    TestExpr::Value(Value::List(List::from_values(
        path.iter().map(key_value).collect(),
    )))
}

fn module_value_expr(value: &Value) -> TestExpr {
    match value {
        Value::Dict(dict) => {
            let mut items = dict.iter();
            let Some((first_key, first_value)) = items.next() else {
                return TestExpr::Value(Value::Dict(crate::core::Dict::new_sync()));
            };

            let mut expr = singleton_expr(key_value(first_key), module_value_expr(first_value));
            for (key, value) in items {
                expr = dict_union_expr(
                    expr,
                    singleton_expr(key_value(key), module_value_expr(value)),
                );
            }
            expr
        }
        _ => TestExpr::Value(value.clone()),
    }
}

fn fixpoint_dict(dict: Dict) -> TestExpr {
    TestExpr::Apply(
        Arc::new(TestExpr::Value(Value::Builtin(Builtin::Fixpoint))),
        Arc::new(function_expr(1, module_value_expr(&Value::Dict(dict)))),
    )
}

fn apply_rooted_fixture(root: &Value, expr: TestExpr) -> Value {
    apply_values(
        &test_context(),
        closed_function_value(1, expr),
        vec![root.clone()],
    )
    .expect("rooted test expression should lower to a callable function")
}

#[test]
fn evaluates_recursive_dictionary_net() {
    let asm = Dict::new_sync().insert(
        crate::core::Key::atom_from_text("result"),
        Value::binary_from_text("Hello, World!"),
    );
    let root = Dict::new_sync().insert(crate::core::Key::atom_from_text("asm"), Value::Dict(asm));

    let value = eval_closed_expr(&fixpoint_dict(root)).expect("term should evaluate");
    let asm = value
        .get_atom_path(&[crate::core::Atom::from_key(
            &crate::core::Key::binary_from_text("asm"),
        )])
        .expect("asm should exist");
    let asm = eval_value(&test_context(), asm)
        .expect("asm binding should evaluate lazily to a dictionary");
    let Value::Dict(asm) = asm else {
        panic!("asm should evaluate to a dictionary");
    };

    assert!(matches!(value, Value::Dict(_)));
    assert_eq!(
        asm.get(&crate::core::Key::atom_from_text("result")),
        Some(&Value::binary_from_text("Hello, World!"))
    );
}

#[test]
fn evaluates_binary_literals() {
    let value = eval_closed_expr(&TestExpr::Value(Value::binary_from_text("oops")))
        .expect("binary literal should evaluate");

    assert_eq!(value, Value::binary_from_text("oops"));
}

#[test]
fn appends_lists() {
    let expr = TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(Builtin::Append))),
            Arc::new(TestExpr::Value(Value::List(List::from_values(vec![
                n(1),
                n(2),
            ])))),
        )),
        Arc::new(TestExpr::Value(Value::List(List::from_values(vec![n(3)])))),
    );

    let value = eval_closed_expr(&expr).expect("append should evaluate");

    let Value::List(list) = value else {
        panic!("append should produce a list");
    };
    let mut values = Vec::new();
    list.for_each_segment(&mut |_bytes| Ok::<_, ()>(()), &mut |segment| {
        values.extend(segment.iter().cloned());
        Ok(())
    })
    .expect("should walk list");
    assert_eq!(values, vec![n(1), n(2), n(3)]);
}

#[test]
fn evaluates_mixed_list_segments() {
    let expr = TestExpr::List(Arc::from([
        Arc::new(TestExpr::Value(n(1))),
        Arc::new(TestExpr::Value(Value::binary_from_text("Hi"))),
        Arc::new(TestExpr::Value(n(2))),
        Arc::new(TestExpr::Value(Value::binary_from_text("!"))),
    ]));

    let value = eval_closed_expr(&expr).expect("list should evaluate");

    let Value::List(list) = value else {
        panic!("list expression should produce a list");
    };
    let mut saw_bytes = Vec::new();
    let mut saw_values = Vec::new();
    list.for_each_segment(
        &mut |bytes| {
            saw_bytes.push(bytes.to_vec());
            Ok::<_, ()>(())
        },
        &mut |segment| {
            saw_values.push(segment.to_vec());
            Ok(())
        },
    )
    .expect("should walk list");

    assert_eq!(
        saw_values,
        vec![
            vec![n(1)],
            vec![Value::binary_from_text("Hi")],
            vec![n(2)],
            vec![Value::binary_from_text("!")]
        ]
    );
    assert!(saw_bytes.is_empty());
}

#[test]
fn appends_list_and_binary() {
    let expr = TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(Builtin::Append))),
            Arc::new(TestExpr::Value(Value::List(List::from_values(vec![
                n(72),
                n(105),
            ])))),
        )),
        Arc::new(TestExpr::Value(Value::binary_from_text("!"))),
    );

    let value = eval_closed_expr(&expr).expect("append should evaluate");

    assert!(matches!(value, Value::List(_)));
}

#[test]
fn append_preserves_lazy_list_chunks_until_observed() {
    let expr = builtin2_expr(
        Builtin::Append,
        TestExpr::Value(Value::List(List::from_values(vec![n(72)]))),
        builtin2_expr(
            Builtin::Append,
            TestExpr::Value(Value::binary_from_text("i")),
            TestExpr::Value(Value::binary_from_text("!")),
        ),
    );

    let value = eval_closed_expr(&expr).expect("append should evaluate lazily");

    let Value::List(list) = value else {
        panic!("append should produce a list");
    };
    assert_eq!(list.known_len(), None);
    assert_eq!(
        list_output_bytes(&test_context(), &list).expect("lazy chunk should force"),
        b"Hi!"
    );
}

#[test]
fn binary_output_does_not_flatten_nested_binary_values() {
    let list = List::from_values(vec![
        Value::binary_from_text("A"),
        n(10),
        Value::binary_from_text("B"),
    ]);

    let error = list_output_bytes(&test_context(), &list)
        .expect_err("nested binary values must not be flattened during extraction");
    assert!(error.to_string().contains("byte integers"));
    assert_eq!(failure_context_items(&error), []);
}

#[test]
fn binary_output_contextualizes_only_nested_evaluation_failures() {
    let list = List::from_values(vec![Value::error(
        &crate::core::test_value_factory(),
        "byte computation failed",
    )]);

    let error = list_output_bytes(&test_context(), &list)
        .expect_err("a failed byte computation must propagate");

    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("binary_extraction")]
    );
}

#[test]
fn list_concat_explicitly_flattens_one_level() {
    let outer = Value::List(List::from_values(vec![
        Value::binary_from_text("A"),
        Value::List(List::from_bytes(Bytes::from_static(b"B"))),
    ]));
    let flattened = eval_closed_expr(&builtin1_expr(Builtin::ListConcat, TestExpr::Value(outer)))
        .expect("list concat should evaluate");
    let Value::List(flattened) = flattened else {
        panic!("list concat should return a list");
    };

    assert_eq!(
        list_output_bytes(&test_context(), &flattened).unwrap(),
        b"AB"
    );
}

#[test]
fn list_concat_defers_outer_children_and_observes_the_suffix_from_the_back() {
    let context = test_context();
    let outer = Value::List(List::concat(
        List::from_thunk(
            LazyValue::error(context.values(), "list concat forced its unused prefix").into(),
        ),
        List::from_values(vec![Value::binary_from_text("B")]),
    ));
    let flattened = apply_values(&context, Value::Builtin(Builtin::ListConcat), vec![outer])
        .and_then(|value| eval_value(&context, &value))
        .expect("list concat should expose one transformed spine node");
    let Value::List(flattened) = flattened else {
        panic!("list concat should produce a list")
    };
    let stats = flattened.visit_logical_parts(&mut |_| {});
    assert_eq!(stats.thunk_items, 2);
    assert_eq!(stats.value_items, 0);

    let split = apply_values(
        &context,
        Value::Builtin(Builtin::ListSplitEnd),
        vec![n(1), Value::List(flattened)],
    )
    .and_then(|value| eval_value(&context, &value))
    .expect("the flattened suffix should remain observable from the back");
    let Value::Dict(split) = split else {
        panic!("split_end should produce a dictionary")
    };
    let Value::List(suffix) = split
        .get(&Key::atom_from_text("right"))
        .expect("split_end should retain its flattened suffix")
    else {
        panic!("split_end suffix should be a list")
    };
    assert_eq!(
        list_output_bytes(&context, suffix).expect("the suffix should evaluate"),
        b"B"
    );
}

#[test]
fn list_concat_delays_invalid_strict_elements_until_their_output_is_observed() {
    let context = test_context();
    let outer = Value::List(List::from_values(vec![n(99), Value::binary_from_text("B")]));
    let flattened = apply_values(&context, Value::Builtin(Builtin::ListConcat), vec![outer])
        .and_then(|value| eval_value(&context, &value))
        .expect("list concat should localize a strict-item failure");
    let Value::List(flattened) = flattened else {
        panic!("list concat should produce a list")
    };

    let split = apply_values(
        &context,
        Value::Builtin(Builtin::ListSplitEnd),
        vec![n(1), Value::List(flattened)],
    )
    .and_then(|value| eval_value(&context, &value))
    .expect("the valid suffix should not observe the invalid prefix item");
    let Value::Dict(split) = split else {
        panic!("split_end should produce a dictionary")
    };
    let Value::List(suffix) = split
        .get(&Key::atom_from_text("right"))
        .expect("split_end should retain its suffix")
    else {
        panic!("split_end suffix should be a list")
    };
    assert_eq!(list_output_bytes(&context, suffix).unwrap(), b"B");
}

#[test]
fn list_concat_resumes_after_its_source_becomes_available() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (source, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("list concat source"))
        .expect("the owner should allocate a promised concat source");
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::ListConcat),
        vec![Value::Promised(source.clone())],
    )
    .expect("list-concat application should build");

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved source should suspend list concat");
    assert!(blocked.blocked_on().is_some());

    set_promise(
        &owner,
        &source,
        Value::List(List::from_values(vec![Value::binary_from_text("A")])),
    )
    .expect("the owner should resolve the concat source");
    let Value::List(flattened) =
        eval_value(&observer, &application).expect("list concat should resume")
    else {
        panic!("list concat should produce a list")
    };
    assert_eq!(
        list_output_bytes(&observer, &flattened).expect("the flattened source should evaluate"),
        b"A"
    );
}

#[test]
fn lazy_list_chunks_error_when_they_do_not_evaluate_to_lists() {
    // The returned list retains managed lazy structure. Keep both its
    // construction and observation on one private heap, and root it while a
    // parallel collector may run in another fixture.
    let context = isolated_test_context();
    let sum = Value::builtin_call(context.values(), Builtin::Add, vec![n(1), n(1)]);
    let append = Value::builtin_call(
        context.values(),
        Builtin::Append,
        vec![Value::binary_from_text("Hi"), sum],
    );
    let value = eval_value(&context, &append).expect("append should preserve lazy chunk");
    let value_root = context
        .values()
        .with_runtime_value_access(|access| access.root_runtime_value(value));
    let value = value_root.clone_core_for_test();
    let Value::List(list) = value else {
        panic!("append should produce a list");
    };

    let err =
        list_output_bytes(&context, &list).expect_err("bad lazy chunk should fail when observed");
    assert!(
        err.to_string()
            .contains("lazy list chunk must evaluate to a list or binary value")
    );
    drop(value_root);
}

#[test]
fn promised_list_chunks_remain_assignable_after_early_observation() {
    let context = test_context();
    let promise = PromisedValue::new(context.values(), "promised list tail");
    let list = context.values().with_runtime_value_access(|access| {
        append_sequence(&access, Value::Promised(promise.clone()))
            .expect("a promise remains a valid deferred list tail")
    });

    assert!(
        list_output_bytes(&context, &list)
            .expect_err("an empty list promise should fail fast")
            .to_string()
            .contains("promised value was observed before initialization")
    );
    set_promise(
        &context,
        &promise,
        Value::Binary(Bytes::from_static(b"assigned")),
    )
    .expect("early observation must not fill the promise");
    assert_eq!(
        list_output_bytes(&test_context(), &list).expect("assigned list promise should resolve"),
        b"assigned"
    );
}

#[test]
fn split_end_does_not_force_lazy_left_branch_when_suffix_is_in_right_branch() {
    let lazy_left = List::from_thunk(
        LazyValue::error(&crate::core::test_value_factory(), "left branch was forced").into(),
    );
    let list = List::concat(lazy_left, List::from_bytes(Bytes::from_static(b"abc")));
    let split = eval_closed_expr(&builtin2_expr(
        Builtin::ListSplitEnd,
        TestExpr::Value(n(1)),
        TestExpr::Value(Value::List(list)),
    ))
    .expect("split_end should not force left branch");

    let Value::Dict(split) = split else {
        panic!("split_end should produce a dictionary");
    };
    let Value::List(suffix) = split
        .get(&Key::atom_from_text("right"))
        .expect("split should include right suffix")
    else {
        panic!("right suffix should be a list");
    };
    assert_eq!(
        list_output_bytes(&test_context(), suffix).expect("right suffix should render"),
        b"c"
    );
}

#[test]
fn evaluates_arithmetic_builtins() {
    let expr = builtin2_expr(
        Builtin::Subtract,
        builtin2_expr(
            Builtin::Add,
            TestExpr::Value(n(1)),
            builtin2_expr(
                Builtin::Multiply,
                TestExpr::Value(n(2)),
                TestExpr::Value(n(3)),
            ),
        ),
        builtin2_expr(
            Builtin::Divide,
            TestExpr::Value(n(4)),
            TestExpr::Value(n(5)),
        ),
    );

    let value = eval_closed_expr(&expr).expect("arithmetic should evaluate");

    assert_eq!(value, Value::Number(Number::parse("31/5").unwrap()));
}

#[test]
fn lazy_arguments_share_forced_values() {
    // The expression embeds a managed lazy before lowering it into the net.
    // Keep that construction and evaluation on a private heap so forced-GC
    // fixtures running in parallel cannot collect the pre-lowering value.
    let context = isolated_test_context();
    let force_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = force_count.clone();
    let counted = TestExpr::Value(Value::semantic_thunk(
        context.values(),
        "counted",
        move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(2))
        },
    ));
    let expr = TestExpr::Apply(
        Arc::new(function_expr_in(
            context.values(),
            1,
            builtin2_expr(Builtin::Add, TestExpr::Local(0), TestExpr::Local(0)),
        )),
        Arc::new(counted),
    );

    let code = lower_test_function_code_in(context.values(), 0, expr);
    let computation = Value::Lazy(LazyValue::from_net_computation(
        context.values(),
        NetValue::new(code.runtime().duplicate_for_test(context.values())),
    ));
    let value = eval_value(&context, &computation).expect("lambda body should evaluate");

    assert_eq!(value, n(4));
    assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn equality_errors_when_dictionary_comparison_reaches_functions() {
    let function = closed_function_value(1, TestExpr::Local(0));
    let left = Value::Dict(Dict::new_sync().insert(Key::atom_from_text("f"), function.clone()));
    let right = Value::Dict(Dict::new_sync().insert(Key::atom_from_text("f"), function));
    let err = eval_closed_expr(&builtin2_expr(
        Builtin::Equal,
        TestExpr::Value(left),
        TestExpr::Value(right),
    ))
    .expect_err("function-valued fields should not be equatable");

    assert!(err.to_string().contains("cannot compare function values"));
}

#[test]
fn interaction_net_classifies_ordinary_functions_as_applicable_operators() {
    let function = closed_function_value(2, TestExpr::Local(0));
    let callable = classify_core_callable(&test_context(), function.clone())
        .expect("an ordinary function should be callable from an interaction net");

    match callable {
        CoreCallable::Operator(CoreOperator::Applicable(actual)) => {
            assert_eq!(actual, function);
        }
        CoreCallable::Operator(other) => {
            panic!("ordinary function lowered to the wrong operator: {other:?}");
        }
        CoreCallable::Net(_) => {
            panic!("ordinary function must not expose its internal stage as a raw net");
        }
    }
}

#[test]
fn ordinary_observers_do_not_unseal_metadata_carriers() {
    // The assertions below repeatedly hand the same managed carrier through
    // independently evaluated fixtures. Keep that carrier rooted while a
    // parallel test may explicitly collect the shared test value domain.
    let values = crate::core::test_value_factory();
    let carrier_root = values.with_runtime_value_access(|access| {
        access.root_runtime_value(Value::initial_metadata_carrier(&values))
    });
    let carrier = carrier_root.clone_core_for_test();

    for builtin in [Builtin::Equal, Builtin::NotEqual, Builtin::Greater] {
        let error = eval_closed_expr(&builtin2_expr(
            builtin,
            TestExpr::Value(carrier.clone()),
            TestExpr::Value(carrier.clone()),
        ))
        .expect_err("comparison must not expose sealed carrier identity");
        assert!(
            error.to_string().contains("cannot compare sealed values"),
            "{error}"
        );
    }

    assert!(
        run_pattern_equal(unit_value(), carrier.clone())
            .expect("a sealed carrier should be an ordinary pattern mismatch")
            .is_empty()
    );
    for builtin in [Builtin::PatternIsList, Builtin::PatternIsDict] {
        assert!(
            run_pattern_builtin(builtin, carrier.clone())
                .expect("sealed carriers should mismatch ordinary shape patterns")
                .is_empty()
        );
    }

    let unit_error = eval_closed_expr(&builtin3_expr(
        Builtin::AssertUnit,
        TestExpr::Value(Value::binary_from_text("sealed result")),
        TestExpr::Value(carrier.clone()),
        TestExpr::Value(n(42)),
    ))
    .expect_err("a sealed unit carrier must not satisfy a unit assertion");
    assert_eq!(
        unit_error.to_string(),
        "sealed result: unit expected, received Sealed"
    );

    let application = apply_value(&test_context(), carrier.clone(), n(0))
        .expect("application construction should remain lazy");
    let application_error = eval_value(&test_context(), &application)
        .expect_err("a sealed unit carrier must not be callable");
    assert_eq!(
        application_error.to_string(),
        "application requires a function value, received Sealed"
    );
    let Err(net_call_error) = classify_core_callable(&test_context(), carrier.clone()) else {
        panic!("an interaction-net call must not unseal metadata");
    };
    assert_eq!(
        net_call_error.to_string(),
        "application requires a function value, received Sealed"
    );

    let key_error =
        eval_key(&carrier).expect_err("a sealed unit carrier must not become a dictionary key");
    assert_eq!(
        key_error.to_string(),
        "dictionary keys must evaluate to keyable values"
    );
    drop(carrier_root);
}

#[test]
fn binary_validation_does_not_disclose_sealed_metadata() {
    let hidden = Value::metadata_carrier(Value::binary_from_text("private trace"));
    let list = List::from_values(vec![hidden]);

    let error =
        list_output_bytes(&test_context(), &list).expect_err("sealed values are not binary bytes");
    assert!(error.to_string().contains("got Sealed(..)"), "{error}");
    assert!(!error.to_string().contains("private trace"), "{error}");
}

#[test]
fn evaluates_extended_math_builtins() {
    let floor = eval_closed_expr(&builtin1_expr(
        Builtin::Floor,
        TestExpr::Value(Value::Number(Number::parse("_7/2").unwrap())),
    ))
    .expect("floor should evaluate");
    let modulus = eval_closed_expr(&builtin2_expr(
        Builtin::Mod,
        TestExpr::Value(Value::Number(Number::parse("17/5").unwrap())),
        TestExpr::Value(Value::Number(Number::parse("3/2").unwrap())),
    ))
    .expect("mod should evaluate");

    assert_eq!(floor, Value::Number((-4).into()));
    assert_eq!(modulus, Value::Number(Number::parse("2/5").unwrap()));
}

#[test]
fn evaluates_slice_and_map_builtins() {
    let slice = eval_closed_expr(&builtin3_expr(
        Builtin::Slice,
        TestExpr::Value(n(1)),
        TestExpr::Value(n(4)),
        TestExpr::Value(Value::binary_from_text("World!")),
    ))
    .expect("slice should evaluate");
    let mapped = eval_closed_expr(&builtin2_expr(
        Builtin::Map,
        function_expr(
            1,
            TestExpr::Apply(
                Arc::new(TestExpr::Apply(
                    Arc::new(TestExpr::Value(Value::Builtin(Builtin::Add))),
                    Arc::new(TestExpr::Local(0)),
                )),
                Arc::new(TestExpr::Value(n(1))),
            ),
        ),
        TestExpr::Value(Value::List(List::from_values(vec![n(1), n(2), n(3)]))),
    ))
    .expect("map should evaluate");
    let binary_len = eval_closed_expr(&builtin1_expr(
        Builtin::ListLen,
        TestExpr::Value(Value::binary_from_text("World!")),
    ))
    .expect("binary len should evaluate");
    let list_len = eval_closed_expr(&builtin1_expr(
        Builtin::ListLen,
        TestExpr::Value(Value::List(List::concat(
            List::from_values(vec![n(1), n(2)]),
            List::from_bytes(Bytes::from_static(b"Hi")),
        ))),
    ))
    .expect("list len should evaluate");

    assert_eq!(slice, Value::binary_from_text("orl"));
    let Value::List(mapped) = mapped else {
        panic!("map should produce a list");
    };
    let items = list_to_value_items(&test_context(), &mapped)
        .expect("mapped list should be readable")
        .iter()
        .map(|value| eval_value(&test_context(), value))
        .collect::<Result<Vec<_>, _>>()
        .expect("mapped values should evaluate");
    assert_eq!(items, vec![n(2), n(3), n(4)]);
    assert_eq!(binary_len, n(6));
    assert_eq!(list_len, n(4));
}

#[test]
fn map_defers_concat_children_and_callable_until_the_selected_item_is_observed() {
    let context = test_context();
    let callable_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&callable_demands);
    let callable = Value::semantic_thunk(context.values(), "deferred map callable", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Builtin(Builtin::Floor))
    });
    let source = Value::List(List::concat(
        List::from_thunk(LazyValue::error(context.values(), "map forced its unused prefix").into()),
        List::from_values(vec![n(7)]),
    ));
    let mapped = apply_values(
        &context,
        Value::Builtin(Builtin::Map),
        vec![callable, source],
    )
    .and_then(|value| eval_value(&context, &value))
    .expect("map should expose one transformed spine node");
    assert_eq!(callable_demands.load(Ordering::SeqCst), 0);

    let Value::List(mapped) = mapped else {
        panic!("map should produce a list")
    };
    let stats = mapped.visit_logical_parts(&mut |_| {});
    assert_eq!(stats.thunk_items, 2);
    assert_eq!(stats.value_items, 0);

    let split = apply_values(
        &context,
        Value::Builtin(Builtin::ListSplitEnd),
        vec![n(1), Value::List(mapped)],
    )
    .and_then(|value| eval_value(&context, &value))
    .expect("the mapped suffix should remain observable from the back");
    let Value::Dict(split) = split else {
        panic!("split_end should produce a dictionary")
    };
    let Value::List(suffix) = split
        .get(&Key::atom_from_text("right"))
        .expect("split_end should retain its mapped suffix")
    else {
        panic!("split_end suffix should be a list")
    };
    assert_eq!(
        list_output_bytes(&context, suffix).expect("the selected item should evaluate"),
        [7]
    );
    assert_eq!(callable_demands.load(Ordering::SeqCst), 1);
}

#[test]
fn map_delays_a_non_callable_failure_until_an_item_is_observed() {
    let context = test_context();
    let mapped = apply_values(
        &context,
        Value::Builtin(Builtin::Map),
        vec![n(99), Value::List(List::from_values(vec![n(1)]))],
    )
    .and_then(|value| eval_value(&context, &value))
    .expect("map must not validate its callable eagerly");
    let Value::List(mapped) = mapped else {
        panic!("map should produce a list")
    };
    let error = list_output_bytes(&context, &mapped)
        .expect_err("observing the mapped item should reject the non-callable value");
    assert!(
        error
            .to_string()
            .contains("application requires a function value"),
        "{error}"
    );
}

#[test]
fn map_resumes_after_its_source_becomes_available_without_forcing_the_callable() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (source, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("map source"))
        .expect("the owner should allocate a promised map source");
    let callable_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&callable_demands);
    let callable = Value::semantic_thunk(observer.values(), "map callable", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Builtin(Builtin::Floor))
    });
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::Map),
        vec![callable, Value::Promised(source.clone())],
    )
    .expect("map application should build");

    let blocked =
        eval_value(&observer, &application).expect_err("the unresolved source should suspend map");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(callable_demands.load(Ordering::SeqCst), 0);

    set_promise(&owner, &source, Value::List(List::from_values(vec![n(8)])))
        .expect("the owner should resolve the map source");
    let Value::List(mapped) = eval_value(&observer, &application).expect("map should resume")
    else {
        panic!("map should produce a list")
    };
    assert_eq!(callable_demands.load(Ordering::SeqCst), 0);
    assert_eq!(
        list_output_bytes(&observer, &mapped).expect("the mapped value should evaluate"),
        [8]
    );
    assert_eq!(callable_demands.load(Ordering::SeqCst), 1);
}

#[test]
fn evaluates_zero_based_list_at_for_lists_and_compact_binaries() {
    let binary_item = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(n(1)),
        TestExpr::Value(Value::binary_from_text("ABC")),
    ))
    .expect("list at should index compact binary data");
    let mixed_item = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(n(2)),
        TestExpr::Value(Value::List(List::concat(
            List::from_values(vec![n(10)]),
            List::from_bytes(Bytes::from_static(b"AB")),
        ))),
    ))
    .expect("list at should index mixed list segments");

    assert_eq!(binary_item, n(i64::from(b'B')));
    assert_eq!(mixed_item, n(i64::from(b'B')));

    let out_of_bounds = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(n(3)),
        TestExpr::Value(Value::binary_from_text("ABC")),
    ))
    .expect_err("list at should reject an index at the list length");
    assert_eq!(
        out_of_bounds.to_string(),
        "list at builtin index is out of bounds"
    );

    let negative = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(n(-1)),
        TestExpr::Value(Value::binary_from_text("ABC")),
    ))
    .expect_err("list at should reject negative indices");
    assert_eq!(
        negative.to_string(),
        "list at builtin requires non-negative integer indices"
    );
}

#[test]
fn compiler_pattern_list_predicates_return_pass_fail_effects() {
    for value in [
        Value::binary_from_text(""),
        Value::binary_from_text("A"),
        Value::List(List::empty()),
        Value::List(List::from_values(vec![n(1)])),
        Value::List(List::from_thunk(
            LazyValue::error(
                &crate::core::test_value_factory(),
                "list predicate forced its contents",
            )
            .into(),
        )),
    ] {
        assert_eq!(
            run_pattern_builtin(Builtin::PatternIsList, value)
                .expect("logical list values should match"),
            [unit_value()]
        );
    }
    assert!(
        run_pattern_builtin(Builtin::PatternIsList, n(1))
            .expect("a kind mismatch should be an ordinary failure")
            .is_empty()
    );

    for value in [Value::binary_from_text(""), Value::List(List::empty())] {
        assert_eq!(
            run_pattern_builtin(Builtin::PatternListIsEmpty, value)
                .expect("empty logical lists should match"),
            [unit_value()]
        );
    }
    for value in [
        Value::binary_from_text("A"),
        Value::List(List::from_values(vec![n(1)])),
        n(1),
    ] {
        assert!(
            run_pattern_builtin(Builtin::PatternListIsEmpty, value)
                .expect("nonempty and wrong-kind values should mismatch")
                .is_empty()
        );
    }
}

#[test]
fn compiler_pattern_equality_mismatches_incompatible_values() {
    let atom = key_value(&Key::atom_from_text("tag"));
    for (expected, actual) in [
        (unit_value(), unit_value()),
        (n(42), n(42)),
        (atom.clone(), atom),
        (
            Value::binary_from_text("AB"),
            Value::List(List::from_values(vec![n(65), n(66)])),
        ),
    ] {
        assert_eq!(
            run_pattern_equal(expected, actual).expect("matching literals should succeed"),
            [unit_value()]
        );
    }

    for (expected, actual) in [
        (n(42), Value::binary_from_text("42")),
        (Value::binary_from_text("AB"), n(42)),
        (
            Value::binary_from_text("AB"),
            Value::List(List::from_values(vec![n(65), Value::Builtin(Builtin::Add)])),
        ),
    ] {
        assert!(
            run_pattern_equal(expected, actual)
                .expect("literal kind and value mismatches should be ordinary failure")
                .is_empty()
        );
    }
    assert_eq!(
        run_pattern_equal(
            n(1),
            Value::error(&crate::core::test_value_factory(), "literal input failed")
        )
        .expect_err("forcing failures must propagate")
        .to_string(),
        "literal input failed"
    );
}

#[test]
fn compiler_pattern_binary_list_equality_resumes_without_replaying_the_literal() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (item, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("pattern equality list item"))
        .expect("the owner should allocate a promised list item");
    let literal_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&literal_demands);
    let expected = Value::semantic_thunk(
        observer.values(),
        "instrumented pattern literal",
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::binary_from_text("A"))
        },
    );
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::PatternEqual),
        vec![
            expected,
            Value::List(List::from_values(vec![Value::Promised(item.clone())])),
        ],
    )
    .expect("pattern equality application should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated pattern equality builtin should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved list item should suspend pattern equality");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(literal_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the pattern-equality checkpoint must trace its literal and list state");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact promised list item");
    assert_eq!(literal_demands.load(Ordering::SeqCst), 1);
    set_promise(&owner, &item, n(i64::from(b'A')))
        .expect("the owner should resolve the promised list item");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned pattern-equality checkpoint must remain live");

    let effect = eval_value(&observer, &application)
        .expect("pattern equality should resume from the list item");
    let handled = apply_values(&observer, Value::Builtin(Builtin::ListEffect), vec![effect])
        .and_then(|value| eval_value(&observer, &value))
        .expect("the resumed pattern effect should be handled");
    let Value::List(results) = handled else {
        panic!("the list effect handler should return a list")
    };
    assert_eq!(
        list_to_value_items(&observer, &results).expect("the pattern result should be readable"),
        [unit_value()]
    );
    assert_eq!(literal_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

#[test]
fn compiler_pattern_path_equality_matches_keyable_lists_directionally() {
    let foo = key_value(&Key::atom_from_text("foo"));
    let expected = Value::List(List::from_values(vec![foo.clone(), n(42)]));
    assert_eq!(
        run_pattern_builtin2(
            Builtin::PatternPathEqual,
            expected.clone(),
            Value::List(List::from_values(vec![foo.clone(), n(42)])),
        )
        .expect("equal computed paths should match"),
        [unit_value()]
    );
    for actual in [
        Value::List(List::from_values(vec![foo, n(43)])),
        Value::List(List::from_values(vec![Value::Builtin(Builtin::Add)])),
        n(42),
    ] {
        assert!(
            run_pattern_builtin2(Builtin::PatternPathEqual, expected.clone(), actual)
                .expect("a different or non-keyable subject path should mismatch")
                .is_empty()
        );
    }
    assert_eq!(
        run_pattern_builtin2(
            Builtin::PatternPathEqual,
            Value::List(List::from_values(vec![Value::Builtin(Builtin::Add)])),
            Value::List(List::empty()),
        )
        .expect_err("an invalid computed expected path should remain an error")
        .to_string(),
        "dictionary keys must evaluate to keyable values"
    );
    assert_eq!(
        run_pattern_builtin2(
            Builtin::PatternPathEqual,
            expected,
            Value::List(List::from_values(vec![Value::error(
                &crate::core::test_value_factory(),
                "quoted path value failed"
            )])),
        )
        .expect_err("forcing failures in the subject path must propagate")
        .to_string(),
        "quoted path value failed"
    );
}

#[test]
fn compiler_pattern_path_equality_resumes_without_replaying_the_expected_path() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (actual_item, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("pattern path subject item"))
        .expect("the owner should allocate a promised path item");
    let expected_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&expected_demands);
    let expected_item = Value::semantic_thunk(
        observer.values(),
        "instrumented expected path item",
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(n(42))
        },
    );
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::PatternPathEqual),
        vec![
            Value::List(List::from_values(vec![expected_item])),
            Value::List(List::from_values(vec![Value::Promised(
                actual_item.clone(),
            )])),
        ],
    )
    .expect("pattern path equality application should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated pattern path builtin should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved subject item should suspend path equality");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(expected_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the path-pattern checkpoint must trace its completed expected path");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact promised subject item");
    assert_eq!(expected_demands.load(Ordering::SeqCst), 1);
    set_promise(&owner, &actual_item, n(42))
        .expect("the owner should resolve the promised subject item");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned path-pattern checkpoint must remain live");

    let effect = eval_value(&observer, &application)
        .expect("path equality should resume from the subject item");
    let handled = apply_values(&observer, Value::Builtin(Builtin::ListEffect), vec![effect])
        .and_then(|value| eval_value(&observer, &value))
        .expect("the resumed pattern effect should be handled");
    let Value::List(results) = handled else {
        panic!("the list effect handler should return a list")
    };
    assert_eq!(
        list_to_value_items(&observer, &results).expect("the pattern result should be readable"),
        [unit_value()]
    );
    assert_eq!(expected_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

#[test]
fn compiler_pattern_dictionary_operations_preserve_remainders() {
    let foo = Key::atom_from_text("foo");
    let bar = Key::atom_from_text("bar");
    let keep = Key::atom_from_text("keep");
    let other = Key::atom_from_text("other");
    let child = Dict::new_sync()
        .insert(bar.clone(), n(7))
        .insert(keep.clone(), n(8));
    let dict = Dict::new_sync()
        .insert(foo.clone(), Value::Dict(child))
        .insert(other.clone(), n(9));
    let path = Value::List(List::from_values(vec![key_value(&foo), key_value(&bar)]));

    let [parts]: [Value; 1] =
        run_pattern_builtin2(Builtin::PatternDictTryTake, path, Value::Dict(dict))
            .expect("a present static dictionary path should match")
            .try_into()
            .expect("successful extraction should return one parts value");
    let Value::Dict(parts) = parts else {
        panic!("dictionary extraction should return a parts dictionary");
    };
    assert_eq!(parts.get(&*keys::VALUE), Some(&n(7)));
    let Some(Value::Dict(rest)) = parts.get(&*keys::REST) else {
        panic!("dictionary extraction should retain a dictionary remainder");
    };
    assert_eq!(rest.get(&other), Some(&n(9)));
    let Some(Value::Dict(child)) = rest.get(&foo) else {
        panic!("the nonempty nested remainder should retain its parent path");
    };
    assert_eq!(child.get(&keep), Some(&n(8)));
    assert!(!child.contains_key(&bar));
}

#[test]
fn compiler_pattern_dictionary_take_resumes_without_replaying_a_completed_prefix() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (leaf, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("pattern dictionary leaf"))
        .expect("the owner should allocate a promised dictionary leaf");
    let prefix_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&prefix_demands);
    let foo = Key::atom_from_text("foo");
    let bar = Key::atom_from_text("bar");
    let promised_leaf = leaf.clone();
    let child = Value::semantic_thunk(
        observer.values(),
        "instrumented pattern dictionary prefix",
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Dict(
                Dict::new_sync().insert(bar.clone(), Value::Promised(leaf.clone())),
            ))
        },
    );
    let path = Value::List(List::from_values(vec![
        key_value(&foo),
        key_value(&Key::atom_from_text("bar")),
    ]));
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::PatternDictTryTake),
        vec![path, Value::Dict(Dict::new_sync().insert(foo, child))],
    )
    .expect("dictionary extraction application should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated dictionary extraction builtin should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved leaf should suspend dictionary extraction");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(prefix_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("dictionary extraction must trace its completed path prefix and frames");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact promised dictionary leaf");
    assert_eq!(prefix_demands.load(Ordering::SeqCst), 1);
    set_promise(&owner, &promised_leaf, n(7)).expect("the owner should resolve the leaf");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned dictionary extraction checkpoint must remain live");

    let effect = eval_value(&observer, &application)
        .expect("dictionary extraction should resume from the leaf");
    let handled = apply_values(&observer, Value::Builtin(Builtin::ListEffect), vec![effect])
        .and_then(|value| eval_value(&observer, &value))
        .expect("the resumed pattern effect should be handled");
    let Value::List(results) = handled else {
        panic!("the list effect handler should return a list")
    };
    let [parts]: [Value; 1] = list_to_value_items(&observer, &results)
        .expect("the pattern result should be readable")
        .try_into()
        .expect("successful extraction should return one parts value");
    let Value::Dict(parts) = parts else {
        panic!("dictionary extraction should return a parts dictionary")
    };
    assert_eq!(parts.get(&*keys::VALUE), Some(&n(7)));
    assert_eq!(prefix_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

#[test]
fn compiler_pattern_dictionary_mismatches_are_pass_fail() {
    let key = Key::atom_from_text("key");
    let path = Value::List(List::from_values(vec![key_value(&key)]));
    for value in [
        Value::Dict(Dict::new_sync()),
        Value::Dict(Dict::new_sync().insert(key.clone(), Value::Dict(Dict::new_sync()))),
        n(1),
    ] {
        assert!(
            run_pattern_builtin2(Builtin::PatternDictTryTake, path.clone(), value)
                .expect("missing, undefined, and wrong-kind paths should mismatch")
                .is_empty()
        );
    }

    assert_eq!(
        run_pattern_builtin(Builtin::PatternIsDict, Value::Dict(Dict::new_sync()))
            .expect("dictionary values should pass the kind check"),
        [unit_value()]
    );
    assert!(
        run_pattern_builtin(Builtin::PatternIsDict, n(1))
            .expect("wrong-kind values should mismatch")
            .is_empty()
    );

    let logically_empty = Dict::new_sync().insert(
        key,
        Value::Lazy(LazyValue::semantic_thunk(
            &crate::core::test_value_factory(),
            "empty dictionary field",
            |_| Ok(Value::Dict(Dict::new_sync())),
        )),
    );
    assert_eq!(
        run_pattern_builtin(Builtin::PatternDictIsEmpty, Value::Dict(logically_empty))
            .expect("nested and deferred undefined values should be logically empty"),
        [unit_value()]
    );
    assert!(
        run_pattern_builtin(
            Builtin::PatternDictIsEmpty,
            Value::Dict(Dict::new_sync().insert(Key::atom_from_text("present"), n(1))),
        )
        .expect("a present dictionary value should mismatch empty")
        .is_empty()
    );
    assert_eq!(
        run_pattern_builtin(
            Builtin::PatternDictIsEmpty,
            Value::Dict(Dict::new_sync().insert(
                Key::atom_from_text("broken"),
                Value::error(&crate::core::test_value_factory(), "dict value failed")
            ),),
        )
        .expect_err("forcing failures while establishing emptiness must propagate")
        .to_string(),
        "dict value failed"
    );
}

#[test]
fn compiler_pattern_dictionary_emptiness_resumes_without_replaying_prior_members() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (second, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("pattern dictionary second member"))
        .expect("the owner should allocate a promised dictionary member");
    let first_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&first_demands);
    let first = Value::semantic_thunk(
        observer.values(),
        "instrumented first undefined member",
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Dict(Dict::new_sync()))
        },
    );
    let source = Value::Dict(
        Dict::new_sync()
            .insert(Key::Number(0.into()), first)
            .insert(Key::Number(1.into()), Value::Promised(second.clone())),
    );
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::PatternDictIsEmpty),
        vec![source],
    )
    .expect("dictionary emptiness application should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated dictionary-emptiness builtin should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved second member should suspend emptiness traversal");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(first_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the dictionary-emptiness checkpoint must trace completed members");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact promised member");
    assert_eq!(first_demands.load(Ordering::SeqCst), 1);
    set_promise(&owner, &second, Value::Dict(Dict::new_sync()))
        .expect("the owner should resolve the second dictionary member");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned dictionary-emptiness checkpoint must remain live");

    let effect = eval_value(&observer, &application)
        .expect("dictionary emptiness should resume from the second member");
    let handled = apply_values(&observer, Value::Builtin(Builtin::ListEffect), vec![effect])
        .and_then(|value| eval_value(&observer, &value))
        .expect("the resumed pattern effect should be handled");
    let Value::List(results) = handled else {
        panic!("the list effect handler should return a list")
    };
    assert_eq!(
        list_to_value_items(&observer, &results).expect("the pattern result should be readable"),
        [unit_value()]
    );
    assert_eq!(first_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

#[test]
fn compiler_pattern_optional_dictionary_operations_preserve_absence_and_errors() {
    let foo = Key::atom_from_text("foo");
    let bar = Key::atom_from_text("bar");
    let keep = Key::atom_from_text("keep");
    let path = Value::List(List::from_values(vec![key_value(&foo), key_value(&bar)]));

    let absent = Dict::new_sync().insert(
        foo.clone(),
        Value::Dict(Dict::new_sync().insert(keep.clone(), n(8))),
    );
    let [parts]: [Value; 1] = run_pattern_builtin2(
        Builtin::PatternDictTryTakeOptional,
        path.clone(),
        Value::Dict(absent.clone()),
    )
    .expect("an optional absent path should succeed")
    .try_into()
    .expect("optional extraction should return one parts value");
    let Value::Dict(parts) = parts else {
        panic!("optional dictionary extraction should return a parts dictionary");
    };
    assert_eq!(
        parts.get(&*keys::VALUE),
        Some(&Value::Dict(Dict::new_sync()))
    );
    assert_eq!(parts.get(&*keys::REST), Some(&Value::Dict(absent)));

    let present = Dict::new_sync().insert(
        foo.clone(),
        Value::Dict(
            Dict::new_sync()
                .insert(bar.clone(), n(7))
                .insert(keep, n(8)),
        ),
    );
    let [parts]: [Value; 1] = run_pattern_builtin2(
        Builtin::PatternDictTryTakeOptional,
        path.clone(),
        Value::Dict(present),
    )
    .expect("an optional present path should extract normally")
    .try_into()
    .expect("optional extraction should return one parts value");
    let Value::Dict(parts) = parts else {
        panic!("optional dictionary extraction should return a parts dictionary");
    };
    assert_eq!(parts.get(&*keys::VALUE), Some(&n(7)));

    let wrong_intermediate = Value::Dict(Dict::new_sync().insert(foo.clone(), n(1)));
    assert!(
        run_pattern_builtin2(
            Builtin::PatternDictTryTakeOptional,
            path.clone(),
            wrong_intermediate,
        )
        .expect("a non-dictionary path prefix should mismatch")
        .is_empty()
    );

    let failed = Value::Dict(Dict::new_sync().insert(
        foo,
        Value::error(
            &crate::core::test_value_factory(),
            "optional dict path failed",
        ),
    ));
    assert_eq!(
        run_pattern_builtin2(Builtin::PatternDictTryTakeOptional, path, failed)
            .expect_err("forcing failures along an optional path must propagate")
            .to_string(),
        "optional dict path failed"
    );
}

#[test]
fn compiler_pattern_dictionary_take_rejects_an_empty_compiler_path() {
    assert_eq!(
        run_pattern_builtin2(
            Builtin::PatternDictTryTake,
            Value::List(List::empty()),
            Value::Dict(Dict::new_sync()),
        )
        .expect_err("an empty compiler path must remain an evaluation error")
        .to_string(),
        "pattern-dict-try-take received an empty compiler path"
    );
}

#[test]
fn compiler_pattern_list_decomposition_preserves_compact_remainders() {
    let [uncons]: [Value; 1] =
        run_pattern_builtin(Builtin::PatternListTryUncons, Value::binary_from_text("AB"))
            .expect("a compact binary should uncons")
            .try_into()
            .expect("a successful match should have one result");
    let Value::Dict(uncons) = uncons else {
        panic!("uncons should return a parts dictionary");
    };
    assert_eq!(uncons.get(&*keys::HEAD), Some(&n(i64::from(b'A'))));
    assert_eq!(
        uncons.get(&*keys::TAIL),
        Some(&Value::binary_from_text("B"))
    );

    let [uncons]: [Value; 1] = run_pattern_builtin(
        Builtin::PatternListTryUncons,
        Value::List(List::from_values(vec![n(1), n(2)])),
    )
    .expect("a flat value list should uncons")
    .try_into()
    .expect("a successful match should have one result");
    let Value::Dict(uncons) = uncons else {
        panic!("uncons should return a parts dictionary");
    };
    assert_eq!(uncons.get(&*keys::HEAD), Some(&n(1)));
    let Some(Value::List(tail)) = uncons.get(&*keys::TAIL) else {
        panic!("a value-list remainder should stay a list");
    };
    assert_eq!(
        pop_list_front(&test_context(), tail)
            .expect("flat tail should be readable")
            .map(|(head, _)| head),
        Some(n(2))
    );

    let [unsnoc]: [Value; 1] =
        run_pattern_builtin(Builtin::PatternListTryUnsnoc, Value::binary_from_text("AB"))
            .expect("a compact binary should unsnoc")
            .try_into()
            .expect("a successful match should have one result");
    let Value::Dict(unsnoc) = unsnoc else {
        panic!("unsnoc should return a parts dictionary");
    };
    assert_eq!(
        unsnoc.get(&*keys::INIT),
        Some(&Value::binary_from_text("A"))
    );
    assert_eq!(unsnoc.get(&*keys::LAST), Some(&n(i64::from(b'B'))));
}

#[test]
fn compiler_pattern_list_decomposition_mismatches_without_masking_failures() {
    for builtin in [Builtin::PatternListTryUncons, Builtin::PatternListTryUnsnoc] {
        for value in [
            Value::binary_from_text(""),
            Value::List(List::empty()),
            n(1),
        ] {
            assert!(
                run_pattern_builtin(builtin, value)
                    .expect("empty and wrong-kind values should mismatch")
                    .is_empty()
            );
        }
        assert_eq!(
            run_pattern_builtin(
                builtin,
                Value::error(&crate::core::test_value_factory(), "pattern input failed")
            )
            .expect_err("forcing failures must propagate")
            .to_string(),
            "pattern input failed"
        );
    }
}

#[test]
fn compiler_pattern_unsnoc_does_not_force_an_unrelated_prefix_hole() {
    let list = List::concat(
        List::from_thunk(
            LazyValue::error(&crate::core::test_value_factory(), "prefix was forced").into(),
        ),
        List::from_values(vec![n(9)]),
    );
    let [parts]: [Value; 1] = run_pattern_builtin(Builtin::PatternListTryUnsnoc, Value::List(list))
        .expect("a known suffix should unsnoc without its prefix")
        .try_into()
        .expect("a successful match should have one result");
    let Value::Dict(parts) = parts else {
        panic!("unsnoc should return a parts dictionary");
    };
    assert_eq!(parts.get(&*keys::LAST), Some(&n(9)));
    assert!(matches!(parts.get(&*keys::INIT), Some(Value::List(_))));
}

#[test]
fn compiler_pattern_unsnoc_resumes_a_promised_suffix_without_forcing_its_prefix() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (tail, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("pattern unsnoc tail"))
        .expect("the owner should allocate a promised list tail");
    let source = Value::List(List::concat(
        List::from_thunk(
            LazyValue::error(observer.values(), "pattern unsnoc forced its prefix").into(),
        ),
        List::from_thunk(tail.clone().into()),
    ));
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::PatternListTryUnsnoc),
        vec![source],
    )
    .expect("pattern-unsnoc application should build");

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved suffix should suspend pattern unsnoc");
    assert!(blocked.blocked_on().is_some());
    set_promise(&owner, &tail, Value::List(List::from_values(vec![n(9)])))
        .expect("the owner should resolve the promised suffix");

    let effect = eval_value(&observer, &application)
        .expect("pattern unsnoc should resume without observing its prefix");
    let handled = apply_values(&observer, Value::Builtin(Builtin::ListEffect), vec![effect])
        .and_then(|value| eval_value(&observer, &value))
        .expect("the resumed pattern effect should be handled");
    let Value::List(results) = handled else {
        panic!("the list effect handler should return a list")
    };
    let [parts]: [Value; 1] = list_to_value_items(&observer, &results)
        .expect("the pattern result should be readable")
        .try_into()
        .expect("a successful match should have one result");
    let Value::Dict(parts) = parts else {
        panic!("unsnoc should return a parts dictionary")
    };
    assert_eq!(parts.get(&*keys::LAST), Some(&n(9)));
}

#[test]
fn text_lines_preserves_empty_and_trailing_lines() {
    let lines = eval_closed_expr(&builtin1_expr(
        Builtin::TextLines,
        TestExpr::Value(Value::binary_from_text("first\n\nthird\n")),
    ))
    .expect("text lines should split compact binary text");
    let Value::List(lines) = lines else {
        panic!("text lines should produce a list");
    };

    assert_eq!(
        list_to_value_items(&test_context(), &lines).expect("line list should be readable"),
        vec![
            Value::binary_from_text("first"),
            Value::binary_from_text(""),
            Value::binary_from_text("third"),
            Value::binary_from_text(""),
        ]
    );
}

#[test]
fn text_lines_resumes_a_promised_item_without_replaying_its_prefix() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (item, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("text-lines item"))
        .expect("the owner should allocate a promised text byte");
    let prefix_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&prefix_demands);
    let prefix = Value::semantic_thunk(observer.values(), "text-lines prefix", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(n(b'a' as i64))
    });
    let source = Value::List(List::from_values(vec![
        prefix,
        Value::Promised(item.clone()),
        n(b'\n' as i64),
        n(b'c' as i64),
    ]));
    let application = apply_values(&observer, Value::Builtin(Builtin::TextLines), vec![source])
        .expect("text-lines application should build");

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved byte should suspend text lines");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(prefix_demands.load(Ordering::SeqCst), 1);

    set_promise(&owner, &item, n(b'b' as i64)).expect("the owner should resolve the promised byte");
    let Value::List(lines) = eval_value(&observer, &application).expect("text lines should resume")
    else {
        panic!("text lines should produce a list")
    };
    assert_eq!(
        list_to_value_items(&observer, &lines).expect("line list should be readable"),
        vec![Value::binary_from_text("ab"), Value::binary_from_text("c")]
    );
    assert_eq!(
        prefix_demands.load(Ordering::SeqCst),
        1,
        "resumption must retain the completed prefix byte"
    );
}

#[test]
fn text_lines_resumes_a_promised_list_chunk() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (tail, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("text-lines list tail"))
        .expect("the owner should allocate a promised list tail");
    let source = Value::List(List::concat(
        List::from_bytes(Bytes::from_static(b"a")),
        List::from_thunk(tail.clone().into()),
    ));
    let application = apply_values(&observer, Value::Builtin(Builtin::TextLines), vec![source])
        .expect("text-lines application should build");

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved list chunk should suspend text lines");
    assert!(blocked.blocked_on().is_some());
    set_promise(
        &owner,
        &tail,
        Value::List(List::from_bytes(Bytes::from_static(b"b\nc"))),
    )
    .expect("the owner should resolve the promised list tail");

    let Value::List(lines) = eval_value(&observer, &application).expect("text lines should resume")
    else {
        panic!("text lines should produce a list")
    };
    assert_eq!(
        list_to_value_items(&observer, &lines).expect("line list should be readable"),
        vec![Value::binary_from_text("ab"), Value::binary_from_text("c")]
    );
}

#[test]
fn evaluates_split_and_split_end_builtins() {
    let split = eval_closed_expr(&builtin2_expr(
        Builtin::ListSplit,
        TestExpr::Value(n(2)),
        TestExpr::Value(Value::binary_from_text("Hello")),
    ))
    .expect("split should evaluate");
    let split_end = eval_closed_expr(&builtin2_expr(
        Builtin::ListSplitEnd,
        TestExpr::Value(n(2)),
        TestExpr::Value(Value::List(List::concat(
            List::from_values(vec![n(1), n(2)]),
            List::from_bytes(Bytes::from_static(b"abc")),
        ))),
    ))
    .expect("split_end should evaluate");

    let Value::Dict(split) = split else {
        panic!("split should return a dictionary");
    };
    assert_eq!(
        split.get(&Key::atom_from_text("left")),
        Some(&Value::binary_from_text("He"))
    );
    assert_eq!(
        split.get(&Key::atom_from_text("right")),
        Some(&Value::binary_from_text("llo"))
    );

    let Value::Dict(split_end) = split_end else {
        panic!("split_end should return a dictionary");
    };
    let Value::List(prefix) = split_end
        .get(&Key::atom_from_text("left"))
        .expect("split_end should include left")
    else {
        panic!("split_end left should be a list");
    };
    let Value::List(suffix) = split_end
        .get(&Key::atom_from_text("right"))
        .expect("split_end should include right")
    else {
        panic!("split_end right should be a list");
    };

    assert_eq!(
        list_to_value_items(&test_context(), prefix).expect("prefix should be readable"),
        vec![n(1), n(2), Value::Number(Number::from_u8(b'a'))]
    );
    assert_eq!(
        list_to_value_items(&test_context(), suffix).expect("suffix should be readable"),
        vec![
            Value::Number(Number::from_u8(b'b')),
            Value::Number(Number::from_u8(b'c'))
        ]
    );
}

#[test]
fn slice_builtin_shares_binary_storage() {
    let bytes = Bytes::from_static(b"Hello");
    let slice = eval_closed_expr(&builtin3_expr(
        Builtin::Slice,
        TestExpr::Value(n(1)),
        TestExpr::Value(n(4)),
        TestExpr::Value(Value::Binary(bytes.clone())),
    ))
    .expect("slice should evaluate");

    let Value::Binary(slice) = slice else {
        panic!("binary slice should remain binary");
    };
    assert_eq!(&slice[..], b"ell");
    assert_eq!(slice.as_ptr(), bytes[1..].as_ptr());
}

#[test]
fn evaluates_function_net_application_lazily() {
    let expr = TestExpr::Apply(
        Arc::new(function_expr(1, TestExpr::Local(0))),
        Arc::new(builtin2_expr(
            Builtin::Add,
            TestExpr::Value(n(1)),
            TestExpr::Value(n(2)),
        )),
    );

    let value = eval_closed_expr(&expr).expect("lambda application should evaluate");

    assert_eq!(value, n(3));
}

#[test]
fn function_nets_capture_outer_values() {
    let invoke = function_expr(
        1,
        TestExpr::Apply(
            Arc::new(TestExpr::Local(0)),
            Arc::new(TestExpr::Value(n(0))),
        ),
    );
    let returns_outer = function_expr(1, TestExpr::Local(1));
    let outer = function_expr(
        1,
        TestExpr::Apply(Arc::new(invoke), Arc::new(returns_outer)),
    );
    let value = eval_closed_expr(&TestExpr::Apply(
        Arc::new(outer),
        Arc::new(TestExpr::Value(n(42))),
    ))
    .expect("nested functions should evaluate");

    assert_eq!(value, n(42));
}

#[test]
fn partial_builtins_share_lazy_arguments() {
    let force_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = force_count.clone();
    let argument = TestExpr::Value(Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "partial argument",
        move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(40))
        },
    ));
    let make_partial = function_expr(
        1,
        TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(Builtin::Add))),
            Arc::new(TestExpr::Local(0)),
        ),
    );
    let partial = eval_closed_expr(&TestExpr::Apply(Arc::new(make_partial), Arc::new(argument)))
        .expect("a partial builtin should retain its argument lazily");

    assert!(matches!(partial, Value::PartialBuiltin(_)));
    assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        eval_value(
            &test_context(),
            &apply_value(&test_context(), partial.clone(), n(2)).unwrap(),
        )
        .unwrap(),
        n(42)
    );
    assert_eq!(
        eval_value(
            &test_context(),
            &apply_value(&test_context(), partial, n(3)).unwrap(),
        )
        .unwrap(),
        n(43)
    );
    assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn net_list_literals_store_lazy_values_without_exporting_list_holes() {
    let force_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = force_count.clone();
    let expression = TestExpr::Apply(
        Arc::new(function_expr(
            1,
            TestExpr::List(Arc::from([Arc::new(TestExpr::Local(0))])),
        )),
        Arc::new(TestExpr::Value(Value::semantic_thunk(
            &crate::core::test_value_factory(),
            "list value",
            move |_| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(n(42))
            },
        ))),
    );
    let Value::List(list) = eval_closed_expr(&expression).unwrap() else {
        panic!("net-backed list literal should produce a list");
    };
    let Some((item, tail)) = list
        .try_pop_front(&mut |_| -> Result<_, EvaluationHalt> {
            panic!("embedded lazy value must not become a list hole")
        })
        .unwrap()
    else {
        panic!("net-backed list literal should contain its argument");
    };
    let ListItem::Value(item) = item else {
        panic!("lazy argument should remain an ordinary list value")
    };
    assert!(matches!(item, Value::Lazy(_)));
    assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(eval_value(&test_context(), &item).unwrap(), n(42));
    assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(pop_list_front(&test_context(), &tail).unwrap().is_none());
}

#[test]
fn closed_semantic_list_holes_remain_host_observable() {
    let Value::Lazy(hole) =
        Value::semantic_thunk(&crate::core::test_value_factory(), "list hole", |_| {
            Ok(Value::List(List::from_values(vec![n(42)])))
        })
    else {
        unreachable!()
    };
    let list = List::from_thunk(hole.into());

    let (value, tail) = pop_list_front(&test_context(), &list).unwrap().unwrap();
    assert_eq!(value, n(42));
    assert!(pop_list_front(&test_context(), &tail).unwrap().is_none());
}

#[test]
fn dropped_arguments_do_not_prevent_later_bindings_from_resolving() {
    let function = closed_function_value(2, TestExpr::Local(0));
    let value = eval_value(&test_context(), &apply_test_values(function, [n(1), n(42)]))
        .expect("function with dropped argument should evaluate");

    assert_eq!(value, n(42));
}

#[test]
fn method_objects_apply_via_apply_member() {
    let method = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("apply"),
        closed_function_value(
            1,
            builtin2_expr(Builtin::Add, TestExpr::Local(0), TestExpr::Value(n(1))),
        ),
    ));
    let value = eval_closed_expr(&TestExpr::Apply(
        Arc::new(TestExpr::Value(method)),
        Arc::new(TestExpr::Value(n(41))),
    ))
    .expect("method object application should evaluate");

    assert_eq!(value, n(42));
}

#[test]
fn effect_values_apply_by_extending_the_effect_function() {
    let effect = test_effect_value(closed_function_value(
        1,
        TestExpr::Access(
            Arc::new(TestExpr::Local(0)),
            Arc::from([TestKey::Key(Key::atom_from_text("op"))]),
        ),
    ));
    let applied = eval_closed_expr(&TestExpr::Apply(
        Arc::new(TestExpr::Value(effect)),
        Arc::new(TestExpr::Value(n(41))),
    ))
    .expect("effect application should evaluate");
    let Value::Dict(effect) = applied else {
        panic!("effect application should produce an effect value");
    };
    let function = effect
        .get(&Key::atom_from_text("eff"))
        .expect("effect should contain an eff function")
        .clone();
    let api = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("op"),
        closed_function_value(
            1,
            builtin2_expr(Builtin::Add, TestExpr::Local(0), TestExpr::Value(n(1))),
        ),
    ));

    let value = apply_value(
        &test_context(),
        eval_value(&test_context(), &function).unwrap(),
        api,
    )
    .and_then(|value| eval_value(&test_context(), &value))
    .expect("extended effect function should evaluate with an API");
    assert_eq!(value, n(42));
}

#[test]
fn effect_apply_resumes_from_its_exact_function_operand() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (function, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("effect apply function"))
        .expect("the owner should allocate a promised function");
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::EffectApply),
        vec![Value::Promised(function.clone()), n(2), n(40)],
    )
    .expect("effect application should build");

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved function should suspend effect application");
    assert!(blocked.blocked_on().is_some());

    set_promise(&owner, &function, Value::Builtin(Builtin::Add))
        .expect("the owner should resolve the promised function");
    assert_eq!(
        eval_value(&observer, &application).expect("effect application should resume"),
        n(42)
    );
}

#[test]
fn effect_call_finishes_its_argument_spine_before_observing_the_api() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (tail, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("effect call argument tail"))
        .expect("the owner should allocate a promised argument tail");
    let method_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&method_demands);
    let method = Value::semantic_thunk(observer.values(), "effect API method", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Builtin(Builtin::Add))
    });
    let prefix_demands = Arc::new(AtomicUsize::new(0));
    let observed_prefix = Arc::clone(&prefix_demands);
    let prefix = Value::semantic_thunk(observer.values(), "effect call prefix", move |_| {
        observed_prefix.fetch_add(1, Ordering::SeqCst);
        Ok(Value::List(List::from_values(vec![n(19)])))
    });
    let api = Value::Dict(Dict::new_sync().insert(Key::binary_from_text("add"), method));
    let arguments = Value::List(List::concat(
        List::from_thunk(
            match prefix {
                Value::Lazy(prefix) => prefix,
                _ => unreachable!("a semantic thunk must remain lazy"),
            }
            .into(),
        ),
        List::from_thunk(ListThunk::Promised(tail.clone())),
    ));
    let call = apply_values(
        &observer,
        Value::Builtin(Builtin::EffectCall),
        vec![Value::binary_from_text("add"), arguments, api],
    )
    .expect("effect call should build");
    let Value::Lazy(call_lazy) = &call else {
        panic!("a saturated effect call should remain lazy")
    };
    let call_root = call_lazy.root(observer.values());

    let blocked = eval_value(&observer, &call)
        .expect_err("the unresolved argument tail should suspend effect dispatch");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(method_demands.load(Ordering::SeqCst), 0);
    assert_eq!(prefix_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the effect-call checkpoint must trace its completed prefix");
    eval_value(&observer, &call).expect_err("a later route must resume the exact argument tail");
    assert_eq!(prefix_demands.load(Ordering::SeqCst), 1);

    set_promise(&owner, &tail, Value::List(List::from_values(vec![n(23)])))
        .expect("the owner should resolve the promised argument tail");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned effect-call checkpoint must remain live");
    assert_eq!(
        eval_value(&observer, &call).expect("effect dispatch should resume"),
        n(42)
    );
    assert_eq!(method_demands.load(Ordering::SeqCst), 1);
    assert_eq!(prefix_demands.load(Ordering::SeqCst), 1);
    drop(call_root);
}

#[test]
fn effect_map_finishes_its_list_front_before_observing_the_api() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (tail, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("effect map list tail"))
        .expect("the owner should allocate a promised map tail");
    let method_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&method_demands);
    let return_method = observer.values().with_runtime_value_access(|access| {
        let return_method = closed_function_value_in(observer.values(), 1, TestExpr::Local(0));
        access.root_runtime_value(return_method)
    });
    let return_method =
        Value::semantic_thunk(observer.values(), "effect API return", move |context| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(context.with_value_access(|access| access.clone_root(&return_method)))
        });
    let api = Value::Dict(Dict::new_sync().insert((*keys::R).clone(), return_method));
    let items = Value::List(List::from_thunk(ListThunk::Promised(tail.clone())));
    let operation = apply_values(
        &observer,
        Value::Builtin(Builtin::EffectMapRun),
        vec![n(0), items, Value::List(List::empty()), api],
    )
    .expect("effect map should build");
    let Value::Lazy(operation_lazy) = &operation else {
        panic!("a saturated effect-map run should remain lazy")
    };
    let operation_root = operation_lazy.root(observer.values());

    let blocked = eval_value(&observer, &operation)
        .expect_err("the unresolved list tail should suspend effect map");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(method_demands.load(Ordering::SeqCst), 0);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the effect-map checkpoint must trace its suspended list front");
    eval_value(&observer, &operation)
        .expect_err("a later route must resume the exact effect-map list front");
    assert_eq!(method_demands.load(Ordering::SeqCst), 0);

    set_promise(&owner, &tail, Value::List(List::empty()))
        .expect("the owner should resolve the promised map tail");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned effect-map checkpoint must remain live");
    assert_eq!(
        eval_value(&observer, &operation).expect("effect map should resume"),
        Value::List(List::empty())
    );
    assert_eq!(method_demands.load(Ordering::SeqCst), 1);
    drop(operation_root);
}

#[test]
fn effect_application_requires_singleton_eff_tag() {
    let not_singleton = Value::Dict(
        Dict::new_sync()
            .insert(
                Key::atom_from_text("eff"),
                closed_function_value(1, TestExpr::Local(0)),
            )
            .insert(Key::atom_from_text("extra"), n(1)),
    );
    let err = eval_closed_expr(&TestExpr::Apply(
        Arc::new(TestExpr::Value(not_singleton)),
        Arc::new(TestExpr::Value(n(42))),
    ))
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "application requires a function value, received Dict"
    );
}

#[test]
fn non_callable_application_reports_semantic_value_kinds() {
    for (value, expected) in [
        (
            Value::Dict(Dict::new_sync()),
            "application requires a function value, received Undefined",
        ),
        (
            unit_value(),
            "application requires a function value, received Unit",
        ),
        (
            n(42),
            "application requires a function value, received Number",
        ),
    ] {
        let application = apply_value(&test_context(), value, n(0))
            .expect("application construction should remain lazy");
        let error = eval_value(&test_context(), &application)
            .expect_err("applying a non-callable value should fail");
        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn tuple_ordering_requires_a_singleton_tuple_tag() {
    let left = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::TUPLE).clone(),
                Value::List(List::from_values(vec![n(1)])),
            )
            .insert(Key::atom_from_text("extra"), n(1)),
    );
    let right = Value::Dict(Dict::new_sync().insert(
        (*keys::TUPLE).clone(),
        Value::List(List::from_values(vec![n(2)])),
    ));

    let err = eval_closed_expr(&builtin2_expr(
        Builtin::Less,
        TestExpr::Value(left),
        TestExpr::Value(right),
    ))
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "less-than builtin can only order dictionaries tagged as `tuple`"
    );
}

#[test]
fn local_dictionary_paths_resolve_without_a_global_root() {
    let dict = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("tail"),
        Value::binary_from_text("World"),
    ));
    let expr = TestExpr::Apply(
        Arc::new(function_expr(
            1,
            TestExpr::Access(
                Arc::new(TestExpr::Local(0)),
                Arc::from([TestKey::Key(Key::atom_from_text("tail"))]),
            ),
        )),
        Arc::new(TestExpr::Value(dict)),
    );

    let value = eval_closed_expr(&expr).expect("local dictionary path should evaluate");

    assert_eq!(value, Value::binary_from_text("World"));
}

#[test]
fn divide_builtin_rejects_zero() {
    let expr = builtin2_expr(
        Builtin::Divide,
        TestExpr::Value(n(1)),
        TestExpr::Value(n(0)),
    );
    let err = eval_closed_expr(&expr).expect_err("division by zero should fail");
    assert_eq!(err.to_string(), "divide builtin cannot divide by zero");
}

#[test]
fn evaluates_keyable_values_into_keys() {
    let key = eval_key(&Value::List(List::concat(
        List::from_values(vec![n(1)]),
        List::from_bytes(Bytes::from_static(b"Hi")),
    )))
    .expect("list should evaluate to a key");

    assert_eq!(
        key,
        Key::List(Arc::from([
            k(1),
            Key::Number(Number::from_u8(b'H')),
            Key::Number(Number::from_u8(b'i')),
        ]))
    );
}

#[test]
fn evaluates_lazy_values_before_key_validation() {
    let key = eval_key(&fixture_computation(TestExpr::Value(n(1))))
        .expect("lazy values should be allowed when they evaluate to keyable values");

    assert_eq!(key, k(1));
}

#[test]
fn dictionaries_remain_lazy_under_eval_value() {
    let value = Value::Dict(crate::core::Dict::new_sync().insert(
        Key::atom_from_text("answer"),
        fixture_computation(TestExpr::Value(n(42))),
    ));

    let evaluated = eval_value(&test_context(), &value).expect("dict should stay lazy");

    assert_eq!(evaluated, value);
}

#[test]
fn missing_access_can_evaluate_to_an_undefined_key() {
    let root = Value::Dict(crate::core::Dict::new_sync());
    let key = eval_key(&apply_rooted_fixture(
        &root,
        global_access(vec![TestKey::Key(Key::atom_from_text("missing"))]),
    ))
    .expect("missing names should now resolve to empty dictionaries");

    assert_eq!(key, Key::Dict(Arc::from([])));
}

#[test]
fn raw_value_to_key_rejects_lazy_values() {
    assert_eq!(
        Key::from_value(&fixture_computation(TestExpr::Value(n(1)))),
        None
    );
}

#[test]
fn eval_key_forces_nested_dictionary_values() {
    let key = eval_key(&Value::Dict(crate::core::Dict::new_sync().insert(
        Key::atom_from_text("answer"),
        fixture_computation(TestExpr::Value(n(42))),
    )))
    .expect("dict key should force nested values");

    assert_eq!(
        key,
        Key::Dict(Arc::from([(Key::atom_from_text("answer"), k(42),)]))
    );
}

#[test]
fn eval_key_elides_empty_dictionary_values_from_dict_keys() {
    let empty = eval_key(&Value::Dict(crate::core::Dict::new_sync()))
        .expect("empty dict should be keyable");
    let with_empty_field = eval_key(&Value::Dict(crate::core::Dict::new_sync().insert(
        Key::atom_from_text("key"),
        Value::Dict(crate::core::Dict::new_sync()),
    )))
    .expect("dict with empty field should be keyable");

    assert_eq!(empty, Key::Dict(Arc::from([])));
    assert_eq!(with_empty_field, Key::Dict(Arc::from([])));
}

#[test]
fn singleton_dict_filters_empty_dictionary_values() {
    let value = eval_closed_expr(&singleton_expr(
        Value::Atom(crate::core::Atom::from_key(
            &crate::core::Key::binary_from_text("gone"),
        )),
        TestExpr::Value(Value::Dict(crate::core::Dict::new_sync())),
    ))
    .expect("singleton dict should evaluate");

    assert_eq!(value, Value::Dict(crate::core::Dict::new_sync()));
}

#[test]
fn dictionary_unions_merge_nested_dictionaries_transitively() {
    let key = Key::atom_from_text("greeting");
    let hello = Key::atom_from_text("hello");
    let world = Key::atom_from_text("world");

    let expr = dict_union_expr(
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(
                key.clone(),
                Value::Dict(
                    crate::core::Dict::new_sync()
                        .insert(hello.clone(), Value::binary_from_text("Hello")),
                ),
            ),
        )),
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(
                key.clone(),
                Value::Dict(
                    crate::core::Dict::new_sync()
                        .insert(world.clone(), Value::binary_from_text("World")),
                ),
            ),
        )),
    );

    let value = eval_closed_expr(&expr).expect("dict union should evaluate");
    let greeting = value.get_key_path(&[key]).expect("greeting should exist");
    let Value::Lazy(greeting) = greeting else {
        panic!("greeting should stay lazy until demanded");
    };
    let greeting = eval_value(&test_context(), &Value::Lazy(greeting.clone()))
        .expect("nested dict union should evaluate when demanded");
    let Value::Dict(greeting) = greeting else {
        panic!("greeting should evaluate to a merged dictionary");
    };

    assert_eq!(
        greeting.get(&hello),
        Some(&Value::binary_from_text("Hello"))
    );
    assert_eq!(
        greeting.get(&world),
        Some(&Value::binary_from_text("World"))
    );
}

#[test]
fn dictionary_unions_treat_empty_dictionary_values_as_undefined() {
    let key = Key::atom_from_text("greeting");
    let expr = dict_union_expr(
        singleton_expr(
            Value::Atom(crate::core::Atom::from_key(
                &crate::core::Key::binary_from_text("greeting"),
            )),
            TestExpr::Value(Value::binary_from_text("Hello")),
        ),
        singleton_expr(
            Value::Atom(crate::core::Atom::from_key(
                &crate::core::Key::binary_from_text("greeting"),
            )),
            TestExpr::Value(Value::Dict(crate::core::Dict::new_sync())),
        ),
    );

    let value = eval_closed_expr(&expr).expect("dict union should evaluate");
    assert_eq!(
        value.get_key_path(&[key]),
        Some(&Value::binary_from_text("Hello"))
    );
}

#[test]
fn dictionary_union_resumes_without_replaying_a_completed_operand() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (right, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("dictionary union right operand"))
        .expect("the owner should allocate a promised operand");
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted_attempts = attempts.clone();
    let left_key = Key::atom_from_text("left");
    let left_key_for_thunk = left_key.clone();
    let left = LazyValue::semantic_thunk(
        observer.values(),
        "counted dictionary union left operand",
        move |_context| {
            counted_attempts.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Dict(
                Dict::new_sync().insert(left_key_for_thunk.clone(), n(41)),
            ))
        },
    );
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::DictUnion),
        vec![Value::Lazy(left), Value::Promised(right.clone())],
    )
    .expect("dictionary union application should build");

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved right operand should suspend dictionary union");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);

    let right_key = Key::atom_from_text("right");
    set_promise(
        &owner,
        &right,
        Value::Dict(Dict::new_sync().insert(right_key.clone(), n(42))),
    )
    .expect("the owner should resolve the right operand");
    let value = eval_value(&observer, &application)
        .expect("dictionary union should resume after promise assignment");
    let Value::Dict(dict) = value else {
        panic!("dictionary union should produce a dictionary");
    };
    assert_eq!(dict.get(&left_key), Some(&n(41)));
    assert_eq!(dict.get(&right_key), Some(&n(42)));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "resumption must retain the completed left operand"
    );
}

#[test]
fn dictionary_duplicate_merge_resumes_its_second_operand() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (right, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("dictionary duplicate right operand"))
        .expect("the owner should allocate a promised operand");
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::MergeDuplicate),
        vec![
            Value::binary_from_text("nested"),
            Value::Dict(Dict::new_sync().insert(Key::atom_from_text("left"), n(41))),
            Value::Promised(right.clone()),
        ],
    )
    .expect("duplicate merge application should build");

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved right operand should suspend duplicate merge");
    assert!(blocked.blocked_on().is_some());

    let right_key = Key::atom_from_text("right");
    set_promise(
        &owner,
        &right,
        Value::Dict(Dict::new_sync().insert(right_key.clone(), n(42))),
    )
    .expect("the owner should resolve the duplicate operand");
    let value = eval_value(&observer, &application)
        .expect("duplicate merge should resume after promise assignment");
    let Value::Dict(dict) = value else {
        panic!("duplicate merge should produce a dictionary");
    };
    assert_eq!(dict.get(&Key::atom_from_text("left")), Some(&n(41)));
    assert_eq!(dict.get(&right_key), Some(&n(42)));
}

#[test]
fn list_at_resumes_without_replaying_a_completed_lazy_chunk() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (tail, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("list-at deferred tail"))
        .expect("the owner should allocate a promised list tail");
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted_attempts = attempts.clone();
    let prefix = LazyValue::semantic_thunk(
        observer.values(),
        "counted list-at prefix",
        move |_context| {
            counted_attempts.fetch_add(1, Ordering::SeqCst);
            Ok(Value::List(List::from_values(vec![n(41)])))
        },
    );
    let list = Value::List(List::concat(
        List::from_thunk(prefix.into()),
        List::from_thunk(tail.clone().into()),
    ));
    let application = apply_values(&observer, Value::Builtin(Builtin::ListAt), vec![n(1), list])
        .expect("list-at application should build");

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved tail should suspend list-at");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);

    set_promise(&owner, &tail, Value::List(List::from_values(vec![n(42)])))
        .expect("the owner should resolve the list tail");
    assert_eq!(
        eval_value(&observer, &application).expect("list-at should resume"),
        n(42)
    );
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "resumption must retain the completed prefix chunk"
    );
}

#[test]
fn split_end_resumes_from_the_back_without_forcing_an_unrelated_prefix() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (tail, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("split-end deferred tail"))
        .expect("the owner should allocate a promised list tail");
    let lazy_prefix = LazyValue::error(
        observer.values(),
        "split-end forced an unrelated lazy prefix",
    );
    let list = Value::List(List::concat(
        List::from_thunk(lazy_prefix.into()),
        List::from_thunk(tail.clone().into()),
    ));
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::ListSplitEnd),
        vec![n(1), list],
    )
    .expect("split-end application should build");

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved tail should suspend split-end");
    assert!(blocked.blocked_on().is_some());
    set_promise(&owner, &tail, Value::List(List::from_values(vec![n(42)])))
        .expect("the owner should resolve the list tail");

    let Value::Dict(split) = eval_value(&observer, &application)
        .expect("split-end should resume without observing its prefix")
    else {
        panic!("split-end should produce a dictionary")
    };
    let Value::List(suffix) = split
        .get(&Key::atom_from_text("right"))
        .expect("split-end should retain its suffix")
    else {
        panic!("split-end suffix should be a list")
    };
    assert_eq!(
        list_output_bytes(&observer, suffix).expect("strict suffix should render"),
        [42]
    );
}

#[test]
fn dictionary_unions_defer_ambiguous_keys_until_observed() {
    let key = Key::atom_from_text("greeting");
    let expr = dict_union_expr(
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
        )),
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("World")),
        )),
    );

    let value = eval_closed_expr(&expr).expect("outer dict union should stay evaluable");
    let ambiguous = value
        .get_key_path(&[key])
        .expect("ambiguous key should exist");
    let Value::Lazy(ambiguous) = ambiguous else {
        panic!("ambiguous duplicate should stay as a stuck expression");
    };

    let err = eval_value(&test_context(), &Value::Lazy(ambiguous.clone()))
        .expect_err("ambiguous key should fail only when demanded");

    assert_eq!(
        err.to_string(),
        "dictionary union is ambiguous at key `greeting`"
    );
}

#[test]
fn dictionary_updates_overwrite_duplicate_values() {
    let key = Key::atom_from_text("greeting");
    let expr = dict_update_expr(
        key_path_expr(vec![key.clone()]),
        TestExpr::Value(Value::binary_from_text("World")),
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
        )),
    );

    let value = eval_closed_expr(&expr).expect("dict update should evaluate");

    assert_eq!(
        value.get_key_path(&[key]),
        Some(&Value::binary_from_text("World"))
    );
}

#[test]
fn dictionary_updates_merge_nested_dictionaries_transitively() {
    let key = Key::atom_from_text("greeting");
    let hello = Key::atom_from_text("hello");
    let world = Key::atom_from_text("world");

    let expr = dict_update_expr(
        key_path_expr(vec![key.clone(), world.clone()]),
        TestExpr::Value(Value::binary_from_text("World")),
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(
                key.clone(),
                Value::Dict(
                    crate::core::Dict::new_sync()
                        .insert(hello.clone(), Value::binary_from_text("Hello")),
                ),
            ),
        )),
    );

    let value = eval_closed_expr(&expr).expect("dict update should evaluate");
    let greeting = value.get_key_path(&[key]).expect("greeting should exist");
    let Value::Dict(greeting) = greeting else {
        panic!("greeting should resolve directly to a dictionary");
    };

    assert_eq!(
        greeting.get(&hello),
        Some(&Value::binary_from_text("Hello"))
    );
    assert_eq!(
        greeting.get(&world),
        Some(&Value::binary_from_text("World"))
    );
}

#[test]
fn dictionary_updates_treat_empty_dictionary_values_as_undefined() {
    let key = Key::atom_from_text("greeting");
    let expr = dict_update_expr(
        key_path_expr(vec![key.clone()]),
        TestExpr::Value(Value::Dict(crate::core::Dict::new_sync())),
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
        )),
    );

    let value = eval_closed_expr(&expr).expect("dict update should evaluate");
    assert_eq!(value.get_key_path(&[key]), None);
}

#[test]
fn names_can_traverse_dictionary_union_bindings() {
    let d = Key::atom_from_text("d");
    let hello = Key::atom_from_text("hello");

    let root = crate::core::Dict::new_sync().insert(
        d.clone(),
        fixture_computation(dict_union_expr(
            TestExpr::Value(Value::Dict(
                crate::core::Dict::new_sync()
                    .insert(hello.clone(), Value::binary_from_text("Hello")),
            )),
            TestExpr::Value(Value::Dict(crate::core::Dict::new_sync())),
        )),
    );

    let value = eval_closed_expr(&fixpoint_dict(root)).expect("root should evaluate");
    let resolved = eval_value(
        &test_context(),
        &apply_rooted_fixture(
            &value,
            global_access(vec![TestKey::Key(d), TestKey::Key(hello)]),
        ),
    )
    .expect("dotted name should force intermediate dict unions");

    assert_eq!(resolved, Value::binary_from_text("Hello"));
}

#[test]
fn names_can_expand_list_valued_path_segments() {
    let foo = Key::atom_from_text("foo");
    let one = k(1);
    let two = k(2);
    let three = k(3);

    let nested = Value::Dict(
        crate::core::Dict::new_sync().insert(
            one.clone(),
            Value::Dict(
                crate::core::Dict::new_sync().insert(
                    two.clone(),
                    Value::Dict(
                        crate::core::Dict::new_sync()
                            .insert(three.clone(), Value::binary_from_text("World")),
                    ),
                ),
            ),
        ),
    );

    let root = crate::core::Dict::new_sync().insert(foo.clone(), nested);
    let value = eval_closed_expr(&fixpoint_dict(root)).expect("root should evaluate");
    let resolved = eval_value(
        &test_context(),
        &apply_rooted_fixture(
            &value,
            global_access(vec![
                TestKey::Key(foo),
                TestKey::PathIndex(Arc::new(TestExpr::Apply(
                    Arc::new(TestExpr::Apply(
                        Arc::new(TestExpr::Value(Value::Builtin(Builtin::Append))),
                        Arc::new(TestExpr::List(Arc::from([
                            Arc::new(TestExpr::Value(n(1))),
                            Arc::new(TestExpr::Value(n(2))),
                        ]))),
                    )),
                    Arc::new(TestExpr::List(Arc::from([Arc::new(TestExpr::Value(n(3)))]))),
                ))),
            ]),
        ),
    )
    .expect("list-valued path segment should expand into multiple lookups");

    assert_eq!(resolved, Value::binary_from_text("World"));
}

#[test]
fn missing_dictionary_members_resolve_to_empty_dictionary() {
    let root = Value::Dict(crate::core::Dict::new_sync().insert(
        Key::atom_from_text("present"),
        Value::Dict(crate::core::Dict::new_sync()),
    ));
    let resolved = eval_value(
        &test_context(),
        &apply_rooted_fixture(
            &root,
            global_access(vec![
                TestKey::Key(Key::atom_from_text("present")),
                TestKey::Key(Key::atom_from_text("missing")),
            ]),
        ),
    )
    .expect("missing member access should stay evaluable");

    assert_eq!(resolved, Value::Dict(crate::core::Dict::new_sync()));
}

#[test]
fn anno_builtin_continues_demand_after_assertions_pass() {
    let root =
        Value::Dict(crate::core::Dict::new_sync().insert(Key::atom_from_text("later"), n(42)));
    let annotation = singleton_expr(
        Value::Atom(crate::core::Atom::from_key(
            &crate::core::Key::binary_from_text("assert_undefined"),
        )),
        dict_union_expr(
            singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("name"),
                )),
                TestExpr::Value(Value::binary_from_text("missing")),
            ),
            singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("value"),
                )),
                global_access(vec![TestKey::Key(Key::atom_from_text("missing"))]),
            ),
        ),
    );

    let value = eval_value(
        &test_context(),
        &apply_rooted_fixture(
            &root,
            TestExpr::Apply(
                Arc::new(TestExpr::Apply(
                    Arc::new(TestExpr::Value(Value::Builtin(Builtin::Anno))),
                    Arc::new(annotation),
                )),
                Arc::new(global_access(vec![TestKey::Key(Key::atom_from_text(
                    "later",
                ))])),
            ),
        ),
    )
    .expect("anno should pass through successful assertions");

    assert_eq!(value, n(42));
}

#[test]
fn anno_builtin_reports_failed_assertions_during_demand() {
    let annotation = singleton_expr(
        Value::Atom(crate::core::Atom::from_key(
            &crate::core::Key::binary_from_text("assert_defined"),
        )),
        dict_union_expr(
            singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("name"),
                )),
                TestExpr::Value(Value::binary_from_text("foo")),
            ),
            singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("value"),
                )),
                global_access(vec![TestKey::Key(Key::atom_from_text("foo"))]),
            ),
        ),
    );

    let error = eval_value(
        &test_context(),
        &apply_rooted_fixture(
            &Value::Dict(crate::core::Dict::new_sync()),
            TestExpr::Apply(
                Arc::new(TestExpr::Apply(
                    Arc::new(TestExpr::Value(Value::Builtin(Builtin::Anno))),
                    Arc::new(annotation),
                )),
                Arc::new(TestExpr::Value(n(1))),
            ),
        ),
    )
    .expect_err("failed anno should raise during demand");
    assert_eq!(
        error.to_string(),
        "cannot override `foo` because it is not defined"
    );
}

#[test]
fn assert_unit_builtin_uses_its_diagnostic_context() {
    let target = n(42);
    let value = eval_closed_expr(&builtin3_expr(
        Builtin::AssertUnit,
        TestExpr::Value(Value::binary_from_text("test operation result")),
        TestExpr::Value(unit_value()),
        TestExpr::Value(target.clone()),
    ))
    .expect("unit assertion should return its target");
    assert_eq!(value, target);

    let error = eval_closed_expr(&builtin3_expr(
        Builtin::AssertUnit,
        TestExpr::Value(Value::binary_from_text("test operation result")),
        TestExpr::Value(Value::Dict(Dict::new_sync())),
        TestExpr::Value(n(42)),
    ))
    .expect_err("non-unit assertion value should fail");
    assert_eq!(
        error.to_string(),
        "test operation result: unit expected, received Undefined"
    );
}

#[test]
fn assert_unit_annotation_has_optional_diagnostic_context() {
    let annotation = |payload| {
        singleton_expr(
            Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
                "assert_unit",
            ))),
            payload,
        )
    };
    let value_payload = || {
        singleton_expr(
            Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text("value"))),
            TestExpr::Value(n(1)),
        )
    };

    let generic_error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        annotation(value_payload()),
        TestExpr::Value(n(42)),
    ))
    .expect_err("context-free unit annotation should fail generically");
    assert_eq!(generic_error.to_string(), "unit expected, received Number");

    let contextual_payload = dict_union_expr(
        value_payload(),
        singleton_expr(
            Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
                "context",
            ))),
            TestExpr::Value(Value::binary_from_text("annotated operation result")),
        ),
    );
    let contextual_error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        annotation(contextual_payload),
        TestExpr::Value(n(42)),
    ))
    .expect_err("contextual unit annotation should fail");
    assert_eq!(
        contextual_error.to_string(),
        "annotated operation result: unit expected, received Number"
    );
}

#[test]
fn assert_unit_annotation_resumes_diagnostic_context_after_collection_without_replay() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (diagnostic_context, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("annotation diagnostic context"))
        .expect("the owner should allocate a promised diagnostic context");
    let value_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&value_demands);
    let value = Value::semantic_thunk(
        observer.values(),
        "instrumented annotation assertion value",
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(n(42))
        },
    );
    let annotation = Value::Dict(
        Dict::new_sync().insert(
            Key::atom_from_text("assert_unit"),
            Value::Dict(
                Dict::new_sync()
                    .insert((*keys::VALUE).clone(), value)
                    .insert(
                        (*keys::CONTEXT).clone(),
                        Value::Promised(diagnostic_context.clone()),
                    ),
            ),
        ),
    );
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::Anno),
        vec![annotation, n(7)],
    )
    .expect("assertion annotation application should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated annotation builtin should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved diagnostic context should suspend the assertion");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(value_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the annotation checkpoint must trace completed assertion work");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact diagnostic-context promise");
    assert_eq!(value_demands.load(Ordering::SeqCst), 1);

    set_promise(
        &owner,
        &diagnostic_context,
        Value::binary_from_text("assertion result"),
    )
    .expect("the owner should resolve the promised diagnostic context");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned annotation checkpoint must remain live");
    let failure = eval_value(&observer, &application)
        .expect_err("the resumed non-unit assertion should fail");
    assert_eq!(
        failure.to_string(),
        "assertion result: unit expected, received Number"
    );
    assert_eq!(value_demands.load(Ordering::SeqCst), 1);
    drop(application_root);
}

#[test]
fn error_annotations_carry_diagnostic_values_and_ordered_contexts() {
    let atom = |name| Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(name)));
    let context_annotation = |context| {
        TestExpr::Value(Value::Dict(
            Dict::new_sync().insert((*keys::CONTEXT).clone(), context),
        ))
    };
    let message = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::MSG).clone(),
                Value::Dict(
                    Dict::new_sync()
                        .insert(
                            (*keys::TEXT).clone(),
                            Value::binary_from_text("handler failed"),
                        )
                        .insert(
                            (*keys::CONTEXT).clone(),
                            Value::List(List::from_values(vec![Value::binary_from_text(
                                "emitted",
                            )])),
                        ),
                ),
            )
            .insert(Key::atom_from_text("operation"), atom("emit")),
    );
    let failure = builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(atom("error")),
        TestExpr::Value(message),
    );
    let inner = builtin2_expr(
        Builtin::Anno,
        context_annotation(Value::binary_from_text("inner")),
        failure,
    );
    let outer = builtin2_expr(
        Builtin::Anno,
        context_annotation(Value::binary_from_text("outer")),
        inner,
    );

    let error = eval_closed_expr(&outer).expect_err("error annotation must fail when demanded");
    assert_eq!(error.to_string(), "handler failed");
    let diagnostic =
        halt_diagnostic_value(&error).expect("permanent errors must project to diagnostics");
    let Value::Dict(diagnostic) = eval_value(&test_context(), &diagnostic).unwrap() else {
        panic!("failure diagnostic must be a dictionary");
    };
    let operation = eval_value(
        &test_context(),
        diagnostic
            .get(&Key::atom_from_text("operation"))
            .expect("diagnostic should retain ad hoc fields"),
    )
    .unwrap();
    assert_eq!(operation, atom("emit"));
    let message = eval_value(
        &test_context(),
        diagnostic
            .get(&*keys::MSG)
            .expect("diagnostic should define msg"),
    )
    .unwrap();
    let Value::Dict(message) = message else {
        panic!("diagnostic msg must be a dictionary");
    };
    let contexts = eval_value(
        &test_context(),
        message
            .get(&*keys::CONTEXT)
            .expect("context annotation should define msg.context"),
    )
    .unwrap();
    let Value::List(contexts) = contexts else {
        panic!("msg.context must be a list");
    };
    assert_eq!(
        list_to_value_items(&test_context(), &contexts).unwrap(),
        [
            Value::binary_from_text("outer"),
            Value::binary_from_text("inner"),
            Value::binary_from_text("emitted")
        ]
    );
}

#[test]
fn error_annotations_contextualize_failure_while_evaluating_their_message() {
    let error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("error"),
        ))),
        TestExpr::Value(Value::error(
            &crate::core::test_value_factory(),
            "message construction failed",
        )),
    ))
    .expect_err("failure while constructing an error message must propagate");
    assert_eq!(error.to_string(), "message construction failed");

    let diagnostic =
        halt_diagnostic_value(&error).expect("message-construction failure must remain permanent");
    let Value::Dict(diagnostic) = eval_value(&test_context(), &diagnostic).unwrap() else {
        panic!("message-construction failure must project to a diagnostic");
    };
    let message = eval_value(
        &test_context(),
        diagnostic
            .get(&*keys::MSG)
            .expect("diagnostic should define msg"),
    )
    .unwrap();
    let Value::Dict(message) = message else {
        panic!("diagnostic msg must be a dictionary");
    };
    let contexts = eval_value(
        &test_context(),
        message
            .get(&*keys::CONTEXT)
            .expect("message-construction failure should define msg.context"),
    )
    .unwrap();
    let Value::List(contexts) = contexts else {
        panic!("msg.context must be a list");
    };
    assert_eq!(
        list_to_value_items(&test_context(), &contexts).unwrap(),
        [evaluation_context_frame("error_message")]
    );
}

#[test]
fn annotation_selection_contextualizes_only_nested_evaluation_failures() {
    let error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::error(
            &crate::core::test_value_factory(),
            "annotation selection failed",
        )),
        TestExpr::Value(n(42)),
    ))
    .expect_err("failure while selecting an annotation must propagate");
    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("annotation")]
    );
}

#[test]
fn index_builtins_contextualize_demand_without_decorating_validation_errors() {
    let values = Value::List(List::from_values(vec![n(42)]));
    let nested = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(Value::error(
            &crate::core::test_value_factory(),
            "index computation failed",
        )),
        TestExpr::Value(values.clone()),
    ))
    .expect_err("failure while evaluating the index must propagate");
    assert_eq!(
        failure_context_items(&nested),
        [evaluation_context_frame("list_index")]
    );

    let validation = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(Value::binary_from_text("not an index")),
        TestExpr::Value(values),
    ))
    .expect_err("a nonnumeric index must fail validation");
    assert_eq!(failure_context_items(&validation), []);
}

fn failure_context_items(error: &EvaluationHalt) -> Vec<Value> {
    let diagnostic = halt_diagnostic_value(error).expect("test error should be permanent");
    let Value::Dict(diagnostic) = eval_value(&test_context(), &diagnostic).unwrap() else {
        panic!("failure diagnostic must be a dictionary");
    };
    let message = eval_value(
        &test_context(),
        diagnostic
            .get(&*keys::MSG)
            .expect("diagnostic should define msg"),
    )
    .unwrap();
    let Value::Dict(message) = message else {
        panic!("diagnostic msg must be a dictionary");
    };
    let contexts = eval_value(
        &test_context(),
        message
            .get(&*keys::CONTEXT)
            .expect("diagnostic should define msg.context"),
    )
    .unwrap();
    let Value::List(contexts) = contexts else {
        panic!("msg.context must be a list");
    };
    list_to_value_items(&test_context(), &contexts).unwrap()
}

#[test]
fn context_annotations_are_transparent_and_do_not_demand_context_on_success() {
    let annotation = TestExpr::Value(Value::Dict(Dict::new_sync().insert(
        (*keys::CONTEXT).clone(),
        Value::error(
            &crate::core::test_value_factory(),
            "unused context must remain lazy",
        ),
    )));
    let value = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        annotation,
        TestExpr::Value(n(42)),
    ))
    .expect("successful context annotation should return its target");
    assert_eq!(value, n(42));
}

#[test]
fn metadata_annotation_initializes_the_canonical_sealed_carrier() {
    let context = test_context();
    let annotation = || {
        Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
            "meta_init",
        )))
    };
    let target_forces = Arc::new(AtomicUsize::new(0));
    let counted_target_forces = target_forces.clone();
    let unit = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "metadata annotation unit",
        move |_| {
            counted_target_forces.fetch_add(1, Ordering::SeqCst);
            Ok(unit_value())
        },
    );

    let first = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![annotation(), unit],
    )
    .expect("metadata annotation should accept demanded canonical unit");
    let first = eval_value(&context, &first).expect("metadata initialization should evaluate");
    let second = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![annotation(), unit_value()],
    )
    .expect("metadata annotation should reuse its canonical carrier");
    let second = eval_value(&context, &second).expect("metadata initialization should evaluate");

    assert_eq!(target_forces.load(Ordering::SeqCst), 1);
    assert_eq!(first, initial_metadata());
    assert_eq!(second, initial_metadata());
    assert_eq!(first, second);
    assert_eq!(
        first.associated_metadata(),
        Some(Value::Dict(Dict::new_sync()))
    );

    let seq_result = apply_values(
        &context,
        Value::Builtin(Builtin::Seq),
        vec![first.clone(), n(42)],
    )
    .expect("initial metadata should already satisfy shallow sequencing");
    assert_eq!(eval_value(&context, &seq_result).unwrap(), n(42));

    let spark_result = apply_values(&context, Value::Builtin(Builtin::Spark), vec![first, n(43)])
        .expect("initial metadata should be safe to spark");
    assert_eq!(eval_value(&context, &spark_result).unwrap(), n(43));
}

#[test]
fn metadata_annotation_rejects_non_unit_and_existing_carriers() {
    let context = test_context();
    let annotation = || {
        Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
            "meta_init",
        )))
    };

    for (target, expected_kind) in [
        (n(42), "Number"),
        (Value::Dict(Dict::new_sync()), "Undefined"),
        (initial_metadata(), "Sealed"),
    ] {
        let result = apply_values(
            &context,
            Value::Builtin(Builtin::Anno),
            vec![annotation(), target],
        )
        .expect("annotation application should remain lazy");
        let error = eval_value(&context, &result)
            .expect_err("metadata initialization must require canonical unit");
        assert_eq!(
            error.to_string(),
            format!("unit expected, received {expected_kind}")
        );
    }
}

#[test]
fn old_metadata_annotation_spellings_are_unrecognized() {
    let context = test_context();
    let old_initial = Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text("meta")));
    let old_initial = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![old_initial, n(42)],
    )
    .expect("an unrecognized annotation should apply lazily");
    assert_eq!(
        eval_value(&context, &old_initial)
            .expect("an unrecognized annotation should preserve its target"),
        n(42),
        "the old initializer must not create a sealed carrier"
    );

    let carrier = Value::metadata_carrier(n(7));
    let old_update = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("meta_upd"),
        Value::error(
            &crate::core::test_value_factory(),
            "the old update function must remain unused",
        ),
    ));
    let target = Value::List(List::from_values(vec![carrier.clone()]));
    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![old_update, target.clone()],
    )
    .expect("an unrecognized annotation should preserve its target");
    let result = eval_value(&context, &result)
        .expect("the unrecognized annotation should evaluate to its target");
    assert_eq!(result, target);
    let Value::List(result) = result else {
        panic!("the preserved target must remain a list");
    };
    assert_eq!(
        list_to_value_items(&context, &result).unwrap(),
        vec![carrier],
        "the old updater must not derive another carrier"
    );
}

fn run_metadata_update(
    context: &EvalContext,
    function: Value,
    carriers: Vec<Value>,
) -> Result<Vec<Value>, EvaluationHalt> {
    run_metadata_transform(context, "meta_pure", function, carriers)
}

fn run_metadata_reflection_update(
    context: &EvalContext,
    function: Value,
    carriers: Vec<Value>,
) -> Result<Vec<Value>, EvaluationHalt> {
    run_metadata_transform(context, "meta_refl", function, carriers)
}

fn run_metadata_transform(
    context: &EvalContext,
    annotation_name: &str,
    function: Value,
    carriers: Vec<Value>,
) -> Result<Vec<Value>, EvaluationHalt> {
    let annotation =
        Value::Dict(Dict::new_sync().insert(Key::atom_from_text(annotation_name), function));
    let result = context.evaluate_builtin_whnf(
        Builtin::Anno,
        vec![annotation, Value::List(List::from_values(carriers))],
    )?;
    let Value::List(result) = result else {
        panic!("metadata update should return a list");
    };
    list_to_value_items(context, &result)
}

fn metadata_reorder_function(indices: &[usize]) -> Value {
    metadata_reorder_function_in(&crate::core::test_value_factory(), indices)
}

fn metadata_reorder_function_in(
    values: &crate::core::CoreValueFactory,
    indices: &[usize],
) -> Value {
    let projections = indices
        .iter()
        .map(|index| {
            Arc::new(builtin2_expr(
                Builtin::ListAt,
                TestExpr::Value(Value::Number(Number::from_usize(*index))),
                TestExpr::Local(0),
            ))
        })
        .collect::<Vec<_>>();
    closed_function_value_in(values, 1, TestExpr::List(Arc::from(projections)))
}

fn evaluated_metadata(context: &EvalContext, carrier: &Value) -> Result<Value, EvaluationHalt> {
    let metadata = carrier
        .associated_metadata()
        .expect("metadata update output must remain sealed");
    eval_value(context, &metadata)
}

#[test]
fn metadata_update_reorders_copies_and_clears_hidden_values() {
    let context = test_context();
    let left = Value::metadata_carrier(n(1));
    let right = Value::metadata_carrier(n(2));

    let swapped = run_metadata_update(
        &context,
        metadata_reorder_function(&[1, 0]),
        vec![left.clone(), right.clone()],
    )
    .expect("metadata update should support permutation");
    assert_eq!(
        swapped
            .iter()
            .map(|carrier| evaluated_metadata(&context, carrier))
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        vec![n(2), n(1)]
    );

    let copied = run_metadata_update(
        &context,
        metadata_reorder_function(&[0, 0]),
        vec![left, right],
    )
    .expect("metadata update should support copying");
    assert_eq!(
        copied
            .iter()
            .map(|carrier| evaluated_metadata(&context, carrier))
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        vec![n(1), n(1)]
    );

    let cleared = run_metadata_update(
        &context,
        closed_function_value(
            1,
            TestExpr::List(Arc::from([
                Arc::new(builtin2_expr(
                    Builtin::Add,
                    builtin2_expr(Builtin::ListAt, TestExpr::Value(n(0)), TestExpr::Local(0)),
                    builtin2_expr(Builtin::ListAt, TestExpr::Value(n(1)), TestExpr::Local(0)),
                )),
                Arc::new(TestExpr::Value(Value::Dict(Dict::new_sync()))),
            ])),
        ),
        vec![Value::metadata_carrier(n(1)), Value::metadata_carrier(n(2))],
    )
    .expect("metadata update should permit merging and clearing");
    assert_eq!(
        cleared
            .iter()
            .map(|carrier| evaluated_metadata(&context, carrier))
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        vec![n(3), Value::Dict(Dict::new_sync())]
    );
}

#[test]
fn metadata_update_resumes_without_replaying_a_completed_carrier() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (second, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("metadata annotation carrier"))
        .expect("the owner should allocate a promised carrier");
    let first_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&first_demands);
    let first = Value::semantic_thunk(observer.values(), "metadata carrier prefix", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::metadata_carrier(n(1)))
    });
    let annotation = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("meta_pure"),
        metadata_reorder_function_in(observer.values(), &[0, 1]),
    ));
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::Anno),
        vec![
            annotation,
            Value::List(List::from_values(vec![
                first,
                Value::Promised(second.clone()),
            ])),
        ],
    )
    .expect("metadata annotation application should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated metadata annotation should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved carrier should suspend metadata extraction");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(first_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the metadata checkpoint must trace its completed carrier prefix");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact promised carrier");
    assert_eq!(first_demands.load(Ordering::SeqCst), 1);

    set_promise(&owner, &second, Value::metadata_carrier(n(2)))
        .expect("the owner should resolve the promised carrier");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned metadata checkpoint must remain live");
    let Value::List(carriers) =
        eval_value(&observer, &application).expect("metadata extraction should resume")
    else {
        panic!("metadata update should produce carrier outputs")
    };
    let carriers = list_to_value_items(&observer, &carriers).unwrap();
    assert_eq!(
        carriers
            .iter()
            .map(|carrier| evaluated_metadata(&observer, carrier))
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        [n(1), n(2)]
    );
    assert_eq!(
        first_demands.load(Ordering::SeqCst),
        1,
        "resumption must retain the completed metadata carrier"
    );
    drop(application_root);
}

#[test]
fn metadata_update_preserves_input_arity_without_validating_output_length() {
    let context = test_context();
    let empty = run_metadata_update(
        &context,
        Value::error(&crate::core::test_value_factory(), "unused update"),
        Vec::new(),
    )
    .expect("an empty carrier list should not demand its update function");
    assert!(empty.is_empty());

    let too_short = run_metadata_update(
        &context,
        closed_function_value(
            1,
            TestExpr::Value(Value::List(List::from_values(vec![n(7)]))),
        ),
        vec![initial_metadata(), initial_metadata()],
    )
    .expect("a short update list should remain latent inside output carriers");
    assert_eq!(too_short.len(), 2);
    let missing_error = evaluated_metadata(&context, &too_short[1])
        .expect_err("only the missing projection should fail");
    assert_eq!(
        missing_error.to_string(),
        "list at builtin index is out of bounds"
    );
    assert_eq!(
        evaluated_metadata(&context, &too_short[0]).unwrap(),
        n(7),
        "a failed later projection must not poison an earlier valid one"
    );

    let extra_forces = Arc::new(AtomicUsize::new(0));
    let counted_extra_forces = extra_forces.clone();
    let extra = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "unused metadata update result",
        move |_| {
            counted_extra_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(9))
        },
    );
    let too_long = run_metadata_update(
        &context,
        closed_function_value(
            1,
            TestExpr::Value(Value::List(List::from_values(vec![n(8), extra]))),
        ),
        vec![initial_metadata()],
    )
    .expect("extra update values should be ignored");
    assert_eq!(too_long.len(), 1);
    assert_eq!(evaluated_metadata(&context, &too_long[0]).unwrap(), n(8));
    assert_eq!(
        extra_forces.load(Ordering::SeqCst),
        0,
        "an unused extra update value must remain lazy"
    );
}

#[test]
fn metadata_update_validates_inputs_strictly_but_not_hidden_metadata() {
    let context = test_context();
    let carrier_forces = Arc::new(AtomicUsize::new(0));
    let counted_carrier_forces = carrier_forces.clone();
    let hidden_forces = Arc::new(AtomicUsize::new(0));
    let counted_hidden_forces = hidden_forces.clone();
    let hidden = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "hidden metadata",
        move |_| {
            counted_hidden_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(11))
        },
    );
    let carrier = Value::metadata_carrier(hidden);
    let lazy_carrier = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "lazy metadata carrier",
        move |_| {
            counted_carrier_forces.fetch_add(1, Ordering::SeqCst);
            Ok(carrier.clone())
        },
    );

    let result = run_metadata_update(
        &context,
        metadata_reorder_function(&[0]),
        vec![lazy_carrier],
    )
    .expect("lazy carrier shells should be demanded during input validation");
    assert_eq!(carrier_forces.load(Ordering::SeqCst), 1);
    assert_eq!(
        hidden_forces.load(Ordering::SeqCst),
        0,
        "input validation must not demand associated metadata"
    );
    assert_eq!(evaluated_metadata(&context, &result[0]).unwrap(), n(11));
    assert_eq!(hidden_forces.load(Ordering::SeqCst), 1);

    let error = run_metadata_update(
        &context,
        Value::error(
            &crate::core::test_value_factory(),
            "update function must remain unused",
        ),
        vec![n(1)],
    )
    .expect_err("ordinary input values must be rejected before update evaluation");
    assert_eq!(
        error.to_string(),
        "`meta_pure` annotation item 0 must be a sealed metadata carrier, received Number"
    );

    let annotation = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("meta_pure"),
        Value::error(
            &crate::core::test_value_factory(),
            "update function must remain unused",
        ),
    ));
    let error = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![annotation, n(1)],
    )
    .and_then(|value| eval_value(&context, &value))
    .expect_err("metadata update target must be a list");
    assert_eq!(
        error.to_string(),
        "`meta_pure` annotation requires a list of sealed metadata carriers"
    );
}

#[test]
fn metadata_update_shares_update_failures_between_projections() {
    let context = test_context();
    let update_forces = Arc::new(AtomicUsize::new(0));
    let counted_update_forces = update_forces.clone();
    let function = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "failing metadata update",
        move |_| {
            counted_update_forces.fetch_add(1, Ordering::SeqCst);
            Err(EvaluationHalt::new("shared metadata update failed"))
        },
    );
    let result = run_metadata_update(
        &context,
        function,
        vec![initial_metadata(), initial_metadata()],
    )
    .expect("update failure should remain latent inside output carriers");
    assert_eq!(update_forces.load(Ordering::SeqCst), 0);

    for carrier in &result {
        let error =
            evaluated_metadata(&context, carrier).expect_err("every shared projection must fail");
        assert_eq!(error.to_string(), "shared metadata update failed");
    }
    assert_eq!(
        update_forces.load(Ordering::SeqCst),
        1,
        "all projections must share one update application"
    );
}

#[test]
fn metadata_update_delegates_output_interpretation_to_list_at() {
    let context = test_context();
    let binary = run_metadata_update(
        &context,
        closed_function_value(1, TestExpr::Value(Value::binary_from_text("x"))),
        vec![initial_metadata()],
    )
    .expect("binary update output should remain indexable");
    assert_eq!(
        evaluated_metadata(&context, &binary[0]).unwrap(),
        n(i64::from(b'x'))
    );

    let number = run_metadata_update(
        &context,
        closed_function_value(1, TestExpr::Value(n(42))),
        vec![initial_metadata()],
    )
    .expect("an unindexable update result should remain latent");
    let error =
        evaluated_metadata(&context, &number[0]).expect_err("the indexed projection must fail");
    assert_eq!(
        error.to_string(),
        "list at builtin requires a list or binary value"
    );
    assert_eq!(
        error.into_permanent_failure().contexts(),
        [evaluation_context_frame("wrap_metadata")]
    );
}

#[test]
fn metadata_reflection_update_is_inert_until_demand_and_shares_one_task() {
    let context = test_context();
    let builds = Arc::new(AtomicUsize::new(0));
    let result_policies = Arc::new(Mutex::new(Vec::new()));
    context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![
                n(2),
                n(1),
            ]))),
            builds: builds.clone(),
            result_policies: result_policies.clone(),
        }))
        .expect("fresh test session should accept its reflection launcher");

    let outputs = run_metadata_reflection_update(
        &context,
        Value::error(
            &crate::core::test_value_factory(),
            "the fixture launcher must not evaluate the effect",
        ),
        vec![Value::metadata_carrier(n(1)), Value::metadata_carrier(n(2))],
    )
    .expect("effectful metadata update should construct its output carriers");
    let copied_first = outputs[0].clone();
    assert_eq!(
        builds.load(Ordering::SeqCst),
        0,
        "constructing, copying, and transporting carriers must not launch their task"
    );

    assert_eq!(evaluated_metadata(&context, &outputs[1]).unwrap(), n(1));
    assert_eq!(evaluated_metadata(&context, &outputs[0]).unwrap(), n(2));
    assert_eq!(evaluated_metadata(&context, &copied_first).unwrap(), n(2));
    assert_eq!(
        builds.load(Ordering::SeqCst),
        1,
        "all projections and carrier copies must share one reflection task"
    );
    assert_eq!(
        *result_policies
            .lock()
            .expect("fixture result policies were poisoned"),
        [ReflectionTaskResultPolicy::ReturnValue]
    );
}

#[test]
fn metadata_reflection_update_blocks_and_resumes_on_its_shared_task() {
    let context = test_context();
    let outputs = run_metadata_reflection_update(
        &context,
        Value::error(&crate::core::test_value_factory(), "unlaunched effect"),
        vec![initial_metadata()],
    )
    .expect("effectful metadata update should remain latent");

    let blocked = evaluated_metadata(&context, &outputs[0])
        .expect_err("an unlaunched metadata task should block");
    let wait = blocked
        .blocked_on()
        .expect("the metadata projection should expose its task wait");
    context.complete_wait_with_value(&wait.0, Value::List(List::from_values(vec![n(42)])));
    assert_eq!(evaluated_metadata(&context, &outputs[0]).unwrap(), n(42));
}

#[test]
fn metadata_reflection_update_propagates_task_failure_and_cancellation() {
    let failure = Arc::new(
        EvaluationFailure::message("metadata reflection task failed")
            .with_context(evaluation_context_frame("metadata_producer")),
    );
    let failed_context = test_context();
    failed_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Failed(failure),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let failed = run_metadata_reflection_update(&failed_context, n(0), vec![initial_metadata()])
        .expect("task failure should remain latent in its output carrier");
    let error = evaluated_metadata(&failed_context, &failed[0])
        .expect_err("demanding failed effectful metadata must propagate its failure");
    assert_eq!(error.to_string(), "metadata reflection task failed");
    assert_eq!(
        failed_context
            .task_registry_counts()
            .unacknowledged_failures,
        0,
        "the demanding metadata projection owns reporting responsibility"
    );

    let cancelled_context = test_context();
    cancelled_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Cancelled,
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let cancelled =
        run_metadata_reflection_update(&cancelled_context, n(0), vec![initial_metadata()])
            .expect("task cancellation should remain latent in its output carrier");
    let error = evaluated_metadata(&cancelled_context, &cancelled[0])
        .expect_err("demanding cancelled effectful metadata must fail");
    assert_eq!(error.to_string(), "reflection result task was cancelled");
}

#[test]
fn metadata_reflection_update_preserves_projection_semantics_and_input_validation() {
    let short_context = test_context();
    short_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![n(7)]))),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let short = run_metadata_reflection_update(
        &short_context,
        n(0),
        vec![initial_metadata(), initial_metadata()],
    )
    .expect("a short result should remain latent");
    assert_eq!(evaluated_metadata(&short_context, &short[0]).unwrap(), n(7));
    assert_eq!(
        evaluated_metadata(&short_context, &short[1])
            .expect_err("the missing projection should fail")
            .to_string(),
        "list at builtin index is out of bounds"
    );

    let long_context = test_context();
    let unused_extra = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "unused effectful metadata result",
        |_| panic!("an extra metadata result must remain unused"),
    );
    long_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![
                n(8),
                unused_extra,
            ]))),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let long = run_metadata_reflection_update(&long_context, n(0), vec![initial_metadata()])
        .expect("an extra result should be ignored");
    assert_eq!(evaluated_metadata(&long_context, &long[0]).unwrap(), n(8));

    let non_list_context = test_context();
    non_list_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(n(42)),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let non_list =
        run_metadata_reflection_update(&non_list_context, n(0), vec![initial_metadata()])
            .expect("an unindexable result should remain latent");
    assert_eq!(
        evaluated_metadata(&non_list_context, &non_list[0])
            .expect_err("the projection should delegate its error to list.at")
            .to_string(),
        "list at builtin requires a list or binary value"
    );

    let partial_context = test_context();
    partial_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![
                Value::error(
                    &crate::core::test_value_factory(),
                    "one metadata projection failed",
                ),
                n(9),
            ]))),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let partial = run_metadata_reflection_update(
        &partial_context,
        n(0),
        vec![initial_metadata(), initial_metadata()],
    )
    .expect("individual failed results should remain latent");
    assert_eq!(
        evaluated_metadata(&partial_context, &partial[0])
            .expect_err("the first metadata result should fail")
            .to_string(),
        "one metadata projection failed"
    );
    assert_eq!(
        evaluated_metadata(&partial_context, &partial[1]).unwrap(),
        n(9),
        "one failed result must not poison a sibling projection"
    );

    let invalid_context = test_context();
    let error = run_metadata_reflection_update(&invalid_context, n(0), vec![n(1)])
        .expect_err("ordinary values must be rejected before task launch");
    assert_eq!(
        error.to_string(),
        "`meta_refl` annotation item 0 must be a sealed metadata carrier, received Number"
    );
    let error = run_metadata_reflection_update(&invalid_context, n(0), Vec::new())
        .expect("an empty carrier list should construct no projections");
    assert!(error.is_empty());
    assert_eq!(invalid_context.reflection_task_count(), 0);

    let annotation = Value::Dict(Dict::new_sync().insert(Key::atom_from_text("meta_refl"), n(0)));
    let error = apply_values(
        &invalid_context,
        Value::Builtin(Builtin::Anno),
        vec![annotation, n(1)],
    )
    .and_then(|value| eval_value(&invalid_context, &value))
    .expect_err("effectful metadata target must be a list");
    assert_eq!(
        error.to_string(),
        "`meta_refl` annotation requires a list of sealed metadata carriers"
    );
}

#[test]
fn metadata_reflection_update_is_demanded_by_seq_and_worker_spark() {
    let seq_context = test_context();
    let seq_builds = Arc::new(AtomicUsize::new(0));
    seq_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![n(7)]))),
            builds: seq_builds.clone(),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let seq_outputs =
        run_metadata_reflection_update(&seq_context, n(0), vec![initial_metadata()]).unwrap();
    assert_eq!(
        evaluate_strategy(&seq_context, Builtin::Seq, seq_outputs[0].clone(), n(42)).unwrap(),
        n(42)
    );
    assert_eq!(seq_builds.load(Ordering::SeqCst), 1);

    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let spark_context = EvalContext::new(&session);
    let spark_builds = Arc::new(AtomicUsize::new(0));
    spark_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![n(8)]))),
            builds: spark_builds.clone(),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let spark_outputs =
        run_metadata_reflection_update(&spark_context, n(0), vec![initial_metadata()]).unwrap();
    let result = evaluate_strategy(
        &spark_context,
        Builtin::Spark,
        spark_outputs[0].clone(),
        n(43),
    )
    .expect("spark should immediately return its target");
    assert_eq!(result, n(43));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while spark_builds.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(
        spark_builds.load(Ordering::SeqCst),
        1,
        "a worker spark should demand the hidden reflection task"
    );
}

#[test]
fn list_annotations_rebalance_and_flatten_lists() {
    let context = isolated_test_context();
    let eval = |expression: TestExpr| {
        let code = lower_test_function_code_in(context.values(), 0, expression);
        let computation = Value::Lazy(LazyValue::from_net_computation(
            context.values(),
            NetValue::new(code.runtime().duplicate_for_test(context.values())),
        ));
        eval_value(&context, &computation)
    };

    let deque = eval(builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("deque"),
        ))),
        TestExpr::Value(Value::List(List::concat(
            List::from_bytes(Bytes::from_static(b"Hello")),
            List::from_values(vec![n(33)]),
        ))),
    ))
    .expect("deque annotation should evaluate");
    let Value::List(deque) = deque else {
        panic!("deque annotation should produce a list");
    };
    assert_eq!(deque.len(), 6);

    let binary = eval(builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("binary"),
        ))),
        TestExpr::Value(Value::List(List::concat(
            List::from_values(vec![n(72), n(105)]),
            List::from_bytes(Bytes::from_static(b"!")),
        ))),
    ))
    .expect("binary annotation should evaluate");
    assert_eq!(binary, Value::binary_from_text("Hi!"));

    let array = eval(builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("array"),
        ))),
        TestExpr::Value(Value::binary_from_text("Hi")),
    ))
    .expect("array annotation should evaluate");
    let Value::List(array) = array else {
        panic!("array annotation should produce a list");
    };
    assert_eq!(
        list_to_value_items(&context, &array).unwrap(),
        vec![n(72), n(105)]
    );
}

#[test]
fn binary_annotation_resumes_without_replaying_a_completed_prefix() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let (item, _owner_task, _owner) = owner
        .task_owned_promise(Arc::from("binary annotation item"))
        .expect("the owner should allocate a promised byte");
    let prefix_demands = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&prefix_demands);
    let prefix = Value::semantic_thunk(observer.values(), "binary annotation prefix", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(n(i64::from(b'a')))
    });
    let annotation = Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
        "binary",
    )));
    let application = apply_values(
        &observer,
        Value::Builtin(Builtin::Anno),
        vec![
            annotation,
            Value::List(List::from_values(vec![
                prefix,
                Value::Promised(item.clone()),
            ])),
        ],
    )
    .expect("binary annotation application should build");
    let Value::Lazy(application_lazy) = &application else {
        panic!("a saturated binary annotation should remain lazy")
    };
    let application_root = application_lazy.root(observer.values());

    let blocked = eval_value(&observer, &application)
        .expect_err("the unresolved byte should suspend binary extraction");
    assert!(blocked.blocked_on().is_some());
    assert_eq!(prefix_demands.load(Ordering::SeqCst), 1);
    observer
        .values()
        .collect_managed_for_test()
        .expect("the binary checkpoint must trace its completed byte prefix");
    eval_value(&observer, &application)
        .expect_err("a later route must resume the exact promised byte");
    assert_eq!(prefix_demands.load(Ordering::SeqCst), 1);

    set_promise(&owner, &item, n(i64::from(b'b')))
        .expect("the owner should resolve the promised byte");
    observer
        .values()
        .collect_managed_for_test()
        .expect("the assigned binary checkpoint must remain live");
    assert_eq!(
        eval_value(&observer, &application).expect("binary extraction should resume"),
        Value::binary_from_text("ab")
    );
    assert_eq!(
        prefix_demands.load(Ordering::SeqCst),
        1,
        "resumption must retain the completed prefix byte"
    );
    drop(application_root);
}

#[test]
fn list_annotations_report_errors_for_wrong_targets() {
    let error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("binary"),
        ))),
        TestExpr::Value(Value::List(List::from_values(vec![n(300)]))),
    ))
    .expect_err("invalid binary annotation should fail during demand");

    assert_eq!(
        error.to_string(),
        "`binary` annotation cannot encode number `300` as a byte"
    );

    let error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("deque"),
        ))),
        TestExpr::Value(n(1)),
    ))
    .expect_err("invalid deque annotation should fail during demand");

    assert!(
        error
            .to_string()
            .contains("`deque` annotation requires a list target")
    );
}

#[test]
fn unknown_annotations_pass_through_targets() {
    let value = eval_closed_expr(&TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(Builtin::Anno))),
            Arc::new(singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("mystery"),
                )),
                TestExpr::Value(n(0)),
            )),
        )),
        Arc::new(TestExpr::Value(n(42))),
    ))
    .expect("unknown annotations should pass through");

    assert_eq!(value, n(42));
}

fn reflection_annotation(context: &EvalContext, effect: Value, target: Value) -> Value {
    let annotation = Value::Dict(Dict::new_sync().insert(Key::atom_from_text("refl"), effect));
    apply_builtin(context, Builtin::Anno, vec![annotation], target)
        .expect("reflection annotation should construct a lazy gate")
}

#[test]
fn reflection_source_reserves_inside_and_activates_after_evaluator_access_closes() {
    let context = EvalContext::isolated(crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    ));
    let builds = Arc::new(AtomicUsize::new(0));
    context
        .install_reflection_launcher(Arc::new(ScopedReflectionLauncher {
            values: context.values().clone(),
            builds: builds.clone(),
        }))
        .expect("fresh test runtime should accept its reflection launcher");

    let value = Value::reflection_task_result(context.values(), n(0));
    assert_eq!(
        eval_value(&context, &value).expect("reflection result should complete"),
        unit_value()
    );
    assert_eq!(
        builds.load(Ordering::SeqCst),
        1,
        "the source-to-promise handoff must activate exactly one task"
    );
}

#[test]
fn dropped_reflection_completion_activation_permit_terminalizes_managed_promise() {
    let context = test_context();
    let builds = Arc::new(AtomicUsize::new(0));
    context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(n(42)),
            builds: builds.clone(),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh permit-drop fixture should accept its reflection launcher");
    let background = context
        .for_runtime_background()
        .expect("the test runtime should retain its background demand");
    let (promise, completion, effect) = context.values().with_runtime_value_access(|access| {
        let completion = access
            .construct_rooted_managed_promise("dropped reflection activation")
            .expect("the managed completion promise must fit its reviewed slot");
        let promise = PromisedValue::from_root(&completion, &access);
        let effect = access.root_runtime_value(n(0));
        (promise, completion, effect)
    });

    let reservation = background
        .reserve_reflection_completion_activation(
            effect,
            None,
            completion,
            ReflectionTaskResultPolicy::ReturnValue,
        )
        .expect("the autonomous reflection task should reserve");
    drop(reservation);

    let failure = promise
        .assignment(context.values())
        .expect("dropping the permit must settle the promise")
        .expect_err("an unactivated task must fail its promise");
    assert_eq!(failure.to_string(), "reflection result task was cancelled");
    assert_eq!(builds.load(Ordering::SeqCst), 0);
    assert_eq!(background.task_registry_counts().reflection_active, 0);
}

#[test]
fn completed_reflection_source_retains_no_external_owner_or_value_domain_cycle() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let domain = Arc::downgrade(values.value_domain());
    {
        let context = EvalContext::isolated(values.clone());
        context
            .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
                terminal: FixtureTaskTerminal::Complete(n(42)),
                builds: Arc::new(AtomicUsize::new(0)),
                result_policies: Arc::new(Mutex::new(Vec::new())),
            }))
            .expect("fresh retirement fixture should accept its reflection launcher");
        let value = Value::reflection_task_result(&values, n(0));
        assert_eq!(eval_value(&context, &value).unwrap(), n(42));
        assert_eq!(
            values.external_owner_count_for_test(),
            0,
            "reflection sources no longer allocate external-owner sidecars"
        );
    }
    drop(values);
    assert!(
        domain.upgrade().is_none(),
        "a completed autonomous reflection task must not retain its value domain"
    );
}
#[test]
fn reflection_task_result_returns_arbitrary_lazy_value_once() {
    let context = test_context();
    let result_forces = Arc::new(AtomicUsize::new(0));
    let counted_result_forces = result_forces.clone();
    let result = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "returned reflection result",
        move |_| {
            counted_result_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(42))
        },
    );
    let builds = Arc::new(AtomicUsize::new(0));
    let result_policies = Arc::new(Mutex::new(Vec::new()));
    context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(result),
            builds: builds.clone(),
            result_policies: result_policies.clone(),
        }))
        .expect("fresh test session should accept its reflection launcher");

    let computation = Value::reflection_task_result(&crate::core::test_value_factory(), n(0));
    let copy = computation.clone();
    assert_eq!(eval_value(&context, &computation).unwrap(), n(42));
    assert_eq!(eval_value(&context, &copy).unwrap(), n(42));
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    assert_eq!(result_forces.load(Ordering::SeqCst), 1);
    assert_eq!(
        *result_policies
            .lock()
            .expect("fixture result policies were poisoned"),
        [ReflectionTaskResultPolicy::ReturnValue]
    );
}

#[test]
fn reflection_task_result_survives_first_session_close_and_returns_completion_value() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let computation = Value::reflection_task_result(owner.values(), n(0));
    let blocked = eval_value(&owner, &computation)
        .expect_err("an unlaunched reflection result task should block");
    let owner_wait = blocked
        .blocked_on()
        .expect("the result computation should expose its stable wait");

    let cross_session = eval_value(&observer, &computation)
        .expect_err("a cross-session observer should follow the owner task");
    assert!(cross_session.blocked_on().is_some());
    assert_eq!(
        observer
            .for_runtime_background()
            .expect("the runtime should retain its background demand")
            .task_registry_counts()
            .reflection_active,
        1,
        "both observer sessions must share one autonomous task"
    );

    drop(owner);
    let resumed = eval_value(&observer, &computation)
        .expect_err("the later session should resume the lazy-owned promise checkpoint");
    let resumed_wait = resumed
        .blocked_on()
        .expect("the resumed checkpoint should expose its autonomous task dependency");
    assert_ne!(
        resumed_wait, owner_wait,
        "closing the first demand session retires its route without changing the autonomous task"
    );
    observer.complete_wait_with_value(&resumed_wait.0, n(43));
    assert_eq!(eval_value(&observer, &computation).unwrap(), n(43));
}

#[test]
fn unobserved_reflection_failure_remains_reportable_until_promise_propagation() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let failure = Arc::new(EvaluationFailure::message(
        "unobserved autonomous reflection failure",
    ));
    owner
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Failed(failure),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh runtime should accept its reflection launcher");
    let background = owner
        .for_runtime_background()
        .expect("the runtime should retain its background demand");
    let (promise, completion, effect) = owner.values().with_runtime_value_access(|access| {
        let completion = access
            .construct_rooted_managed_promise("unobserved reflection failure")
            .expect("the completion promise must fit its reviewed slot");
        let promise = PromisedValue::from_root(&completion, &access);
        let effect = access.root_runtime_value(n(0));
        (promise, completion, effect)
    });
    background
        .reserve_reflection_completion_activation(
            effect,
            None,
            completion,
            ReflectionTaskResultPolicy::ReturnValue,
        )
        .expect("the autonomous reflection task should reserve")
        .activate();

    assert!(matches!(
        background.run_until_quiescent(),
        crate::evaluation::EvaluationSessionRun::Complete(_)
    ));
    assert_eq!(
        background.task_registry_counts().unacknowledged_failures,
        1,
        "an unobserved autonomous failure remains reportable"
    );

    let propagated = eval_value(&observer, &Value::Promised(promise))
        .expect_err("observing the completion promise should propagate the task failure");
    assert_eq!(
        propagated.to_string(),
        "unobserved autonomous reflection failure"
    );
    assert_eq!(
        background.task_registry_counts().unacknowledged_failures,
        0,
        "promise propagation transfers reporting responsibility exactly once"
    );
}

#[test]
fn reflection_task_result_preserves_failure_and_transfers_reporting_responsibility() {
    let context = test_context();
    let producer_frame = evaluation_context_frame("reflection_result_producer");
    let failure = Arc::new(
        EvaluationFailure::message("reflection result failed").with_context(producer_frame.clone()),
    );
    context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Failed(failure),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");

    let error = eval_value(
        &context,
        &Value::reflection_task_result(&crate::core::test_value_factory(), n(0)),
    )
    .expect_err("a failed result task must fail its lazy consumer");
    assert_eq!(error.to_string(), "reflection result failed");
    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("reflection_task"), producer_frame,]
    );
    assert_eq!(
        context.task_registry_counts().unacknowledged_failures,
        0,
        "propagating the failure must remove detached-task reporting responsibility"
    );
}

#[test]
fn reflection_task_result_propagates_cancellation() {
    let context = test_context();
    context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Cancelled,
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");

    let error = eval_value(
        &context,
        &Value::reflection_task_result(&crate::core::test_value_factory(), n(0)),
    )
    .expect_err("a cancelled result task must fail its lazy consumer");
    assert_eq!(error.to_string(), "reflection result task was cancelled");
}

#[test]
fn reflection_gate_waits_before_continuing_target_demand() {
    let context = test_context();
    let forced = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let forced_by_target = forced.clone();
    let target = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "reflection target",
        move |_| {
            forced_by_target.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(42))
        },
    );
    let gate = reflection_annotation(&context, n(0), target.clone());
    let Value::Lazy(gate_lazy) = &gate else {
        panic!("a reflection annotation should construct a lazy gate")
    };
    let gate_root = gate_lazy.root(context.values());

    assert_eq!(context.reflection_task_count(), 0);
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 0);
    context
        .values()
        .collect_managed_for_test()
        .expect("the waiting reflection gate must remain traced");

    let first = eval_value(&context, &gate).expect_err("new reflection task should block");
    let wait = first
        .blocked_on()
        .expect("gate should report its task wait");
    let second = eval_value(&context, &gate).expect_err("queued reflection task should block");

    assert_eq!(second.blocked_on(), Some(wait.clone()));
    assert_eq!(
        context.reflection_task_count(),
        0,
        "autonomous reflection work belongs to the runtime background domain"
    );
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 0);

    context.complete_wait(&wait.0);
    assert_eq!(
        eval_value(&context, &gate).expect("completed gate should continue target demand"),
        n(42)
    );
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(eval_value(&context, &gate).unwrap(), n(42));
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 1);
    drop(gate_root);
}

#[test]
fn running_reflection_gate_blocks_an_observer_session_without_poisoning_its_cache() {
    let (owner, observer, _executor) = same_runtime_contexts();
    let gate = reflection_annotation(&owner, n(0), n(42));
    let Value::Lazy(gate_lazy) = &gate else {
        panic!("reflection annotation should produce a lazy gate")
    };
    let blocked = eval_value(&owner, &gate).expect_err("new reflection task should block");

    let cross_session =
        eval_value(&observer, &gate).expect_err("cross-session gate task should block");
    assert!(cross_session.blocked_on().is_some());
    assert_eq!(
        gate_lazy.cached(owner.values()),
        None,
        "a live cross-session dependency must not become a permanent lazy failure"
    );

    owner.complete_wait(&blocked.blocked_on().unwrap().0);
    assert_eq!(eval_value(&observer, &gate).unwrap(), n(42));
}

#[test]
fn reflection_gate_memoizes_task_failure() {
    let context = test_context();
    let gate = reflection_annotation(&context, n(0), n(42));
    let blocked = eval_value(&context, &gate).expect_err("new reflection task should block");
    let wait = blocked
        .blocked_on()
        .expect("gate should report its task wait");

    context.fail_wait(&wait.0, "reflection task failed deliberately");

    let first = eval_value(&context, &gate).unwrap_err();
    assert_eq!(first.to_string(), "reflection task failed deliberately");
    assert_eq!(
        failure_context_items(&first),
        [evaluation_context_frame("reflection_annotation")]
    );
    let second = eval_value(&context, &gate).unwrap_err();
    assert_eq!(second.to_string(), "reflection task failed deliberately");
    assert_eq!(
        failure_context_items(&second),
        [evaluation_context_frame("reflection_annotation")]
    );
}

fn assert_structured_reflection_gate_failure(stage: GateFailureStage) {
    // This fixture forces collection explicitly, so it must not share the
    // process-wide test value domain with unrelated parallel tests.
    let context = isolated_test_context();
    let detail = Key::atom_from_text("detail");
    let emission = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::MSG).clone(),
                Value::Dict(Dict::new_sync().insert(
                    (*keys::TEXT).clone(),
                    Value::binary_from_text("structured gate failure"),
                )),
            )
            .insert(detail.clone(), n(7)),
    );
    let producer_frame = evaluation_context_frame("gate_producer");
    let failure = Arc::new(
        EvaluationFailure::emission(emission.clone()).with_context(producer_frame.clone()),
    );
    let builds = Arc::new(AtomicUsize::new(0));
    context
        .install_reflection_launcher(Arc::new(GateFailureLauncher {
            failure,
            stage,
            builds: builds.clone(),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let gate = reflection_annotation(&context, n(0), n(42));
    let Value::Lazy(gate_lazy) = &gate else {
        panic!("reflection annotation should produce a lazy gate")
    };
    let gate_root = gate_lazy.root(context.values());
    context
        .values()
        .collect_managed_for_test()
        .expect("the rooted reflection gate must survive collection before launch");

    let first = eval_value(&context, &gate)
        .expect_err("the reflection gate should retain its structured task failure")
        .into_permanent_failure();
    context
        .values()
        .collect_managed_for_test()
        .expect("the terminal gate cache must survive collection after launch");
    assert_eq!(first.emission_value(), Some(&emission));
    assert_eq!(
        first.contexts(),
        [
            evaluation_context_frame("reflection_annotation"),
            producer_frame,
        ]
    );
    let cached = gate_lazy
        .cached(context.values())
        .expect("the failed gate should have a terminal lazy cache")
        .expect_err("the terminal gate cache should contain its failure");
    assert!(Arc::ptr_eq(&first, &cached));

    let second = eval_value(&context, &gate)
        .expect_err("the reflection gate should reuse its cached failure")
        .into_permanent_failure();
    assert!(Arc::ptr_eq(&cached, &second));
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    let Value::Dict(diagnostic) = failure_diagnostic_value(&second) else {
        panic!("structured gate failure should project to a diagnostic dictionary")
    };
    assert_eq!(diagnostic.get(&detail), Some(&n(7)));
    assert_eq!(
        context.task_registry_counts().unacknowledged_failures,
        0,
        "a propagated gate failure must not remain a detached task failure"
    );
    drop(gate_root);
}

#[test]
fn reflection_gate_preserves_structured_launcher_construction_failure() {
    assert_structured_reflection_gate_failure(GateFailureStage::LauncherConstruction);
}

#[test]
fn reflection_gate_preserves_structured_post_launch_failure() {
    assert_structured_reflection_gate_failure(GateFailureStage::TaskPoll);
}

#[test]
fn reflection_gate_blocks_and_resumes_the_exact_net_call() {
    let context = test_context();
    let identity = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    let gate = reflection_annotation(&context, n(0), Value::Net(identity));
    let applied = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        let function = builder.data(gate);
        let value = builder.data(n(42));
        builder.wire(application, function);
        builder.wire(argument, value);
        result
    });
    let runtime = applied.runtime().duplicate_for_test(context.values());

    let computation = Value::Lazy(LazyValue::from_net_computation(
        &crate::core::test_value_factory(),
        applied,
    ));
    let blocked =
        eval_value(&context, &computation).expect_err("call should wait for its reflection gate");
    let wait = blocked
        .blocked_on()
        .expect("call should report a task wait");
    assert_eq!(
        runtime.test_with(&crate::core::test_value_factory(), |net| net
            .active_pairs()
            .filter(|pair| net.blocked_callable_checkpoint(*pair).is_some())
            .count()),
        1
    );
    assert_eq!(
        runtime.active_normalization_batch(&crate::core::test_value_factory()),
        None,
        "specialization waits must not retain a net batch lease"
    );

    context.complete_wait(&wait.0);
    let observer = test_context();
    let resumed = Value::Lazy(LazyValue::from_net_computation(
        &crate::core::test_value_factory(),
        NetValue::new(runtime),
    ));
    assert_eq!(eval_value(&observer, &resumed).unwrap(), n(42));
}

#[test]
fn reflection_gate_blocks_and_resumes_an_exact_net_function_call() {
    let context = test_context();
    let function = closed_function_value(1, TestExpr::Local(0));
    let gate = reflection_annotation(&context, n(0), function);
    let applied = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        let function = builder.data(gate);
        let value = builder.data(n(42));
        builder.wire(application, function);
        builder.wire(argument, value);
        result
    });
    let runtime = applied.runtime().duplicate_for_test(context.values());
    let computation = Value::Lazy(LazyValue::from_net_computation(context.values(), applied));

    let blocked = eval_value(&context, &computation)
        .expect_err("call should wait while its function remains behind a reflection gate");
    let wait = blocked
        .blocked_on()
        .expect("function call should report the gate's exact task wait");
    assert_eq!(
        runtime.test_with(&crate::core::test_value_factory(), |net| net
            .active_pairs()
            .filter(|pair| net.blocked_callable_checkpoint(*pair).is_some())
            .count()),
        1
    );
    assert_eq!(
        runtime.active_normalization_batch(&crate::core::test_value_factory()),
        None,
        "the blocked callable must not retain a net batch lease"
    );

    context.complete_wait(&wait.0);
    let resumed = Value::Lazy(LazyValue::from_net_computation(
        context.values(),
        NetValue::new(runtime),
    ));
    let application = eval_value(&context, &resumed)
        .expect("completed gate should expose the function application");
    assert_eq!(eval_value(&context, &application).unwrap(), n(42));
}

#[test]
fn reflection_gate_blocks_and_resumes_the_exact_net_operator_call() {
    let context = test_context();
    let key = Key::atom_from_text("answer");
    let target = closed_function_value(1, TestExpr::Value(n(42)));
    let gate = reflection_annotation(&context, n(0), target);
    let applied = closed_net(|builder| {
        let operator = context
            .values()
            .with_runtime_value_access(|access| applicable_operator(&access, gate));
        let [input, result] = builder.operator(operator);
        let argument = builder.data(key.to_value_with(context.values()));
        builder.wire(input, argument);
        result
    });
    let runtime = applied.runtime().duplicate_for_test(context.values());
    let pair = runtime
        .test_with(&crate::core::test_value_factory(), |net| {
            net.active_pairs().next()
        })
        .expect("operator call should start ready");
    let computation = Value::Lazy(LazyValue::from_net_computation(context.values(), applied));

    let blocked = eval_value(&context, &computation)
        .expect_err("operator should wait for its reflection gate");
    let wait = blocked
        .blocked_on()
        .expect("operator should report its exact task wait");
    assert!(
        runtime
            .test_with(&crate::core::test_value_factory(), |net| net
                .operator_call(pair))
            .is_none(),
        "operator application should hand demand to the emitted lazy value"
    );
    assert_eq!(
        runtime.active_normalization_batch(&crate::core::test_value_factory()),
        None,
        "operator evaluation must begin after normalization closes"
    );
    assert!(matches!(
        context.poll_wait(&wait.0),
        EvaluationWaitPoll::Pending(_)
    ));

    context.complete_wait(&wait.0);
    assert_eq!(eval_value(&context, &computation).unwrap(), n(42));
}

#[test]
fn builtins_are_curried_and_do_not_force_arguments_early() {
    let unforced = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "unforced builtin argument",
        |_| panic!("partial builtin application forced its first argument"),
    );
    let partial = apply_values(
        &test_context(),
        Value::Builtin(Builtin::Append),
        vec![unforced],
    )
    .expect("partial builtin application should accept its first argument");
    let partial = eval_value(&test_context(), &partial)
        .expect("partial builtin application should produce a partial builtin");

    match partial {
        Value::PartialBuiltin(call) => {
            assert_eq!(call.builtin, Builtin::Append);
            assert_eq!(call.arguments.len(), 1);
            assert!(matches!(&call.arguments[0], Value::Lazy(_)));
        }
        other => panic!("expected partial builtin, got {other:?}"),
    }
}

fn evaluate_strategy(
    context: &EvalContext,
    builtin: Builtin,
    first: Value,
    target: Value,
) -> Result<Value, EvaluationHalt> {
    context.evaluate_builtin_whnf(builtin, vec![first, target])
}

#[test]
fn seq_forces_its_first_argument_before_continuing_target_demand() {
    let context = test_context();
    let error = evaluate_strategy(
        &context,
        Builtin::Seq,
        Value::error(&crate::core::test_value_factory(), "seq forced this error"),
        n(42),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "seq forced this error");

    let target_forces = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted_target_forces = target_forces.clone();
    let target = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "seq target",
        move |_| {
            counted_target_forces.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(42))
        },
    );
    let result = apply_values(&context, Value::Builtin(Builtin::Seq), vec![n(0), target]).unwrap();

    assert_eq!(target_forces.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(eval_value(&context, &result).unwrap(), n(42));
    assert_eq!(target_forces.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn zero_worker_spark_returns_target_without_forcing_work() {
    let context = test_context();
    let unforced = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "discarded spark",
        |_| panic!("zero-worker spark should be silently dropped"),
    );
    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Spark),
        vec![unforced, n(42)],
    )
    .unwrap();

    assert_eq!(eval_value(&context, &result).unwrap(), n(42));
}

#[test]
fn strategies_demand_hidden_metadata_without_exposing_the_carrier() {
    let context = test_context();
    let metadata_forces = Arc::new(AtomicUsize::new(0));
    let counted_metadata_forces = metadata_forces.clone();
    let metadata = Value::semantic_thunk(context.values(), "sequenced metadata", move |_| {
        counted_metadata_forces.fetch_add(1, Ordering::SeqCst);
        Ok(n(7))
    });
    let carrier = Value::metadata_carrier(metadata);
    let target_forces = Arc::new(AtomicUsize::new(0));
    let counted_target_forces = target_forces.clone();
    let target = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "metadata sequence target",
        move |_| {
            counted_target_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(42))
        },
    );

    let result = evaluate_strategy(&context, Builtin::Seq, carrier, target)
        .expect("seq should successfully demand hidden metadata");
    assert_eq!(metadata_forces.load(Ordering::SeqCst), 1);
    assert_eq!(result, n(42));
    assert_eq!(target_forces.load(Ordering::SeqCst), 1);
}

#[test]
fn zero_worker_spark_discards_hidden_metadata_demand() {
    let context = test_context();
    let metadata = Value::semantic_thunk(
        &crate::core::test_value_factory(),
        "discarded metadata spark",
        |_| panic!("zero-worker spark must not demand hidden metadata"),
    );
    let carrier = Value::metadata_carrier(metadata);
    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Spark),
        vec![carrier, n(42)],
    )
    .expect("spark should return its target with no workers");

    assert_eq!(eval_value(&context, &result).unwrap(), n(42));
}

fn wait_for_lazy_cache(context: &EvalContext, lazy: &LazyValue, message: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while lazy.cached(context.values()).is_none() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(lazy.cached(context.values()).is_some(), "{message}");
}

fn wait_for_no_deferred_tasks(context: &EvalContext, message: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while context.task_registry_counts().deferred_active != 0
        && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert_eq!(
        context.task_registry_counts().deferred_active,
        0,
        "{message}"
    );
}

fn wait_for_blocked_sparks(
    coordinator: &crate::evaluation::EvaluationWorkCoordinator,
    expected: usize,
    message: &str,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while coordinator.spark_work_counts().2 != expected && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(coordinator.spark_work_counts().2, expected, "{message}");
}

#[test]
fn worker_spark_demands_metadata_behind_a_lazy_carrier_shell() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let context = EvalContext::new(&session);
    let (shell_sender, shell_receiver) = std::sync::mpsc::channel();
    let (metadata_sender, metadata_receiver) = std::sync::mpsc::channel();
    let metadata = Value::semantic_thunk(context.values(), "worker metadata", move |_| {
        metadata_sender
            .send(())
            .expect("metadata receiver should remain open");
        Ok(n(7))
    });
    let carrier = Value::metadata_carrier(metadata);
    let lazy_carrier = Value::semantic_thunk(
        context.values(),
        "lazy worker metadata carrier",
        move |_| {
            shell_sender
                .send(())
                .expect("carrier receiver should remain open");
            Ok(carrier.clone())
        },
    );

    let result = evaluate_strategy(&context, Builtin::Spark, lazy_carrier, n(42))
        .expect("spark should immediately return its target");
    assert_eq!(result, n(42));
    shell_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker should demand the carrier shell");
    metadata_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker should continue into the hidden metadata");
}

#[test]
fn metadata_strategy_failures_are_cached_and_seq_propagates_them() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let context = EvalContext::new(&session);
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted_attempts = attempts.clone();
    let (attempt_sender, attempt_receiver) = std::sync::mpsc::channel();
    let metadata =
        LazyValue::semantic_thunk(context.values(), "failing metadata strategy", move |_| {
            counted_attempts.fetch_add(1, Ordering::SeqCst);
            attempt_sender
                .send(())
                .expect("attempt receiver should remain open");
            Err(EvaluationHalt::new("metadata strategy failed"))
        });
    let carrier = Value::metadata_carrier(Value::Lazy(metadata.clone()));

    let result = evaluate_strategy(&context, Builtin::Spark, carrier.clone(), n(42))
        .expect("detached metadata failure must not replace the spark target");
    assert_eq!(result, n(42));
    attempt_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker should demand the failing metadata");
    wait_for_lazy_cache(
        &context,
        &metadata,
        "the worker must cache the terminal metadata failure",
    );

    let error = evaluate_strategy(&context, Builtin::Seq, carrier, n(43))
        .expect_err("seq must propagate the cached hidden failure");
    assert_eq!(error.to_string(), "metadata strategy failed");
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn strategies_stop_at_nested_metadata_carriers() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let context = EvalContext::new(&session);
    let hidden_forces = Arc::new(AtomicUsize::new(0));
    let counted_hidden_forces = hidden_forces.clone();
    let hidden = Value::semantic_thunk(context.values(), "nested hidden metadata", move |_| {
        counted_hidden_forces.fetch_add(1, Ordering::SeqCst);
        Ok(n(7))
    });
    let outer = Value::metadata_carrier(Value::metadata_carrier(hidden));

    assert_eq!(
        evaluate_strategy(&context, Builtin::Seq, outer.clone(), n(42))
            .expect("seq should stop after demanding one hidden metadata value"),
        n(42)
    );
    assert_eq!(hidden_forces.load(Ordering::SeqCst), 0);

    context.spark(outer);
    let (finished_sender, finished_receiver) = std::sync::mpsc::channel();
    let sentinel = LazyValue::semantic_thunk(
        context.values(),
        "nested metadata spark sentinel",
        move |_| {
            finished_sender
                .send(())
                .expect("sentinel receiver should remain open");
            Ok(unit_value())
        },
    );
    context.spark(Value::Lazy(sentinel.clone()));
    finished_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker should finish the preceding metadata spark");
    wait_for_lazy_cache(&context, &sentinel, "worker must finish the sentinel spark");
    assert_eq!(
        hidden_forces.load(Ordering::SeqCst),
        0,
        "spark must share seq's single metadata-boundary demand"
    );
}

#[test]
fn spark_admission_drops_whnf_and_follows_completed_promises() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let context = EvalContext::new(&session);
    let net = closed_net(|builder| builder.data(n(1)));
    context.spark(Value::Net(net));
    context.spark(Value::Promised(PromisedValue::new(
        context.values(),
        "unassigned spark input",
    )));
    let promised_forces = Arc::new(AtomicUsize::new(0));
    let counted_promised_forces = promised_forces.clone();
    let promised_work =
        LazyValue::semantic_thunk(context.values(), "promised spark work", move |_| {
            counted_promised_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(7))
        });
    let promise = PromisedValue::new(context.values(), "resolved spark input");
    set_promise(&context, &promise, Value::Lazy(promised_work.clone()))
        .expect("test promise should accept its one assignment");
    context.spark(Value::Promised(promise));

    let (finished_sender, finished_receiver) = std::sync::mpsc::channel();
    let sentinel =
        LazyValue::semantic_thunk(context.values(), "spark admission sentinel", move |_| {
            finished_sender
                .send(())
                .expect("sentinel receiver should remain open");
            Ok(unit_value())
        });
    context.spark(Value::Lazy(sentinel.clone()));
    finished_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker should process the earlier spark jobs first");
    wait_for_lazy_cache(&context, &sentinel, "worker must finish the sentinel spark");
    wait_for_lazy_cache(
        &context,
        &promised_work,
        "worker must finish useful work exposed through the completed promise",
    );
    wait_for_no_deferred_tasks(
        &context,
        "completed spark jobs must retire their deferred records",
    );

    let counts = context.task_registry_counts();
    assert_eq!(
        promised_forces.load(Ordering::SeqCst),
        1,
        "a worker should pursue useful lazy work through a completed promise"
    );
    assert!(promised_work.cached(context.values()).is_some());
    assert_eq!(counts.deferred_active, 0);
    assert_eq!(counts.promises_active, 0);
}

#[test]
fn spark_resumes_after_a_resolver_owned_promise_completes() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let context = EvalContext::new(&session);
    let promise = PromisedValue::new(context.values(), "later spark input");
    context.spark(Value::Promised(promise.clone()));
    wait_for_blocked_sparks(
        &coordinator,
        1,
        "an unassigned promise should retain its spark demand",
    );

    let (forced_sender, forced_receiver) = std::sync::mpsc::channel();
    let assigned = LazyValue::semantic_thunk(context.values(), "resolved spark work", move |_| {
        forced_sender
            .send(())
            .expect("spark result receiver should remain open");
        Ok(n(7))
    });
    set_promise(&context, &promise, Value::Lazy(assigned.clone()))
        .expect("promise should accept its one assignment");

    forced_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("promise completion should resume and finish its spark");
    wait_for_lazy_cache(&context, &assigned, "resumed spark work must be cached");
    wait_for_blocked_sparks(
        &coordinator,
        0,
        "a completed spark must leave no parked executor job",
    );
    wait_for_no_deferred_tasks(
        &context,
        "promise and lazy followers must retire after spark completion",
    );
}

#[test]
fn dropping_a_session_discards_its_blocked_sparks() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    {
        let session = crate::evaluation::EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(&session);
        context.spark(Value::Promised(PromisedValue::new(
            context.values(),
            "discarded blocked spark",
        )));
        wait_for_blocked_sparks(
            &coordinator,
            1,
            "unassigned promise should park before its session is dropped",
        );
    }
    wait_for_blocked_sparks(
        &coordinator,
        0,
        "parked spark values must not outlive their evaluation session",
    );
}

#[test]
fn metadata_seq_preserves_retryable_promise_blockage() {
    let context = test_context();
    let (promise, _owner_task, _owner) = context
        .task_owned_promise(Arc::from("blocked metadata"))
        .unwrap();
    let observer = context.with_new_task().unwrap();
    let carrier = Value::metadata_carrier(Value::Promised(promise.clone()));

    let applied = apply_values(
        &observer,
        Value::Builtin(Builtin::Seq),
        vec![carrier.clone(), n(42)],
    )
    .expect("strategy application should remain lazy");
    let blocked = eval_value(&observer, &applied)
        .expect_err("seq should block on unresolved hidden metadata");
    assert!(blocked.blocked_on().is_some());

    set_promise(&observer, &promise, n(7)).unwrap();
    assert_eq!(
        eval_value(&observer, &applied).expect("seq should resume after hidden metadata completes"),
        n(42)
    );
}

#[test]
fn completed_metadata_updates_release_sources_and_task_records() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let context = EvalContext::isolated(values.clone());
    let prior_source_dropped = Arc::new(AtomicBool::new(false));
    let prior_signal = DropSignal(prior_source_dropped.clone());
    let prior = Value::semantic_thunk(&values, "discardable prior metadata", move |_| {
        let _keep_signal_captured = &prior_signal;
        Ok(n(1))
    });
    let outputs = run_metadata_update(
        &context,
        closed_function_value_in(
            &values,
            1,
            TestExpr::Value(Value::List(List::from_values(vec![n(7)]))),
        ),
        vec![Value::metadata_carrier(prior)],
    )
    .expect("metadata update should remain lazy");
    assert!(
        !prior_source_dropped.load(Ordering::Acquire),
        "the unresolved update must retain its prior metadata input"
    );

    let result = evaluate_strategy(&context, Builtin::Seq, outputs[0].clone(), n(42))
        .expect("seq should complete the derived metadata");
    assert_eq!(result, n(42));
    context
        .values()
        .collect_managed_for_test()
        .expect("unreachable metadata inputs should be collectible after completion");
    assert!(
        prior_source_dropped.load(Ordering::Acquire),
        "a completed update which ignores its input should release the prior metadata graph"
    );
    let counts = context.task_registry_counts();
    assert_eq!(counts.deferred_active, 0);
    assert_eq!(counts.deferred_terminal, 0);
    assert_eq!(counts.deferred_by_wait, 0);
    assert_eq!(counts.deferred_by_task, 0);
}

#[test]
fn strategy_annotations_share_builtin_semantics() {
    let context = test_context();
    let seq_annotation = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("seq"),
        Value::error(&crate::core::test_value_factory(), "annotation forced"),
    ));
    let error = eval_value(
        &context,
        &apply_values(
            &context,
            Value::Builtin(Builtin::Anno),
            vec![seq_annotation, n(42)],
        )
        .expect("annotation application should remain lazy"),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "annotation forced");

    let spark_annotation = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("spark"),
        Value::semantic_thunk(
            &crate::core::test_value_factory(),
            "discarded annotated spark",
            |_| panic!("zero-worker annotated spark should be silently dropped"),
        ),
    ));
    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![spark_annotation, n(42)],
    )
    .unwrap();
    assert_eq!(eval_value(&context, &result).unwrap(), n(42));
}
