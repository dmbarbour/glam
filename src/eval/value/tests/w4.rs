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

#[test]
fn stale_route_cannot_replace_a_newer_lazy_checkpoint() {
    let context = EvalContext::standalone();
    let lazy = LazyValue::semantic_thunk(
        context.values(),
        "W6G.1 exact checkpoint replacement",
        |_| unreachable!("the replacement fixture does not evaluate its source"),
    );
    let rooted = lazy.root(context.values());

    crate::eval::with_direct_evaluator(&context, |evaluator| {
        evaluator.with_value_access(|access| {
            let lazy = access.lazy_root(&rooted);
            let initial = ManagedLazyCheckpointEdge::allocate_regional_in(
                &access,
                super::super::whnf::RegionalWhnfWork::from_focus(&access, context.values().unit()),
            )
            .expect("the initial fixture checkpoint must fit its managed slot");
            assert!(lazy.install_checkpoint(initial).is_ok());

            let first_route = lazy
                .checkpoint_snapshot()
                .expect("the first route must observe the installed checkpoint");
            let stale_route = lazy
                .checkpoint_snapshot()
                .expect("the stale route must observe the same checkpoint");
            let replacement = ManagedLazyCheckpointEdge::allocate_regional_in(
                &access,
                super::super::whnf::RegionalWhnfWork::from_focus(&access, number(1)),
            )
            .expect("the replacement fixture checkpoint must fit its managed slot");
            assert!(lazy.replace_checkpoint(&first_route, replacement).is_ok());
            let installed = lazy
                .checkpoint_snapshot()
                .expect("the winning replacement must remain installed");

            let stale_replacement = ManagedLazyCheckpointEdge::allocate_regional_in(
                &access,
                super::super::whnf::RegionalWhnfWork::from_focus(&access, number(2)),
            )
            .expect("the stale fixture checkpoint must fit its managed slot");
            assert!(
                lazy.replace_checkpoint(&stale_route, stale_replacement)
                    .is_err(),
                "a route may replace only the exact checkpoint it drove"
            );
            let current = lazy
                .checkpoint_snapshot()
                .expect("a rejected stale replacement must preserve the winner");
            assert!(current.same_checkpoint_in(&installed, access.values()));
        });
    });
}

fn collect_between_handoffs(context: &EvalContext) {
    context
        .values()
        .collect_managed_for_test()
        .expect("a returned W4 poll must leave no managed access active");
}

fn pump_to_ready(context: &EvalContext, wait: &crate::evaluation::EvaluationWaitToken) {
    for attempt in 0..64 {
        match context.pump_wait(wait, 256) {
            crate::evaluation::EvaluationPumpOutcome::TargetReady => return,
            crate::evaluation::EvaluationPumpOutcome::BudgetExhausted => {}
            crate::evaluation::EvaluationPumpOutcome::Busy => {
                panic!("deterministic W6G.1 fixture unexpectedly found a claimed producer")
            }
            crate::evaluation::EvaluationPumpOutcome::NoProgress => {
                panic!("deterministic W6G.1 fixture lost its producer")
            }
        }
        assert_ne!(attempt, 63, "W6G.1 fixture exhausted its pump bound");
    }
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
fn net_whnf_checkpoint_survives_route_loss_and_collection() {
    let context = EvalContext::standalone();
    let promise = PromisedValue::new(context.values(), "W6G.1 retained net callable");
    let mut builder =
        crate::interaction_net::NetBuilder::<crate::core_net::CoreSpecialization>::new();
    let [application, argument, result] = builder.bind();
    let function = builder.data(Value::Promised(promise.clone()));
    let value = builder.data(context.values().unit());
    builder.wire(application, function);
    builder.wire(argument, value);
    let runtime = context
        .values()
        .instantiate_core_net(&builder.finish(result));
    let lazy =
        LazyValue::from_net_computation(context.values(), crate::core::NetValue::new(runtime));
    let retained = lazy.root(context.values());
    let mut machine = lazy_machine(&context, lazy);
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);

    let wait = 'blocked: {
        for attempt in 0..64 {
            match machine.poll(
                &poll,
                &mut crate::evaluation::EvaluationStepBudget::new(256),
            ) {
                EvaluationMachinePoll::Yielded => {}
                EvaluationMachinePoll::Blocked(block) => {
                    let Some(WorkDependency::Wait(wait)) = block.dependency else {
                        panic!("the blocked callable must publish its subscribed wait")
                    };
                    break 'blocked wait;
                }
                _ => panic!("unexpected pre-assignment net-WHNF poll"),
            }
            assert_ne!(
                attempt, 63,
                "net-WHNF fixture did not reach its promise wait"
            );
        }
        unreachable!("bounded block loop must block or panic")
    };
    context.values().with_runtime_value_access(|access| {
        let checkpoint = machine
            .lazy
            .access(&access)
            .expect("the net fixture must share its value domain")
            .checkpoint_snapshot()
            .expect("semantic blockage must remain in a managed checkpoint");
        assert_eq!(checkpoint.kind(), ManagedLazyCheckpointKindTag::NetWhnf);
    });

    let routed_report = context
        .values()
        .collect_managed_for_test()
        .expect("the active route and managed net checkpoint must survive collection");
    drop(machine);
    let retained_report = context
        .values()
        .collect_managed_for_test()
        .expect("the edge-owned net checkpoint must survive collection");
    assert_eq!(
        routed_report.root_entries(),
        retained_report.root_entries() + 1,
        "dropping the route must retire exactly its lazy root while the checkpoint and semantic subscription remain live"
    );

    let callable = crate::eval::test_support::closed_function_value_in(
        context.values(),
        1,
        crate::eval::test_support::TestExpr::Value(context.values().unit()),
    );
    crate::core::set_test_promise(context.values(), &promise, callable)
        .expect("the retained callable promise should accept its assignment");
    pump_to_ready(&context, &wait);

    let retained = context
        .values()
        .with_runtime_value_access(|access| LazyValue::from_root(&retained, &access));
    let mut resumed = lazy_machine(&context, retained);
    let value = 'resume: {
        for attempt in 0..64 {
            match resumed.poll(
                &poll,
                &mut crate::evaluation::EvaluationStepBudget::new(256),
            ) {
                EvaluationMachinePoll::Yielded => {}
                EvaluationMachinePoll::Complete(value) => break 'resume value,
                EvaluationMachinePoll::Failed(failure) => panic!("{failure}"),
                EvaluationMachinePoll::Blocked(block) => {
                    if let Some(WorkDependency::Wait(wait)) = block.dependency {
                        pump_to_ready(&context, &wait);
                    }
                }
                EvaluationMachinePoll::ScheduleSpark(_) => {
                    panic!("resumed net-WHNF checkpoint unexpectedly scheduled a spark")
                }
                EvaluationMachinePoll::Exit(_) => {
                    panic!("resumed net-WHNF checkpoint unexpectedly requested exit")
                }
                EvaluationMachinePoll::Cancelled => {
                    panic!("resumed net-WHNF checkpoint was unexpectedly cancelled")
                }
            }
            assert_ne!(attempt, 63, "retained net-WHNF fixture did not complete");
        }
        unreachable!("bounded resume loop must complete or panic")
    };
    assert_eq!(value.clone_core_for_test(), context.values().unit());
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
