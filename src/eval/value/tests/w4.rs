use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

fn lazy_machine(context: &EvalContext, lazy: LazyValue) -> LazyTaskMachine {
    LazyTaskMachine {
        context: context.clone(),
        lazy: lazy.root(context.values()),
        work: LazyTaskWork::Produce,
    }
}

fn number(value: i64) -> Value {
    Value::Number(value.into())
}

fn collect_between_handoffs(context: &EvalContext) {
    context
        .values()
        .collect_managed_for_test()
        .expect("a returned W4 poll must leave no managed access active");
}

#[test]
fn host_call_yields_on_both_sides_and_consumes_its_result_once() {
    let context = EvalContext::standalone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let lazy = LazyValue::host_call(context.values(), "W4 host boundary", {
        let values = context.values().clone();
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(crate::runtime::RuntimeValueRoot::new(&values, number(42)))
        }
    });
    let mut machine = lazy_machine(&context, lazy);
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);

    assert!(matches!(
        machine.poll(&poll, 1),
        EvaluationMachinePoll::Yielded
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(machine.work, LazyTaskWork::HostCall(_)));
    collect_between_handoffs(&context);

    assert!(matches!(
        machine.poll(&poll, 1),
        EvaluationMachinePoll::Yielded
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let LazyTaskWork::HostCall(host) = &machine.work else {
        panic!("the callback result must remain in its typed owner")
    };
    assert!(matches!(host.state, HostCallSourceState::After(Ok(_))));
    collect_between_handoffs(&context);

    assert!(matches!(
        machine.poll(&poll, 1),
        EvaluationMachinePoll::Yielded
    ));
    collect_between_handoffs(&context);
    let EvaluationMachinePoll::Complete(value) = machine.poll(&poll, 1) else {
        panic!("the rooted callback result must complete on a later poll")
    };
    assert_eq!(value.clone_core_for_test(), number(42));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let EvaluationMachinePoll::Complete(value) = machine.poll(&poll, 1) else {
        panic!("a repeated poll must use the lazy cache")
    };
    assert_eq!(value.clone_core_for_test(), number(42));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_host_call_is_not_replayed_after_publication() {
    let context = EvalContext::standalone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let lazy = LazyValue::host_call(context.values(), "W4 failed host boundary", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        Err(Arc::new(EvaluationFailure::message("host callback failed")))
    });
    let mut machine = lazy_machine(&context, lazy);
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);

    assert!(matches!(
        machine.poll(&poll, 1),
        EvaluationMachinePoll::Yielded
    ));
    collect_between_handoffs(&context);
    assert!(matches!(
        machine.poll(&poll, 1),
        EvaluationMachinePoll::Yielded
    ));
    collect_between_handoffs(&context);
    let EvaluationMachinePoll::Failed(failure) = machine.poll(&poll, 1) else {
        panic!("the callback failure must become the lazy failure")
    };
    assert_eq!(failure.to_string(), "host callback failed");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let EvaluationMachinePoll::Failed(failure) = machine.poll(&poll, 1) else {
        panic!("a repeated poll must use the cached callback failure")
    };
    assert_eq!(failure.to_string(), "host callback failed");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn host_call_follows_a_lazy_result_without_reinvocation() {
    let context = EvalContext::standalone();
    let calls = Arc::new(AtomicUsize::new(0));
    let result_forces = Arc::new(AtomicUsize::new(0));
    let observed_forces = Arc::clone(&result_forces);
    let result = Value::semantic_thunk(context.values(), "W4 lazy host result", move |_| {
        observed_forces.fetch_add(1, Ordering::SeqCst);
        Ok(number(44))
    });
    let observed_calls = Arc::clone(&calls);
    let lazy = LazyValue::external_host_call(
        context.values(),
        "W4 lazy host callback",
        crate::core::HostCallRecord::external_with_semantic_values(
            "W4 host fixture",
            "eval/value/tests/w4.rs",
            "one explicit lazy result",
        ),
        [result],
        move |bundle| {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            let mut roots = bundle.into_roots().into_vec();
            Ok(roots
                .pop()
                .expect("the explicit capture bundle must contain the lazy result"))
        },
    );
    let value = Value::Lazy(lazy);

    assert_eq!(eval_value(&context, &value).unwrap(), number(44));
    assert_eq!(eval_value(&context, &value).unwrap(), number(44));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result_forces.load(Ordering::SeqCst), 1);
}

#[test]
fn reflection_source_reserves_then_waits_from_one_typed_owner() {
    let context = EvalContext::standalone();
    let value = Value::reflection_task_result(context.values(), number(0));
    let Value::Lazy(lazy) = value else {
        panic!("a reflection task result must be lazy")
    };
    let mut machine = lazy_machine(&context, lazy);
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);

    assert!(matches!(
        machine.poll(&poll, 1),
        EvaluationMachinePoll::Yielded
    ));
    assert!(matches!(machine.work, LazyTaskWork::Reflection(_)));
    collect_between_handoffs(&context);

    assert!(matches!(
        machine.poll(&poll, 1),
        EvaluationMachinePoll::Yielded
    ));
    let wait = {
        let LazyTaskWork::Reflection(reflection) = &machine.work else {
            panic!("the reflection source must retain its typed owner")
        };
        reflection
            .reservation
            .as_ref()
            .expect("the source owner must retain its reservation")
            .handle()
            .wait()
            .clone()
    };
    collect_between_handoffs(&context);

    let EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
        dependency: Some(WorkDependency::Wait(blocked)),
        ..
    }) = machine.poll(&poll, 1)
    else {
        panic!("the reserved reflection source must expose its stable wait")
    };
    assert_eq!(blocked, wait);
    collect_between_handoffs(&context);

    context.complete_wait_with_value(&wait, number(43));
    collect_between_handoffs(&context);
    assert!(matches!(
        machine.poll(&poll, 1),
        EvaluationMachinePoll::Yielded
    ));
    collect_between_handoffs(&context);
    let EvaluationMachinePoll::Complete(value) = machine.poll(&poll, 1) else {
        panic!("the completed reflection result must resume ordinary WHNF work")
    };
    assert_eq!(value.clone_core_for_test(), number(43));
}
