//! Deterministic small-stack closure fixtures for Phase W7B.
//!
//! These tests force semantic depth and suspension order. Repeating a test
//! under an uncontrolled scheduler is deliberately not used as evidence.

use std::hint::black_box;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;

use crate::core::{
    CoreValueFactory, Dict, EvaluationFailure, EvaluationHalt, FixpointComputation, FunctionValue,
    Key, LazyValue, List, PromisedValue, Value,
};
use crate::core_net::CoreDataKey;
use crate::eval::access_machine::{ConversionPoll, KeyConversionMachine, KeyListMachine};
use crate::eval::test_support::{TestExpr, closed_function_value_in};
use crate::eval::whnf::{
    RegionalWhnfDrive, RegionalWhnfStep, RegionalWhnfWork, WhnfComputation, WhnfStepBudget,
    drive_regional,
};
use crate::evaluation::{
    EvalContext, EvaluationMachinePoll, EvaluationPollContext, EvaluationPumpOutcome,
    EvaluationStepBudget, EvaluationTaskBlock, EvaluationTaskMachine, EvaluationWaitPoll,
    EvaluatorStepContext, OwnedEvalContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::number::Number;
use crate::runtime::{RuntimeIds, RuntimeValueRoot, allocate_evaluation_runtime_id};

const SMALL_STACK_BYTES: usize = 512 * 1024;
const SEMANTIC_DEPTH: usize = 4_096;
const PRODUCER_DEPTH: usize = 512;
const OWNER_DEPTH: usize = 128;
const RECURSIVE_CONTROL_ENV: &str = "GLAM_W7B_RECURSIVE_CONTROL";

fn on_small_stack<T: Send + 'static>(
    name: &'static str,
    operation: impl FnOnce() -> T + Send + 'static,
) -> T {
    thread::Builder::new()
        .name(name.into())
        .stack_size(SMALL_STACK_BYTES)
        .spawn(operation)
        .expect("the W7B small-stack witness thread should spawn")
        .join()
        .expect("the W7B small-stack witness must not panic")
}

fn context() -> OwnedEvalContext {
    EvalContext::isolated(CoreValueFactory::new(
        allocate_evaluation_runtime_id(),
        RuntimeIds::new(),
    ))
}

fn return_first_capture(
    _context: &EvaluatorStepContext<'_>,
    captures: &[Value],
) -> Result<Value, EvaluationHalt> {
    Ok(captures[0].clone())
}

fn lazy_alias_root(context: &EvalContext, leaf: Value) -> RuntimeValueRoot {
    context.values().construct_runtime_value_root(|access| {
        let mut current = leaf;
        for _ in 0..PRODUCER_DEPTH {
            let lazy = LazyValue::semantic_computation_in(
                access,
                "W7B lazy alias",
                Arc::from([access.duplicate_value(&current)]),
                return_first_capture,
            );
            current = Value::Lazy(lazy);
        }
        current
    })
}

fn promised_alias_root(context: &EvalContext, leaf: Value, depth: usize) -> RuntimeValueRoot {
    let mut current = leaf;
    for _ in 0..depth {
        let promise = PromisedValue::new(context.values(), "W7B promised alias");
        crate::core::set_test_promise(context.values(), &promise, current)
            .expect("a fresh promised alias should accept its assignment");
        current = Value::Promised(promise);
    }
    RuntimeValueRoot::new(context.values(), current)
}

fn rooted_closed_function(context: &EvalContext, arity: usize, body: TestExpr) -> RuntimeValueRoot {
    RuntimeValueRoot::new(
        context.values(),
        closed_function_value_in(context.values(), arity, body),
    )
}

fn application_chain_root(context: &EvalContext, leaf: Value) -> RuntimeValueRoot {
    application_chain_root_with_depth(context, leaf, PRODUCER_DEPTH)
}

