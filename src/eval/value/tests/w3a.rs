use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

fn lazy_machine(context: &EvalContext, lazy: LazyValue) -> (LazyValue, LazyTaskMachine) {
    let root = lazy.root(context.values());
    let machine = LazyTaskMachine {
        context: context.clone(),
        lazy: root.clone(),
        work: LazyTaskWork::Produce,
    };
    (lazy, machine)
}

#[test]
fn lazy_source_result_is_installed_once_before_following_a_promise() {
    let context = EvalContext::isolated(crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    ));
    let promise = PromisedValue::new(context.values(), "source-result promise");
    let _promise_root = promise.root(context.values());
    let evaluations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&evaluations);
    let returned = promise.clone();
    let lazy = LazyValue::semantic_thunk(
        context.values(),
        "source result retained by WHNF",
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Promised(returned.clone()))
        },
    );
    let (lazy, mut machine) = lazy_machine(&context, lazy);
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);
    context
        .values()
        .collect_managed_for_test()
        .expect("the rooted source promise must survive collection before its first poll");

    for _ in 0..2 {
        let EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
            dependency: Some(WorkDependency::Promise(dependency)),
            ..
        }) = machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
        else {
            panic!("the retained source result must wait on its exact promise")
        };
        assert_eq!(dependency.id(), promise.id(context.values()));
        assert_eq!(evaluations.load(Ordering::SeqCst), 1);
    }

    crate::core::set_test_promise(context.values(), &promise, Value::Number(79.into()))
        .expect("the source-result promise should accept one assignment");
    assert!(matches!(
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)),
        EvaluationMachinePoll::Yielded
    ));
    let EvaluationMachinePoll::Complete(value) =
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
    else {
        panic!("the assigned promise must complete the owning lazy")
    };
    assert_eq!(value.clone_core_for_test(), Value::Number(79.into()));
    assert_eq!(evaluations.load(Ordering::SeqCst), 1);
    let cached = lazy
        .cached(context.values())
        .expect("the lazy task must publish one terminal cache");
    assert_eq!(
        cached
            .expect("the retained promise should succeed")
            .into_value(),
        Value::Number(79.into())
    );
    assert!(lazy.source_snapshot(context.values()).is_none());
}

#[test]
fn lazy_source_failure_is_cached_without_replaying_the_source() {
    let context = EvalContext::standalone();
    let evaluations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&evaluations);
    let lazy = LazyValue::semantic_thunk(
        context.values(),
        "source failure retained by lazy",
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Err(EvaluationHalt::new("source failed permanently"))
        },
    );
    let (_root, mut machine) = lazy_machine(&context, lazy);
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);

    let EvaluationMachinePoll::Failed(failure) =
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
    else {
        panic!("a permanent source failure must fail its lazy task")
    };
    assert!(failure.to_string().contains("source failed permanently"));
    assert_eq!(evaluations.load(Ordering::SeqCst), 1);

    let EvaluationMachinePoll::Failed(failure) =
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
    else {
        panic!("a repeated poll must observe the cached source failure")
    };
    assert!(failure.to_string().contains("source failed permanently"));
    assert_eq!(evaluations.load(Ordering::SeqCst), 1);
}
