use std::sync::Arc;

use bytes::Bytes;

use crate::core::{Dict, FixpointComputation, LazyValue, Value};
use crate::number::Number;

use super::*;

fn closed_net(build: impl FnOnce(&mut NetBuilder<CoreSpecialization>) -> Port) -> NetValue {
    let mut builder = NetBuilder::new();
    let exposed = build(&mut builder);
    NetValue::new(builder.finish(exposed).instantiate_shared())
}

fn fixture_computation(expr: TestExpr) -> Value {
    lower_test_computation_value(expr)
}

fn apply_test_values(function: Value, arguments: impl IntoIterator<Item = Value>) -> Value {
    apply_values(&test_context(), function, arguments.into_iter().collect())
        .expect("test application should accept a callable value")
}

#[test]
fn closed_net_values_can_expose_ordinary_data_repeatedly() {
    let net = closed_net(|builder| builder.data(n(42)));
    let value = Value::Net(net);

    assert_eq!(eval_value(&test_context(), &value).unwrap(), n(42));
    assert_eq!(eval_value(&test_context(), &value).unwrap(), n(42));
}

#[test]
fn closed_net_values_attach_to_applications_through_cursors() {
    let identity = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    let expression = TestExpr::Apply(
        Arc::new(TestExpr::Value(Value::Net(identity))),
        Arc::new(TestExpr::Value(n(42))),
    );

    assert_eq!(eval_closed_expr(&expression).unwrap(), n(42));
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
    let value = Value::Lazy(LazyValue::from_net_computation(identity));

    assert_eq!(
        eval_value(&test_context(), &value).unwrap_err().to_string(),
        "lazy net computation exposed a bind instead of data"
    );
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
fn explicit_net_application_may_return_a_residual_bind() {
    let two_argument_net = closed_net(|builder| {
        let spine = builder.bind_spine(2);
        for argument in &spine.arguments {
            let eraser = builder.copy(0);
            builder.wire(*argument, eraser.input);
        }
        let result = builder.data(n(42));
        builder.wire(spine.result, result);
        spine.input
    });

    assert!(matches!(
        apply_net(&test_context(), two_argument_net, n(0)).unwrap(),
        Value::Net(_)
    ));
}

#[test]
fn zero_arity_apply_operator_is_data_identity() {
    let operator = apply_arity_operator(0, Arc::from([]));
    let data = n(42);

    assert_eq!(
        apply_core_operator(&test_context(), &operator, &data).unwrap(),
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
    assert!(first.stage().runtime().ptr_eq(second.stage().runtime()));
}

#[test]
fn curried_function_partial_application_exposes_the_next_bind() {
    let function = closed_function_value(3, TestExpr::Local(2));
    let partially_applied = eval_value(&test_context(), &apply_test_values(function, [n(11)]))
        .expect("first application should expose the remaining bind chain");
    let Value::Function(first_stage) = &partially_applied else {
        panic!("partial application should produce another function stage");
    };
    assert_eq!(first_stage.remaining_arity(), 2);
    let cloned_stage = partially_applied.clone();
    let Value::Function(cloned_stage) = cloned_stage else {
        unreachable!()
    };
    assert!(
        first_stage
            .stage()
            .runtime()
            .ptr_eq(cloned_stage.stage().runtime())
    );

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
        Value::deferred(label, move |_| {
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
    let function = closed_function_value(1, TestExpr::Value(Value::error("unreached body")));

    assert!(matches!(function, Value::Function(_)));
}

fn n(value: i64) -> Value {
    Value::Number(value.into())
}

#[test]
fn promised_lazy_values_fail_fast_until_initialized() {
    let promised = LazyValue::promised("test promised value");
    let value = Value::Lazy(promised.clone());

    assert_eq!(
        eval_value(&test_context(), &value).unwrap_err().to_string(),
        "promised value was observed before initialization"
    );
    promised.set(n(42)).unwrap();
    assert_eq!(eval_value(&test_context(), &value).unwrap(), n(42));
}

#[test]
fn task_owned_fixpoint_rejects_recursive_demand_and_blocks_other_tasks() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let observer = session.with_new_task().unwrap();
    let fixpoint = LazyValue::fixpoint(&owner, "test fixpoint").unwrap();
    let value = Value::Lazy(fixpoint.clone());

    let recursive = eval_value(&owner, &value).unwrap_err();
    assert!(
        recursive
            .to_string()
            .contains("recursively observed itself")
    );

    let blocked = eval_value(&observer, &value).unwrap_err();
    assert!(blocked.blocked_on().is_some());

    fixpoint.set(n(42)).unwrap();
    assert_eq!(eval_value(&observer, &value).unwrap(), n(42));
}

#[test]
fn failed_task_fails_its_unresolved_fixpoint_promises() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let observer = session.with_new_task().unwrap();
    let fixpoint = LazyValue::fixpoint(&owner, "test fixpoint").unwrap();
    let value = Value::Lazy(fixpoint);

    assert!(
        eval_value(&observer, &value)
            .unwrap_err()
            .blocked_on()
            .is_some()
    );
    owner.fail_unresolved_promises("producer failed deliberately");
    assert_eq!(
        eval_value(&observer, &value).unwrap_err().to_string(),
        "producer failed deliberately"
    );
}

#[test]
fn value_fixpoint_reports_recursive_self_observation() {
    let context = test_context();
    let function = closed_function_value(1, TestExpr::Local(0));
    let fixpoint = Value::Lazy(
        LazyValue::computed_fixpoint(
            "recursive value fixpoint",
            FixpointComputation::Function(function),
        )
        .unwrap(),
    );

    let error = eval_value(&context, &fixpoint).unwrap_err();
    assert!(
        error.to_string().contains("recursively observed itself"),
        "{error}"
    );
    assert!(error.blocked_on().is_none());

    let observer = context.with_new_task().unwrap();
    assert_eq!(
        eval_value(&observer, &fixpoint).unwrap_err().to_string(),
        error.to_string()
    );
}

#[test]
fn fixpoint_builtin_uses_task_owned_recursive_observation() {
    let expression = builtin1_expr(Builtin::Fixpoint, function_expr(1, TestExpr::Local(0)));

    let error = eval_closed_expr(&expression).unwrap_err();
    assert!(
        error.to_string().contains("recursively observed itself"),
        "{error}"
    );
    assert!(error.blocked_on().is_none());
}

#[test]
fn suspended_value_fixpoint_keeps_one_knot_for_concurrent_observers() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let observer = session.with_new_task().unwrap();
    let gate = reflection_annotation(&owner, n(0), n(42));
    let function = closed_function_value(1, TestExpr::Value(gate));
    let fixpoint = Value::Lazy(
        LazyValue::computed_fixpoint(
            "suspended value fixpoint",
            FixpointComputation::Function(function),
        )
        .unwrap(),
    );

    let producer_block = eval_value(&owner, &fixpoint).unwrap_err();
    let producer_wait = producer_block
        .blocked_on()
        .expect("producer should suspend on its reflection gate");
    let observer_block = eval_value(&observer, &fixpoint).unwrap_err();
    let fixpoint_wait = observer_block
        .blocked_on()
        .expect("observer should wait on the fixpoint itself");
    assert_ne!(producer_wait, fixpoint_wait);

    owner.complete_wait(&producer_wait.0);
    assert_eq!(eval_value(&owner, &fixpoint).unwrap(), n(42));
    assert_eq!(eval_value(&observer, &fixpoint).unwrap(), n(42));
}

#[test]
fn deferred_values_use_the_context_that_forces_them() {
    let context = test_context();
    let expected_context = context.clone();
    let value = Value::deferred("context-sensitive test value", move |actual_context| {
        assert!(actual_context.shares_session_with(&expected_context));
        Ok(n(42))
    });

    assert_eq!(eval_value(&context, &value).unwrap(), n(42));
}

#[test]
fn ready_lazy_errors_fail_when_observed() {
    let value = Value::error("deliberate failure");

    assert_eq!(
        eval_value(&test_context(), &value).unwrap_err().to_string(),
        "deliberate failure"
    );
}

fn function_expr(arity: usize, body: TestExpr) -> TestExpr {
    let code = Arc::new(lower_test_function_code(arity, body));
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

    assert_eq!(saw_values, vec![vec![n(1)], vec![n(2)]]);
    assert_eq!(saw_bytes, vec![b"Hi".to_vec(), b"!".to_vec()]);
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
fn lazy_list_chunks_error_when_they_do_not_evaluate_to_lists() {
    let expr = builtin2_expr(
        Builtin::Append,
        TestExpr::Value(Value::binary_from_text("Hi")),
        builtin2_expr(Builtin::Add, TestExpr::Value(n(1)), TestExpr::Value(n(1))),
    );

    let value = eval_closed_expr(&expr).expect("append should preserve lazy chunk");
    let Value::List(list) = value else {
        panic!("append should produce a list");
    };

    let err = list_output_bytes(&test_context(), &list)
        .expect_err("bad lazy chunk should fail when observed");
    assert!(err.contains("lazy list chunk must evaluate to a list or binary value"));
}

#[test]
fn split_end_does_not_force_lazy_left_branch_when_suffix_is_in_right_branch() {
    let lazy_left = List::from_thunk(LazyValue::error("left branch was forced"));
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
    let force_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = force_count.clone();
    let counted = TestExpr::Value(Value::deferred("counted", move |_| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(n(2))
    }));
    let expr = TestExpr::Apply(
        Arc::new(function_expr(
            1,
            builtin2_expr(Builtin::Add, TestExpr::Local(0), TestExpr::Local(0)),
        )),
        Arc::new(counted),
    );

    let value = eval_closed_expr(&expr).expect("lambda body should evaluate");

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
    let argument = TestExpr::Value(Value::deferred("partial argument", move |_| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(n(40))
    }));
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
        apply_value(&test_context(), partial.clone(), n(2)).unwrap(),
        n(42)
    );
    assert_eq!(apply_value(&test_context(), partial, n(3)).unwrap(), n(43));
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
        Arc::new(TestExpr::Value(Value::deferred("list value", move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(42))
        }))),
    );
    let Value::List(list) = eval_closed_expr(&expression).unwrap() else {
        panic!("net-backed list literal should produce a list");
    };
    let Some((item, tail)) = list
        .try_pop_front(&mut |_| -> Result<_, EvalError> {
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
    let Value::Lazy(hole) = Value::deferred("list hole", |_| {
        Ok(Value::List(List::from_values(vec![n(42)])))
    }) else {
        unreachable!()
    };
    let list = List::from_thunk(hole);

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
    let effect = effect_value(closed_function_value(
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

    assert_eq!(err.to_string(), "application requires a function value");
}

#[test]
fn tagged_payload_ignores_only_semantically_undefined_extra_entries() {
    let payload = n(42);
    let lazy_empty = Value::Lazy(LazyValue::deferred("empty tag field", |_| {
        Ok(Value::Dict(Dict::new_sync()))
    }));
    let recursively_empty =
        Value::Dict(Dict::new_sync().insert(Key::atom_from_text("nested"), lazy_empty));
    let tagged = Dict::new_sync()
        .insert((*keys::TUPLE).clone(), payload.clone())
        .insert(Key::atom_from_text("ignored"), recursively_empty.clone());

    assert_eq!(
        tagged
            .tagged_payload(&test_context(), &keys::TUPLE)
            .unwrap(),
        Some(payload)
    );
    assert_eq!(
        tagged
            .insert(Key::atom_from_text("defined"), n(1))
            .tagged_payload(&test_context(), &keys::TUPLE)
            .unwrap(),
        None
    );
    assert_eq!(
        Dict::new_sync()
            .insert((*keys::TUPLE).clone(), recursively_empty)
            .tagged_payload(&test_context(), &keys::TUPLE)
            .unwrap(),
        None
    );
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
fn anno_builtin_preserves_lazy_targets_when_assertions_pass() {
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

    let Value::Lazy(thunk) = value else {
        panic!("anno should preserve lazy target evaluation");
    };
    let resolved = eval_value(&test_context(), &Value::Lazy(thunk))
        .expect("returned target should still evaluate");
    assert_eq!(resolved, n(42));
}

#[test]
fn anno_builtin_returns_stuck_errors_for_failed_assertions() {
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

    let value = eval_value(
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
    .expect("failed anno should still produce a stuck value");

    let Value::Lazy(thunk) = value else {
        panic!("failed anno should produce a stuck expression");
    };
    let err = eval_value(&test_context(), &Value::Lazy(thunk))
        .expect_err("failed anno should raise on demand");
    assert_eq!(
        err.to_string(),
        "cannot override `foo` because it is not defined"
    );
}

#[test]
fn list_annotations_rebalance_and_flatten_lists() {
    let deque = eval_closed_expr(&builtin2_expr(
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

    let binary = eval_closed_expr(&builtin2_expr(
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

    let array = eval_closed_expr(&builtin2_expr(
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
        list_to_value_items(&test_context(), &array).unwrap(),
        vec![n(72), n(105)]
    );
}

#[test]
fn list_annotations_return_stuck_errors_for_wrong_targets() {
    let value = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("binary"),
        ))),
        TestExpr::Value(Value::List(List::from_values(vec![n(300)]))),
    ))
    .expect("annotation should evaluate to a stuck expression");

    assert_eq!(
        eval_value(&test_context(), &value).unwrap_err().to_string(),
        "`binary` annotation cannot encode number `300` as a byte"
    );

    let value = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("deque"),
        ))),
        TestExpr::Value(n(1)),
    ))
    .expect("annotation should evaluate to a stuck expression");

    assert!(
        eval_value(&test_context(), &value)
            .unwrap_err()
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
fn reflection_gate_starts_once_and_leaves_its_target_unforced() {
    let context = test_context();
    let forced = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let forced_by_target = forced.clone();
    let target = Value::deferred("reflection target", move |_| {
        forced_by_target.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(n(42))
    });
    let gate = reflection_annotation(&context, n(0), target.clone());

    let first = eval_value(&context, &gate).expect_err("new reflection task should block");
    let wait = first
        .blocked_on()
        .expect("gate should report its task wait");
    let second = eval_value(&context, &gate).expect_err("queued reflection task should block");

    assert_eq!(second.blocked_on(), Some(wait.clone()));
    assert_eq!(context.reflection_task_count(), 1);
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 0);

    context.complete_wait(&wait.0);
    assert_eq!(eval_value(&context, &gate).unwrap(), target);
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn running_reflection_gate_rejects_a_foreign_session() {
    let owner = test_context();
    let observer = test_context();
    let gate = reflection_annotation(&owner, n(0), n(42));
    let blocked = eval_value(&owner, &gate).expect_err("new reflection task should block");

    assert_eq!(
        eval_value(&observer, &gate).unwrap_err().to_string(),
        "reflection annotation task belongs to another evaluation session"
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

    assert_eq!(
        eval_value(&context, &gate).unwrap_err().to_string(),
        "reflection task failed deliberately"
    );
    assert_eq!(
        eval_value(&context, &gate).unwrap_err().to_string(),
        "reflection task failed deliberately"
    );
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
    let runtime = applied.runtime().clone();

    let blocked = eval_value(&context, &Value::Net(applied))
        .expect_err("call should wait for its reflection gate");
    let wait = blocked
        .blocked_on()
        .expect("call should report a task wait");
    assert_eq!(runtime.with(|net| net.blocked_calls().count()), 1);

    context.complete_wait(&wait.0);
    let observer = test_context();
    assert_eq!(
        eval_value(&observer, &Value::Net(NetValue::new(runtime))).unwrap(),
        n(42)
    );
}

#[test]
fn builtins_are_curried_and_do_not_force_arguments_early() {
    let unforced = Value::deferred("unforced builtin argument", |_| {
        panic!("partial builtin application forced its first argument")
    });
    let partial = apply_values(
        &test_context(),
        Value::Builtin(Builtin::Append),
        vec![unforced],
    )
    .expect("partial builtin application should accept its first argument");

    match partial {
        Value::PartialBuiltin(call) => {
            assert_eq!(call.builtin, Builtin::Append);
            assert_eq!(call.arguments.len(), 1);
            assert!(matches!(&call.arguments[0], Value::Lazy(_)));
        }
        other => panic!("expected partial builtin, got {other:?}"),
    }
}