fn application_chain_root_with_depth(
    context: &EvalContext,
    leaf: Value,
    depth: usize,
) -> RuntimeValueRoot {
    let identity = rooted_closed_function(context, 1, TestExpr::Local(0));
    context.values().construct_runtime_value_root(|access| {
        let identity = identity.clone_core_with(access);
        let mut current = leaf;
        for _ in 0..depth {
            current = Value::Lazy(LazyValue::from_application_in(
                access,
                access.duplicate_value(&identity),
                Arc::from([access.duplicate_value(&current)]),
            ));
        }
        current
    })
}

struct WhnfReflectionTask {
    context: EvalContext,
    computation: WhnfComputation,
}

impl EvaluationTaskMachine for WhnfReflectionTask {
    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        step_budget: &mut EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        match poll_whnf_computation(
            &mut self.computation,
            poll_context,
            &self.context,
            step_budget,
        ) {
            WhnfOwnerPoll::Ready(value) => EvaluationMachinePoll::Complete(value),
            WhnfOwnerPoll::Pending(dependency) => {
                EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    dependency: Some(dependency),
                    observed_epoch: None,
                    error: None,
                })
            }
            WhnfOwnerPoll::Yielded => EvaluationMachinePoll::Yielded,
            WhnfOwnerPoll::Failed(failure) => EvaluationMachinePoll::Failed(failure),
            WhnfOwnerPoll::External(boundary) => {
                unreachable!("the W7B reflection fixture reached external {boundary:?}")
            }
        }
    }
}

fn fixpoint_chain_root(context: &EvalContext, leaf: Value) -> RuntimeValueRoot {
    // In the test fixture's de Bruijn convention Local(1) is the first of two
    // arguments. Partially applying it therefore creates a constant function
    // which ignores the fixpoint marker.
    let constant_template = rooted_closed_function(context, 2, TestExpr::Local(1));
    context.values().construct_runtime_value_root(|access| {
        let Value::Function(template) = constant_template.clone_core_with(access) else {
            unreachable!("the constant template is a function")
        };
        let mut current = leaf;
        for _ in 0..PRODUCER_DEPTH {
            let stage = super::net::attach_net_many_in(
                access,
                template.duplicate_stage_in(access),
                vec![access.duplicate_value(&current)],
            );
            let function = Value::Function(FunctionValue::new(stage, 1));
            current = Value::Lazy(LazyValue::computed_fixpoint_in(
                access,
                "W7B fixpoint chain",
                FixpointComputation::Function(function),
            ));
        }
        current
    })
}

fn assert_ready_number(result: RuntimeValueRoot, expected: usize) {
    assert_eq!(
        result.clone_core_for_test(),
        Value::Number(Number::from_usize(expected))
    );
}

fn drive_until_stably_blocked(context: &EvalContext, work_bound: usize) {
    for _ in 0..work_bound {
        if !context.poll_one_runtime_work_for_test() {
            return;
        }
    }
    panic!("the forced W7B suspension did not become stably blocked");
}

fn drive_executor_until_stably_blocked(context: &EvalContext, work_bound: usize) {
    for _ in 0..work_bound {
        if !context.poll_one_executor_work_for_test() {
            return;
        }
    }
    panic!("the forced W7B executor suspension did not become stably blocked");
}

fn assert_producer_chain(
    build: fn(&EvalContext, Value) -> RuntimeValueRoot,
    uninterrupted_thread: &'static str,
    suspension_thread: &'static str,
    resumption_thread: &'static str,
) {
    on_small_stack(uninterrupted_thread, move || {
        let context = context();
        let root = build(&context, Value::Number(Number::from_usize(PRODUCER_DEPTH)));
        let result = context
            .evaluate_root_whnf(root)
            .expect("the uninterrupted producer chain should reach WHNF");
        assert_ready_number(result, PRODUCER_DEPTH);
    });

    let (context, promise, handle, first_owner) = on_small_stack(suspension_thread, move || {
        let context = context();
        let promise = PromisedValue::new(context.values(), "W7B producer-chain suspension");
        let root = build(&context, Value::Promised(promise.clone()));
        let handle = context
            .demand_whnf(root)
            .expect("the forced producer-chain demand should be admitted");
        drive_until_stably_blocked(&context, PRODUCER_DEPTH * 64 + 128);
        assert!(handle.poll().is_none());
        assert_eq!(promise.exact_subscription_count(context.values()), 1);
        (context, promise, handle, thread::current().id())
    });
    crate::core::set_test_promise(
        context.values(),
        &promise,
        Value::Number(Number::from_usize(PRODUCER_DEPTH)),
    )
    .expect("the forced producer-chain suspension should resolve once");
    let (second_owner, result, _context) = on_small_stack(resumption_thread, move || {
        let owner = thread::current().id();
        let result = context
            .drive_client_demand_value_for_test(handle)
            .expect("the assigned producer chain should resume");
        (owner, result, context)
    });
    assert_ne!(first_owner, second_owner);
    assert_ready_number(result, PRODUCER_DEPTH);
}

