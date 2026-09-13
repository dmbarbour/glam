use std::sync::Arc;

use crate::core::{CoreValueFactory, EvaluationFailure, PromisedValue, Value};
use crate::evaluation::{EvalContext, EvaluationPollContext};
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::*;

fn isolated_values() -> CoreValueFactory {
    CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
}

fn promise_computation(
    values: &CoreValueFactory,
    label: &'static str,
) -> (PromisedValue, WhnfComputation) {
    let promise = PromisedValue::new(values, label);
    let root = values.construct_runtime_value_root(|access| {
        access.duplicate_value(&Value::Promised(promise.clone()))
    });
    (promise, WhnfComputation::from_root(root))
}

#[test]
fn assigned_promise_success_delegates_without_a_follower() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let (promise, mut computation) = promise_computation(&values, "assigned success");
    crate::core::set_test_promise(&values, &promise, Value::Number(47.into()))
        .expect("promise should accept one assignment");

    let outcome = poll.with_value_access(&context, |access| {
        computation.poll_semantic_in(&access, &mut WhnfStepBudget::new(2))
    });
    let WhnfPoll::Ready(value) = outcome else {
        panic!("assigned promise must delegate directly to its value")
    };
    assert_eq!(value.clone_core_for_test(), Value::Number(47.into()));
    assert_eq!(context.deferred_task_count(), 0);
}

#[test]
fn assigned_promise_failure_remains_the_original_structured_failure() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let (promise, mut computation) = promise_computation(&values, "assigned failure");
    let failure = Arc::new(EvaluationFailure::message("promise failed"));
    crate::core::fail_test_promise(&values, &promise, failure.clone())
        .expect("promise should accept one failure");

    let outcome = poll.with_value_access(&context, |access| {
        computation.poll_semantic_in(&access, &mut WhnfStepBudget::new(1))
    });
    let WhnfPoll::Failed(actual) = outcome else {
        panic!("assigned promise failure must remain terminal")
    };
    assert!(Arc::ptr_eq(actual.as_failure(), &failure));
    assert_eq!(context.deferred_task_count(), 0);
}

#[test]
fn unassigned_promise_leaves_as_its_exact_root_then_resumes_after_assignment() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let (promise, mut computation) = promise_computation(&values, "unassigned");
    let expected_id = poll.with_value_access(&context, |access| access.promise(&promise).id());

    let request = poll.with_value_access(&context, |access| {
        computation.poll_semantic_in(&access, &mut WhnfStepBudget::new(1))
    });
    let WhnfPoll::Deferred(WhnfDeferredRequest::Promise(root)) = request else {
        panic!("unassigned promise must request post-region completion policy")
    };
    assert_eq!(root.id(), expected_id);
    assert_eq!(context.deferred_task_count(), 0);

    crate::core::set_test_promise(&values, &promise, Value::Number(53.into()))
        .expect("promise should remain assignable after inspection");
    let resumed = poll.with_value_access(&context, |access| {
        computation.poll_semantic_in(&access, &mut WhnfStepBudget::new(2))
    });
    let WhnfPoll::Ready(value) = resumed else {
        panic!("the same promise checkpoint must observe its assignment")
    };
    assert_eq!(value.clone_core_for_test(), Value::Number(53.into()));
}
