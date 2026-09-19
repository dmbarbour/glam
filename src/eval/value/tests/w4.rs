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
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)),
        EvaluationMachinePoll::Yielded
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(machine.work, LazyTaskWork::HostCallInvoke));
    collect_between_handoffs(&context);

    assert!(matches!(
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)),
        EvaluationMachinePoll::Yielded
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(machine.work, LazyTaskWork::HostCallCheckpoint));
    context.values().with_runtime_value_access(|access| {
        let checkpoint = machine
            .lazy
            .access(&access)
            .expect("host fixture must share its value domain")
            .checkpoint_snapshot()
            .expect("the callback result must remain in its managed checkpoint");
        assert!(matches!(
            checkpoint.observe_host_call_in(&access),
            HostCallCheckpointObservation::After(Ok(_))
        ));
    });
    collect_between_handoffs(&context);

    assert!(matches!(
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)),
        EvaluationMachinePoll::Yielded
    ));
    collect_between_handoffs(&context);
    let EvaluationMachinePoll::Complete(value) =
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
    else {
        panic!("the rooted callback result must complete on a later poll")
    };
    assert_eq!(value.clone_core_for_test(), number(42));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let EvaluationMachinePoll::Complete(value) =
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
    else {
        panic!("a repeated poll must use the lazy cache")
    };
    assert_eq!(value.clone_core_for_test(), number(42));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn interrupted_host_call_is_never_replayed_after_route_loss() {
    let context = EvalContext::standalone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let lazy = LazyValue::host_call(context.values(), "W6G.1 interrupted host call", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        panic!("injected host callback unwind")
    });
    let retained = lazy.root(context.values());
    let mut machine = lazy_machine(&context, lazy);
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);

    assert!(matches!(
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)),
        EvaluationMachinePoll::Yielded
    ));
    assert!(matches!(machine.work, LazyTaskWork::HostCallInvoke));

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1));
    }));
    assert!(unwind.is_err(), "the injected callback must unwind");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(machine.work, LazyTaskWork::HostCallCheckpoint));

    drop(machine);
    collect_between_handoffs(&context);
    let retained = context
        .values()
        .with_runtime_value_access(|access| LazyValue::from_root(&retained, &access));
    let mut resumed = lazy_machine(&context, retained);
    let EvaluationMachinePoll::Failed(failure) =
        resumed.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
    else {
        panic!("a later route must reject the interrupted host call")
    };
    assert!(failure.to_string().contains("refusing to replay"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn completed_host_call_checkpoint_survives_route_loss_and_collection() {
    let context = EvalContext::standalone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let lazy = LazyValue::host_call(context.values(), "W6G.1 retained host outcome", {
        let values = context.values().clone();
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(crate::runtime::RuntimeValueRoot::new(&values, number(45)))
        }
    });
    let retained = lazy.root(context.values());
    let mut machine = lazy_machine(&context, lazy);
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);

    assert!(matches!(
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)),
        EvaluationMachinePoll::Yielded
    ));
    assert!(matches!(
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)),
        EvaluationMachinePoll::Yielded
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(machine);
    collect_between_handoffs(&context);

    let retained = context
        .values()
        .with_runtime_value_access(|access| LazyValue::from_root(&retained, &access));
    let mut resumed = lazy_machine(&context, retained);
    let value = loop {
        match resumed.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)) {
            EvaluationMachinePoll::Yielded => collect_between_handoffs(&context),
            EvaluationMachinePoll::Complete(value) => break value,
            EvaluationMachinePoll::Failed(failure) => panic!("{failure}"),
            _ => panic!("unexpected retained host-call poll"),
        }
    };
    assert_eq!(value.clone_core_for_test(), number(45));
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
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)),
        EvaluationMachinePoll::Yielded
    ));
    collect_between_handoffs(&context);
    assert!(matches!(
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)),
        EvaluationMachinePoll::Yielded
    ));
    collect_between_handoffs(&context);
    let EvaluationMachinePoll::Failed(failure) =
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
    else {
        panic!("the callback failure must become the lazy failure")
    };
    assert_eq!(failure.to_string(), "host callback failed");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let EvaluationMachinePoll::Failed(failure) =
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
    else {
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
    let result_root = context.values().with_runtime_value_access(|access| {
        let result = Value::Lazy(LazyValue::semantic_thunk_in(
            &access,
            "W4 lazy host result",
            move |_| {
                observed_forces.fetch_add(1, Ordering::SeqCst);
                Ok(number(44))
            },
        ));
        access.root_runtime_value(result)
    });
    let result = context
        .values()
        .with_runtime_value_access(|access| result_root.clone_core_with(&access));
    assert_eq!(eval_value(&context, &result).unwrap(), number(44));
    let observed_calls = Arc::clone(&calls);
    let lazy_root = context.values().with_runtime_value_access(|access| {
        let result = result_root.clone_core_with(&access);
        let lazy = LazyValue::external_host_call_in(
            &access,
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
        lazy.root_in(&access)
    });
    let lazy = context
        .values()
        .with_runtime_value_access(|access| LazyValue::from_root(&lazy_root, &access));
    let mut machine = lazy_machine(&context, lazy);
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);
    let value = loop {
        match machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)) {
            EvaluationMachinePoll::Yielded => collect_between_handoffs(&context),
            EvaluationMachinePoll::ScheduleSpark(_) => {
                panic!("the host-call fixture must not schedule a spark")
            }
            EvaluationMachinePoll::Complete(value) => break value,
            EvaluationMachinePoll::Blocked(_) => {
                panic!("the local semantic thunk must not block")
            }
            EvaluationMachinePoll::Failed(failure) => panic!("{failure}"),
            EvaluationMachinePoll::Exit(_) | EvaluationMachinePoll::Cancelled => {
                panic!("the local semantic thunk must not terminate its task")
            }
        }
    };
    assert_eq!(value.clone_core_for_test(), number(44));

    let EvaluationMachinePoll::Complete(value) =
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
    else {
        panic!("a repeated poll must use the lazy cache")
    };
    assert_eq!(value.clone_core_for_test(), number(44));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result_forces.load(Ordering::SeqCst), 1);
}