fn promised_dictionary_chain(context: &EvalContext, depth: usize, key: &Key, leaf: Value) -> Value {
    (0..depth).fold(leaf, |value, _| {
        let promise = PromisedValue::new(context.values(), "W7B dictionary link");
        crate::core::set_test_promise(context.values(), &promise, value)
            .expect("a fresh dictionary link should accept its assignment");
        Value::Dict(Dict::new_sync().insert(key.clone(), Value::Promised(promise)))
    })
}

fn poll_key_conversion(context: &EvalContext, machine: &mut KeyConversionMachine) -> Key {
    let poll = EvaluationPollContext::for_context(context);
    loop {
        let result = poll.evaluate(context, |evaluator| {
            machine.poll(&poll, evaluator, context, &mut EvaluationStepBudget::new(1))
        });
        match result {
            ConversionPoll::Ready(key) => return key,
            ConversionPoll::Yielded => {}
            ConversionPoll::Pending(_) => panic!("the strict key fixture must not suspend"),
            ConversionPoll::Failed(failure) => {
                panic!("the strict key fixture failed: {failure:?}")
            }
        }
    }
}

fn poll_key_list(context: &EvalContext, machine: &mut KeyListMachine) -> Vec<Key> {
    let poll = EvaluationPollContext::for_context(context);
    loop {
        let result = poll.evaluate(context, |evaluator| {
            machine.poll(&poll, evaluator, context, &mut EvaluationStepBudget::new(1))
        });
        match result {
            ConversionPoll::Ready(keys) => return keys,
            ConversionPoll::Yielded => {}
            ConversionPoll::Pending(_) => panic!("the strict list fixture must not suspend"),
            ConversionPoll::Failed(failure) => {
                panic!("the strict list fixture failed: {failure:?}")
            }
        }
    }
}

#[inline(never)]
fn recursive_control(depth: usize) -> usize {
    // Keep the frame observably source-shaped and large enough that the
    // selected depth cannot accidentally fit the explicit W7B stack.
    let frame = [depth as u8; 1_024];
    black_box(&frame);
    if depth == 0 {
        black_box(frame[0] as usize)
    } else {
        let child = recursive_control(depth - 1);
        black_box(&frame);
        child.wrapping_add(black_box(frame[0] as usize))
    }
}

#[test]
fn recursive_depth_control_child() {
    if std::env::var_os(RECURSIVE_CONTROL_ENV).is_none() {
        return;
    }
    on_small_stack("w7b-recursive-control", || {
        black_box(recursive_control(SEMANTIC_DEPTH));
    });
}

#[test]
fn selected_depth_overflows_an_equivalent_recursive_fixture() {
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let test_name = format!("{module}::recursive_depth_control_child");
    let status = Command::new(std::env::current_exe().expect("the test executable should exist"))
        .args(["--exact", test_name.as_str(), "--nocapture"])
        .env(RECURSIVE_CONTROL_ENV, "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the isolated recursive control should launch");
    assert!(
        !status.success(),
        "the W7B semantic depth must exceed the equivalent recursive small-stack fixture"
    );
}

