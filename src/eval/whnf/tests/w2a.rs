use crate::core::{CoreValueFactory, EvaluatedValue, LazyValue, Value};
use crate::evaluation::{EvalContext, EvaluationPollContext};
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::*;

fn isolated_values() -> CoreValueFactory {
    CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
}

fn lazy_computation(
    values: &CoreValueFactory,
    construct: impl for<'scope> FnOnce(&crate::core::RuntimeValueAccess<'scope>) -> LazyValue,
) -> (LazyValue, WhnfComputation) {
    let mut lazy = None;
    let root = values.construct_runtime_value_root(|access| {
        let value = construct(access);
        lazy = Some(value.clone());
        Value::Lazy(value)
    });
    (
        lazy.expect("the fixture must retain its lazy facade"),
        WhnfComputation::from_root(root),
    )
}

#[test]
fn cached_lazy_success_delegates_to_its_value_without_a_boundary() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let (lazy, mut computation) = lazy_computation(&values, |access| {
        LazyValue::semantic_thunk_in(access, "cached success", |_| {
            unreachable!("cached lazy source must not run")
        })
    });
    poll.with_value_access(&context, |access| {
        let root = lazy.root_in(access.values());
        let cached = root.cache(
            access.values(),
            Ok(EvaluatedValue::try_from(Value::Number(41.into())).expect("number is already WHNF")),
        );
        assert!(cached.is_ok());
    });

    let outcome = poll.with_value_access(&context, |access| {
        computation.poll_semantic_in(&access, &mut WhnfStepBudget::new(2))
    });
    let WhnfPoll::Ready(value) = outcome else {
        panic!("cached lazy success must complete without orchestration")
    };
    assert_eq!(value.clone_core_for_test(), Value::Number(41.into()));
    assert_eq!(context.deferred_task_count(), 0);
}

#[test]
fn cached_lazy_failure_is_published_without_reopening_its_source() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let failure = std::sync::Arc::new(crate::core::EvaluationFailure::message("cached failure"));
    let (_lazy, mut computation) = lazy_computation(&values, |access| {
        LazyValue::failure_in(access, "cached failure", failure.clone())
    });

    let outcome = poll.with_value_access(&context, |access| {
        computation.poll_semantic_in(&access, &mut WhnfStepBudget::new(1))
    });
    let WhnfPoll::Failed(actual) = outcome else {
        panic!("cached lazy failure must remain a permanent failure")
    };
    assert!(std::sync::Arc::ptr_eq(actual.as_failure(), &failure));
    assert_eq!(context.deferred_task_count(), 0);
}

#[test]
fn uncached_lazy_leaves_as_its_exact_root_and_resumes_from_that_identity() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let (lazy, mut computation) = lazy_computation(&values, |access| {
        LazyValue::semantic_thunk_in(access, "uncached", |_| {
            unreachable!("regional inspection must not run the producer")
        })
    });
    let expected_id = poll.with_value_access(&context, |access| access.lazy(&lazy).id());

    let request = poll.with_value_access(&context, |access| {
        computation.poll_semantic_in(&access, &mut WhnfStepBudget::new(1))
    });
    let WhnfPoll::Deferred(WhnfDeferredRequest::Lazy(root)) = request else {
        panic!("uncached lazy must request post-region producer coordination")
    };
    assert_eq!(root.id(), expected_id);
    assert_eq!(context.deferred_task_count(), 0);

    poll.with_value_access(&context, |access| {
        let cached = root.cache(
            access.values(),
            Ok(EvaluatedValue::try_from(Value::Number(43.into())).expect("number is already WHNF")),
        );
        assert!(cached.is_ok());
    });
    let resumed = poll.with_value_access(&context, |access| {
        computation.poll_semantic_in(&access, &mut WhnfStepBudget::new(2))
    });
    let WhnfPoll::Ready(value) = resumed else {
        panic!("the exact checkpointed lazy must observe its completed cache")
    };
    assert_eq!(value.clone_core_for_test(), Value::Number(43.into()));
}