#[test]
fn reflection_source_hands_off_to_an_ordinary_promised_whnf_checkpoint() {
    let context = EvalContext::standalone();
    let value = Value::reflection_task_result(context.values(), number(0));
    let Value::Lazy(lazy) = value else {
        panic!("a reflection task result must be lazy")
    };
    let mut machine = lazy_machine(&context, lazy);
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);

    let first = machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1));
    assert!(matches!(machine.work, LazyTaskWork::WhnfCheckpoint));
    collect_between_handoffs(&context);

    let blocked = match first {
        EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
            dependency: Some(WorkDependency::Promise(blocked)),
            ..
        }) => blocked,
        EvaluationMachinePoll::Yielded => {
            let EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                dependency: Some(WorkDependency::Promise(blocked)),
                ..
            }) = machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
            else {
                panic!("the reserved reflection source must expose its stable wait")
            };
            blocked
        }
        EvaluationMachinePoll::Complete(_) => panic!("reflection handoff completed early"),
        EvaluationMachinePoll::Failed(failure) => panic!("reflection handoff failed: {failure}"),
        EvaluationMachinePoll::ScheduleSpark(_) => {
            panic!("reflection handoff unexpectedly scheduled a spark")
        }
        EvaluationMachinePoll::Exit(_) => panic!("reflection handoff requested exit"),
        EvaluationMachinePoll::Cancelled => panic!("reflection handoff was cancelled"),
        EvaluationMachinePoll::Blocked(_) => {
            panic!("reflection handoff blocked without its promise dependency")
        }
    };
    collect_between_handoffs(&context);

    let producer = blocked
        .producer()
        .expect("the reflection completion promise must retain its task producer");
    context.complete_wait_with_value(&producer.wait(), number(43));
    collect_between_handoffs(&context);
    assert!(matches!(
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)),
        EvaluationMachinePoll::Yielded
    ));
    collect_between_handoffs(&context);
    let EvaluationMachinePoll::Complete(value) =
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1))
    else {
        panic!("the completed reflection result must resume ordinary WHNF work")
    };
    assert_eq!(value.clone_core_for_test(), number(43));
}