#[test]
fn explicit_whnf_worklist_completes_at_the_recursive_control_depth() {
    on_small_stack("w7b-explicit-whnf-worklist", || {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let context = EvalContext::isolated(values);
        let poll = EvaluationPollContext::for_context(&context);
        poll.with_value_access(&context, |access| {
            let expected = Value::Number(Number::from_usize(SEMANTIC_DEPTH));
            let work =
                RegionalWhnfWork::from_focus(&access, access.values().duplicate_value(&expected));
            let mut transitions = 0;
            let mut budget = WhnfStepBudget::new(SEMANTIC_DEPTH + 1);
            let outcome = drive_regional(&access, work, &mut budget, |_access, _work| {
                if transitions == SEMANTIC_DEPTH {
                    RegionalWhnfStep::Ready(Value::Number(Number::from_usize(SEMANTIC_DEPTH)))
                } else {
                    transitions += 1;
                    RegionalWhnfStep::Delegate(Value::Number(Number::from_usize(SEMANTIC_DEPTH)))
                }
            });
            let RegionalWhnfDrive::Ready(actual) = outcome else {
                panic!("the bounded explicit worklist should complete")
            };
            assert_eq!(actual, expected);
            assert_eq!(transitions, SEMANTIC_DEPTH);
            assert_eq!(budget.remaining(), 0);
        });
    });
}

#[test]
fn deep_lazy_aliases_complete_and_resume_on_a_forced_owner() {
    on_small_stack("w7b-lazy-alias-uninterrupted", || {
        let context = context();
        let root = lazy_alias_root(&context, Value::Number(Number::from_usize(SEMANTIC_DEPTH)));
        let result = context
            .evaluate_root_whnf(root)
            .expect("the uninterrupted lazy aliases should reach WHNF");
        assert_ready_number(result, SEMANTIC_DEPTH);
    });

    let (context, promise, handle, first_owner) =
        on_small_stack("w7b-lazy-alias-suspend-owner", || {
            let context = context();
            let promise = PromisedValue::new(context.values(), "W7B lazy alias suspension");
            let root = lazy_alias_root(&context, Value::Promised(promise.clone()));
            let handle = context
                .demand_whnf(root)
                .expect("the forced lazy-alias demand should be admitted");
            drive_until_stably_blocked(&context, SEMANTIC_DEPTH * 2 + 32);
            assert!(handle.poll().is_none());
            assert_eq!(promise.exact_subscription_count(context.values()), 1);
            (context, promise, handle, thread::current().id())
        });
    crate::core::set_test_promise(
        context.values(),
        &promise,
        Value::Number(Number::from_usize(SEMANTIC_DEPTH)),
    )
    .expect("the forced lazy-alias suspension should resolve once");
    let (second_owner, result, _context) =
        on_small_stack("w7b-lazy-alias-resume-owner", move || {
            let owner = thread::current().id();
            let result = context
                .drive_client_demand_value_for_test(handle)
                .expect("the assigned lazy aliases should resume");
            (owner, result, context)
        });
    assert_ne!(first_owner, second_owner);
    assert_ready_number(result, SEMANTIC_DEPTH);
}

#[test]
fn deep_promise_aliases_complete_and_resume_on_a_forced_owner() {
    on_small_stack("w7b-promise-alias-uninterrupted", || {
        let context = context();
        let root = promised_alias_root(
            &context,
            Value::Number(Number::from_usize(SEMANTIC_DEPTH)),
            SEMANTIC_DEPTH,
        );
        let result = context
            .evaluate_root_whnf(root)
            .expect("the uninterrupted promise aliases should reach WHNF");
        assert_ready_number(result, SEMANTIC_DEPTH);
    });

    let (context, promise, handle, first_owner) =
        on_small_stack("w7b-promise-alias-suspend-owner", || {
            let context = context();
            let promise = PromisedValue::new(context.values(), "W7B promise alias suspension");
            let root =
                promised_alias_root(&context, Value::Promised(promise.clone()), SEMANTIC_DEPTH);
            let handle = context
                .demand_whnf(root)
                .expect("the forced promise-alias demand should be admitted");
            drive_until_stably_blocked(&context, SEMANTIC_DEPTH + 32);
            assert!(handle.poll().is_none());
            assert_eq!(promise.exact_subscription_count(context.values()), 1);
            (context, promise, handle, thread::current().id())
        });
    crate::core::set_test_promise(
        context.values(),
        &promise,
        Value::Number(Number::from_usize(SEMANTIC_DEPTH)),
    )
    .expect("the forced promise-alias suspension should resolve once");
    let (second_owner, result, _context) =
        on_small_stack("w7b-promise-alias-resume-owner", move || {
            let owner = thread::current().id();
            let result = context
                .drive_client_demand_value_for_test(handle)
                .expect("the assigned promise aliases should resume");
            (owner, result, context)
        });
    assert_ne!(first_owner, second_owner);
    assert_ready_number(result, SEMANTIC_DEPTH);
}

#[test]
fn deep_application_chain_completes_and_resumes_on_a_forced_owner() {
    assert_producer_chain(
        application_chain_root,
        "w7b-application-uninterrupted",
        "w7b-application-suspend-owner",
        "w7b-application-resume-owner",
    );
}

#[test]
fn deep_fixpoint_chain_completes_and_resumes_on_a_forced_owner() {
    assert_producer_chain(
        fixpoint_chain_root,
        "w7b-fixpoint-uninterrupted",
        "w7b-fixpoint-suspend-owner",
        "w7b-fixpoint-resume-owner",
    );
}

#[test]
fn deep_static_access_path_completes_on_the_small_stack() {
    let (_context, result) = on_small_stack("w7b-static-access", || {
        let context = context();
        let key = Key::atom_from_text("next");
        let path: Arc<[CoreDataKey]> = (0..SEMANTIC_DEPTH)
            .map(|_| CoreDataKey::Key(key.clone()))
            .collect::<Vec<_>>()
            .into();
        let base = promised_dictionary_chain(
            &context,
            SEMANTIC_DEPTH,
            &key,
            Value::Number(Number::from_usize(SEMANTIC_DEPTH)),
        );
        let root = context.values().construct_runtime_value_root(|access| {
            Value::Lazy(LazyValue::from_access_in(access, path, Arc::from([base])))
        });
        let result = context
            .evaluate_root_whnf(root)
            .expect("the deep static path should reach its leaf");
        assert_ready_number(result.clone(), SEMANTIC_DEPTH);
        (context, result)
    });
    assert_ready_number(result, SEMANTIC_DEPTH);
}

#[test]
fn deeply_nested_dictionary_key_conversion_completes_on_the_small_stack() {
    let (_context, key) = on_small_stack("w7b-key-conversion", || {
        let context = context();
        let member = Key::atom_from_text("member");
        let input = RuntimeValueRoot::new(
            context.values(),
            promised_dictionary_chain(
                &context,
                SEMANTIC_DEPTH,
                &member,
                Value::Number(Number::from_usize(SEMANTIC_DEPTH)),
            ),
        );
        let mut machine = KeyConversionMachine::new(input, None);
        let key = poll_key_conversion(&context, &mut machine);
        (context, key)
    });

    let mut current = &key;
    for _ in 0..SEMANTIC_DEPTH {
        let Key::Dict(entries) = current else {
            panic!("every nested key level must remain a dictionary")
        };
        assert_eq!(entries.len(), 1);
        current = &entries[0].1;
    }
    assert_eq!(current, &Key::Number(Number::from_usize(SEMANTIC_DEPTH)));
}

#[test]
fn large_strict_collection_conversion_completes_on_the_small_stack() {
    let (_context, keys) = on_small_stack("w7b-collection-conversion", || {
        let context = context();
        let input = context.values().construct_runtime_value_root(|_| {
            Value::List(List::from_values(
                (0..SEMANTIC_DEPTH)
                    .map(|index| Value::Number(Number::from_usize(index)))
                    .collect(),
            ))
        });
        let mut machine = KeyListMachine::unowned(input);
        let keys = poll_key_list(&context, &mut machine);
        (context, keys)
    });
    assert_eq!(keys.len(), SEMANTIC_DEPTH);
    assert_eq!(keys.first(), Some(&Key::Number(Number::from_usize(0))));
    assert_eq!(
        keys.last(),
        Some(&Key::Number(Number::from_usize(SEMANTIC_DEPTH - 1)))
    );
}

#[test]
fn deep_aliases_preserve_one_structured_failure_on_the_small_stack() {
    let _context = on_small_stack("w7b-structured-failure", || {
        let context = context();
        let detail = Key::atom_from_text("detail");
        let emission = Value::Dict(
            Dict::new_sync()
                .insert(
                    Key::atom_from_text("msg"),
                    Value::binary_from_text("W7B structured failure"),
                )
                .insert(detail, Value::Number(7.into())),
        );
        let frame = Value::Dict(Dict::new_sync().insert(
            Key::atom_from_text("eval"),
            Value::binary_from_text("small_stack"),
        ));
        let failure =
            Arc::new(EvaluationFailure::emission(emission.clone()).with_context(frame.clone()));
        let promise = PromisedValue::new(context.values(), "W7B structured failure");
        crate::core::fail_test_promise(context.values(), &promise, failure.clone())
            .expect("the fresh terminal promise should accept one failure");
        let root = promised_alias_root(&context, Value::Promised(promise), SEMANTIC_DEPTH);
        let observed = context
            .evaluate_root_whnf(root)
            .expect_err("the deep aliases must preserve their terminal failure")
            .into_permanent_failure();
        assert!(Arc::ptr_eq(&failure, &observed));
        assert_eq!(observed.emission_value(), Some(&emission));
        assert_eq!(observed.contexts(), [frame]);
        context
    });
}

#[test]
fn lazy_route_checkpoint_resumes_on_another_small_stack_poller() {
    let (context, promise, wait, _lazy_root, first_owner) =
        on_small_stack("w7b-lazy-route-first-owner", || {
            let context = context();
            let promise = PromisedValue::new(context.values(), "W7B lazy-route suspension");
            let root = application_chain_root_with_depth(
                &context,
                Value::Promised(promise.clone()),
                OWNER_DEPTH,
            );
            let Value::Lazy(lazy) = root.clone_core_for_test() else {
                unreachable!("the application chain must publish one outer lazy")
            };
            let lazy_root = lazy.root(context.values());
            let wait = super::lazy_root_wait(&context, &lazy_root)
                .expect("the exact lazy route should be admitted");
            loop {
                match context.pump_wait(&wait, 4_096) {
                    EvaluationPumpOutcome::BudgetExhausted => {}
                    EvaluationPumpOutcome::NoProgress => break,
                    EvaluationPumpOutcome::Busy => {
                        panic!("the single-poller fixture cannot have a busy owner")
                    }
                    EvaluationPumpOutcome::TargetReady => {
                        panic!("the unassigned terminal promise cannot be ready")
                    }
                }
            }
            assert_eq!(promise.exact_subscription_count(context.values()), 1);
            (context, promise, wait, lazy_root, thread::current().id())
        });
    crate::core::set_test_promise(
        context.values(),
        &promise,
        Value::Number(Number::from_usize(OWNER_DEPTH)),
    )
    .expect("the lazy-route suspension should resolve once");
    let (second_owner, result, _context) =
        on_small_stack("w7b-lazy-route-second-owner", move || {
            let owner = thread::current().id();
            loop {
                match context.pump_wait(&wait, 4_096) {
                    EvaluationPumpOutcome::TargetReady => break,
                    EvaluationPumpOutcome::BudgetExhausted => {}
                    EvaluationPumpOutcome::Busy | EvaluationPumpOutcome::NoProgress => {
                        panic!("the assigned exact lazy route must make progress")
                    }
                }
            }
            let EvaluationWaitPoll::Complete(value) = context.poll_wait(&wait) else {
                panic!("the exact lazy route must publish its WHNF result")
            };
            (owner, *value, context)
        });
    assert_ne!(first_owner, second_owner);
    assert_ready_number(result, OWNER_DEPTH);
}

#[test]
fn reflection_hosted_checkpoint_resumes_on_another_small_stack_poller() {
    let (context, promise, task, first_owner) =
        on_small_stack("w7b-reflection-first-owner", || {
            let context = context();
            let promise = PromisedValue::new(context.values(), "W7B reflection suspension");
            let root =
                promised_alias_root(&context, Value::Promised(promise.clone()), SEMANTIC_DEPTH);
            let computation = WhnfComputation::from_root(root);
            let task = context
                .schedule_task(move |task_context| {
                    Ok(Box::new(WhnfReflectionTask {
                        context: task_context,
                        computation,
                    }))
                })
                .expect("the WHNF-hosting reflection fixture should schedule");
            drive_until_stably_blocked(&context, SEMANTIC_DEPTH + 32);
            assert!(matches!(
                context.poll_reflection_task(&task),
                EvaluationWaitPoll::Pending(_)
            ));
            assert_eq!(promise.exact_subscription_count(context.values()), 1);
            (context, promise, task, thread::current().id())
        });
    crate::core::set_test_promise(
        context.values(),
        &promise,
        Value::Number(Number::from_usize(SEMANTIC_DEPTH)),
    )
    .expect("the reflection-hosted suspension should resolve once");
    let (second_owner, result, _context) =
        on_small_stack("w7b-reflection-second-owner", move || {
            let owner = thread::current().id();
            for _ in 0..SEMANTIC_DEPTH + 32 {
                match context.poll_reflection_task(&task) {
                    EvaluationWaitPoll::Complete(value) => {
                        return (owner, *value, context);
                    }
                    EvaluationWaitPoll::Pending(_) => {
                        assert!(context.poll_one_runtime_work_for_test());
                    }
                    other => panic!("the reflection fixture terminated unexpectedly: {other:?}"),
                }
            }
            panic!("the reflection-hosted checkpoint exhausted its work bound")
        });
    assert_ne!(first_owner, second_owner);
    assert_ready_number(result, SEMANTIC_DEPTH);
}

#[test]
fn spark_checkpoint_resumes_on_another_small_stack_poller() {
    let (context, promise, lazy, root, first_owner) =
        on_small_stack("w7b-spark-first-owner", || {
            let context = context();
            context.start_manual_spark_worker_for_test();
            let promise = PromisedValue::new(context.values(), "W7B spark suspension");
            let root = application_chain_root_with_depth(
                &context,
                Value::Promised(promise.clone()),
                OWNER_DEPTH,
            );
            let Value::Lazy(lazy) = root.clone_core_for_test() else {
                unreachable!("the spark fixture must publish one outer lazy")
            };
            context.spark_root(root.clone());
            drive_executor_until_stably_blocked(&context, OWNER_DEPTH * 64 + 128);
            assert_eq!(promise.exact_subscription_count(context.values()), 1);
            (context, promise, lazy, root, thread::current().id())
        });
    crate::core::set_test_promise(
        context.values(),
        &promise,
        Value::Number(Number::from_usize(OWNER_DEPTH)),
    )
    .expect("the spark suspension should resolve once");
    let (second_owner, result, _context, _root) =
        on_small_stack("w7b-spark-second-owner", move || {
            let owner = thread::current().id();
            let result = loop {
                if let Some(result) = lazy.cached(context.values()) {
                    break result
                        .expect("the spark application chain should not fail")
                        .into_value();
                }
                assert!(
                    context.poll_one_executor_work_for_test(),
                    "the assigned spark checkpoint must remain runnable"
                );
            };
            context.stop_manual_spark_worker_for_test();
            (owner, result, context, root)
        });
    assert_ne!(first_owner, second_owner);
    assert_eq!(result, Value::Number(Number::from_usize(OWNER_DEPTH)));
}
