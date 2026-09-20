use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::core::{Builtin, ListEffectComputation};

fn isolated_context() -> crate::evaluation::OwnedEvalContext {
    EvalContext::isolated(crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    ))
}

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
    let context = isolated_context();
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

fn assert_object_checkpoint(context: &EvalContext, machine: &LazyTaskMachine) {
    context.values().with_runtime_value_access(|access| {
        let checkpoint = machine
            .lazy
            .access(&access)
            .expect("the object fixture must share its value domain")
            .checkpoint_snapshot()
            .expect("object progress must remain in its managed checkpoint");
        assert_eq!(
            checkpoint.kind(),
            ManagedLazyCheckpointKindTag::ObjectFixpoint
        );
    });
}

fn resume_after_object_route_loss(
    context: &EvalContext,
    retained: &ManagedLazyRoot,
    machine: LazyTaskMachine,
) -> LazyTaskMachine {
    assert_object_checkpoint(context, &machine);
    drop(machine);
    collect_between_handoffs(context);
    let lazy = context
        .values()
        .with_runtime_value_access(|access| LazyValue::from_root(retained, &access));
    lazy_machine(context, lazy)
}

fn assert_list_effect_checkpoint(context: &EvalContext, machine: &LazyTaskMachine) {
    context.values().with_runtime_value_access(|access| {
        let checkpoint = machine
            .lazy
            .access(&access)
            .expect("the list-effect fixture must share its value domain")
            .checkpoint_snapshot()
            .expect("list-effect progress must remain in its managed checkpoint");
        assert_eq!(checkpoint.kind(), ManagedLazyCheckpointKindTag::ListEffect);
    });
}

fn resume_after_list_effect_route_loss(
    context: &EvalContext,
    retained: &ManagedLazyRoot,
    machine: LazyTaskMachine,
) -> LazyTaskMachine {
    assert_list_effect_checkpoint(context, &machine);
    drop(machine);
    collect_between_handoffs(context);
    let lazy = context
        .values()
        .with_runtime_value_access(|access| LazyValue::from_root(retained, &access));
    lazy_machine(context, lazy)
}

fn retained_list_effect_machine(
    context: &EvalContext,
    label: &'static str,
    recipe: ListEffectComputation,
) -> (ManagedLazyRoot, LazyTaskMachine) {
    let retained = context.values().with_runtime_value_access(|access| {
        LazyValue::list_effect_computation_in(&access, label, recipe).root_in(&access)
    });
    let lazy = context
        .values()
        .with_runtime_value_access(|access| LazyValue::from_root(&retained, &access));
    let machine = lazy_machine(context, lazy);
    (retained, machine)
}

fn poll_list_effect_until_blocked(
    context: &EvalContext,
    retained: &ManagedLazyRoot,
    mut machine: LazyTaskMachine,
) -> (LazyTaskMachine, WorkDependency) {
    let poll = crate::evaluation::EvaluationPollContext::for_context(context);
    for attempt in 0..256 {
        match machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)) {
            EvaluationMachinePoll::Yielded => {
                machine = resume_after_list_effect_route_loss(context, retained, machine);
            }
            EvaluationMachinePoll::Blocked(block) => {
                return (
                    machine,
                    block
                        .dependency
                        .expect("list-effect blockage must expose its exact dependency"),
                );
            }
            EvaluationMachinePoll::Complete(_) => panic!("list-effect fixture completed early"),
            EvaluationMachinePoll::Failed(failure) => {
                panic!("list-effect fixture failed: {failure}")
            }
            EvaluationMachinePoll::ScheduleSpark(_)
            | EvaluationMachinePoll::Exit(_)
            | EvaluationMachinePoll::Cancelled => {
                panic!("list-effect fixture crossed an unexpected orchestration boundary")
            }
        }
        assert_ne!(attempt, 255, "list-effect fixture did not reach its wait");
    }
    unreachable!("bounded list-effect fixture loop must block or panic")
}

fn drive_list_effect_after_route_loss(
    context: &EvalContext,
    retained: &ManagedLazyRoot,
    mut machine: LazyTaskMachine,
) -> Value {
    let poll = crate::evaluation::EvaluationPollContext::for_context(context);
    for attempt in 0..512 {
        match machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)) {
            EvaluationMachinePoll::Yielded => {
                machine = resume_after_list_effect_route_loss(context, retained, machine);
            }
            EvaluationMachinePoll::Blocked(block) => {
                let dependency = block
                    .dependency
                    .expect("list-effect blockage must expose its exact dependency");
                machine = resume_after_list_effect_route_loss(context, retained, machine);
                match dependency {
                    WorkDependency::Wait(wait) => pump_to_ready(context, &wait),
                    WorkDependency::Promise(_) => {
                        panic!("the self-contained list-effect fixture left an unresolved promise")
                    }
                    WorkDependency::Test(_) => {
                        panic!("the list-effect fixture exposed a synthetic dependency")
                    }
                }
            }
            EvaluationMachinePoll::Complete(value) => return value.clone_core_for_test(),
            EvaluationMachinePoll::Failed(failure) => {
                panic!("list-effect fixture failed: {failure}")
            }
            EvaluationMachinePoll::ScheduleSpark(_)
            | EvaluationMachinePoll::Exit(_)
            | EvaluationMachinePoll::Cancelled => {
                panic!("list-effect fixture crossed an unexpected orchestration boundary")
            }
        }
        assert_ne!(attempt, 511, "list-effect fixture did not complete");
    }
    unreachable!("bounded list-effect fixture loop must complete or panic")
}

fn fixed_list_handler(context: &EvalContext, value: Value) -> Value {
    crate::eval::test_support::closed_function_value_in(
        context.values(),
        1,
        crate::eval::test_support::TestExpr::Value(Value::List(List::from_values(vec![value]))),
    )
}

fn list_effect_value(function: Value) -> Value {
    Value::Dict(Dict::new_sync().insert((*crate::core::keys::EFF).clone(), function))
}

fn poll_object_until_blocked(
    machine: &mut LazyTaskMachine,
    poll: &crate::evaluation::EvaluationPollContext,
) -> WorkDependency {
    for attempt in 0..128 {
        match machine.poll(poll, &mut crate::evaluation::EvaluationStepBudget::new(1)) {
            EvaluationMachinePoll::Yielded => {}
            EvaluationMachinePoll::Blocked(block) => {
                return block
                    .dependency
                    .expect("object blockage must expose its exact dependency");
            }
            EvaluationMachinePoll::Failed(failure) => panic!("object fixture failed: {failure}"),
            EvaluationMachinePoll::Complete(_) => panic!("object fixture completed early"),
            EvaluationMachinePoll::ScheduleSpark(_)
            | EvaluationMachinePoll::Exit(_)
            | EvaluationMachinePoll::Cancelled => {
                panic!("object fixture crossed an unexpected orchestration boundary")
            }
        }
        assert_ne!(attempt, 127, "object fixture did not reach its next wait");
    }
    unreachable!("bounded object fixture loop must block or panic")
}

fn object_spec(name: &str, deps: Value) -> Value {
    Value::Dict(
        Dict::new_sync()
            .insert(
                (*crate::core::keys::NAME).clone(),
                Value::binary_from_text(name),
            )
            .insert((*crate::core::keys::DEPS).clone(), deps),
    )
}

#[test]
fn host_call_yields_on_both_sides_and_consumes_its_result_once() {
    let context = isolated_context();
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
    let context = isolated_context();
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
    let context = isolated_context();
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
    let context = isolated_context();
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
fn object_checkpoint_preserves_linearization_prefixes_across_route_loss_and_collection() {
    let context = isolated_context();
    let root_spec = PromisedValue::new(context.values(), "retained object spec");
    let root_name = PromisedValue::new(context.values(), "retained object name");
    let dependency_chunk = PromisedValue::new(context.values(), "retained dependency chunk");
    let second_dependency = PromisedValue::new(context.values(), "retained nested dependency spec");
    let _root_name_owner = root_name.root(context.values());
    let _dependency_chunk_owner = dependency_chunk.root(context.values());
    let _second_dependency_owner = second_dependency.root(context.values());
    let first_dependency_demands = Arc::new(AtomicUsize::new(0));
    let observed_first_dependency = Arc::clone(&first_dependency_demands);
    let first_dependency = LazyValue::semantic_thunk(
        context.values(),
        "counted first object dependency",
        move |_| {
            observed_first_dependency.fetch_add(1, Ordering::SeqCst);
            Ok(object_spec("first dependency", Value::List(List::empty())))
        },
    );
    let _first_dependency_owner = first_dependency.root(context.values());
    let first_dependency = Value::Lazy(first_dependency);
    let lazy = LazyValue::computed_fixpoint(
        context.values(),
        "retained object fixpoint",
        FixpointComputation::ObjectInstance(Value::Promised(root_spec.clone())),
    );
    let retained = lazy.root(context.values());
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);
    let mut machine = lazy_machine(&context, lazy);

    let _spec_wait = poll_object_until_blocked(&mut machine, &poll);
    machine = resume_after_object_route_loss(&context, &retained, machine);
    crate::core::set_test_promise(
        context.values(),
        &root_spec,
        Value::Dict(
            Dict::new_sync()
                .insert(
                    (*crate::core::keys::NAME).clone(),
                    Value::Promised(root_name.clone()),
                )
                .insert(
                    (*crate::core::keys::DEPS).clone(),
                    Value::List(List::from_thunk(ListThunk::Promised(
                        dependency_chunk.clone(),
                    ))),
                ),
        ),
    )
    .expect("the retained root spec should accept its assignment");

    let _name_wait = poll_object_until_blocked(&mut machine, &poll);
    machine = resume_after_object_route_loss(&context, &retained, machine);
    crate::core::set_test_promise(
        context.values(),
        &root_name,
        Value::binary_from_text("root"),
    )
    .expect("the retained root name should accept its assignment");

    let _chunk_wait = poll_object_until_blocked(&mut machine, &poll);
    machine = resume_after_object_route_loss(&context, &retained, machine);
    crate::core::set_test_promise(
        context.values(),
        &dependency_chunk,
        Value::List(List::from_values(vec![
            first_dependency,
            Value::Promised(second_dependency.clone()),
        ])),
    )
    .expect("the retained dependency chunk should accept its assignment");

    let first_dependency_wait = poll_object_until_blocked(&mut machine, &poll);
    let WorkDependency::Wait(first_dependency_wait) = first_dependency_wait else {
        panic!("the counted lazy dependency must expose its producer wait")
    };
    pump_to_ready(&context, &first_dependency_wait);
    let nested_wait = poll_object_until_blocked(&mut machine, &poll);
    assert!(matches!(nested_wait, WorkDependency::Promise(_)));
    assert_eq!(first_dependency_demands.load(Ordering::SeqCst), 1);
    machine = resume_after_object_route_loss(&context, &retained, machine);
    crate::core::set_test_promise(
        context.values(),
        &second_dependency,
        object_spec("second dependency", Value::List(List::empty())),
    )
    .expect("the retained nested dependency should accept its assignment");

    let value = 'complete: {
        for attempt in 0..256 {
            match machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)) {
                EvaluationMachinePoll::Yielded => {
                    machine = resume_after_object_route_loss(&context, &retained, machine);
                }
                EvaluationMachinePoll::Complete(value) => break 'complete value,
                EvaluationMachinePoll::Blocked(block) => {
                    if let Some(WorkDependency::Wait(wait)) = block.dependency {
                        pump_to_ready(&context, &wait);
                    } else {
                        panic!("completed dependencies must not leave another promise wait")
                    }
                }
                EvaluationMachinePoll::Failed(failure) => {
                    panic!("object fixture failed: {failure}")
                }
                EvaluationMachinePoll::ScheduleSpark(_)
                | EvaluationMachinePoll::Exit(_)
                | EvaluationMachinePoll::Cancelled => {
                    panic!("object fixture crossed an unexpected orchestration boundary")
                }
            }
            assert_ne!(attempt, 255, "retained object fixture did not complete");
        }
        unreachable!("bounded retained-object loop must complete or panic")
    };
    assert!(matches!(value.clone_core_for_test(), Value::Dict(_)));
    assert_eq!(
        first_dependency_demands.load(Ordering::SeqCst),
        1,
        "a completed dependency prefix must not be replayed by a new route"
    );
}

#[test]
fn object_checkpoint_does_not_replay_mixin_stages_after_route_loss() {
    let context = isolated_context();
    let defs_demands = Arc::new(AtomicUsize::new(0));
    let base_result = PromisedValue::new(context.values(), "retained object base application");
    let self_result = PromisedValue::new(context.values(), "retained object self application");
    let _base_result_owner = base_result.root(context.values());
    let _self_result_owner = self_result.root(context.values());
    let self_function = crate::eval::test_support::closed_function_value_in(
        context.values(),
        1,
        crate::eval::test_support::TestExpr::Value(Value::Promised(self_result.clone())),
    );
    let _self_function_owner =
        crate::runtime::RuntimeValueRoot::new(context.values(), self_function.clone());
    let base_function = crate::eval::test_support::closed_function_value_in(
        context.values(),
        1,
        crate::eval::test_support::TestExpr::Value(Value::Promised(base_result.clone())),
    );
    let _base_function_owner =
        crate::runtime::RuntimeValueRoot::new(context.values(), base_function.clone());

    let observed_defs = Arc::clone(&defs_demands);
    let definitions = Value::Lazy(LazyValue::semantic_thunk(
        context.values(),
        "counted object definitions demand",
        move |_| {
            observed_defs.fetch_add(1, Ordering::SeqCst);
            Ok(base_function.clone())
        },
    ));
    let spec = Value::Dict(
        Dict::new_sync()
            .insert(
                (*crate::core::keys::NAME).clone(),
                Value::binary_from_text("root"),
            )
            .insert(
                (*crate::core::keys::DEPS).clone(),
                Value::List(List::empty()),
            )
            .insert((*crate::core::keys::DEFS).clone(), definitions),
    );
    let lazy = LazyValue::computed_fixpoint(
        context.values(),
        "counted object mix",
        FixpointComputation::ObjectInstance(spec),
    );
    let _retained = lazy.root(context.values());
    let object = Value::Lazy(lazy);
    #[cfg(feature = "interaction-net-profiling")]
    let profile_before = context.values().interaction_net_profile_snapshot();

    eval_value(&context, &object).expect_err("the base application must suspend");
    collect_between_handoffs(&context);
    assert_eq!(defs_demands.load(Ordering::SeqCst), 1);
    crate::core::set_test_promise(context.values(), &base_result, self_function)
        .expect("the retained base application should accept its function result");

    eval_value(&context, &object).expect_err("the self application must suspend");
    collect_between_handoffs(&context);
    assert_eq!(defs_demands.load(Ordering::SeqCst), 1);
    crate::core::set_test_promise(
        context.values(),
        &self_result,
        Value::Dict(Dict::new_sync().insert(Key::binary_from_text("answer"), number(42))),
    )
    .expect("the retained self application should accept its result");

    let Value::Dict(value) =
        eval_value(&context, &object).expect("the retained mixin should finish")
    else {
        panic!("the counted mixin must produce an object dictionary")
    };
    assert_eq!(
        value.get(&Key::binary_from_text("answer")),
        Some(&number(42))
    );
    assert_eq!(defs_demands.load(Ordering::SeqCst), 1);
    #[cfg(feature = "interaction-net-profiling")]
    {
        let profile_after = context.values().interaction_net_profile_snapshot();
        assert_eq!(
            profile_after.reductions.call - profile_before.reductions.call,
            2,
            "the base and self mixin applications must each commit exactly once"
        );
    }
}

#[test]
fn list_effect_run_checkpoint_does_not_replay_effect_or_handler_demand() {
    let context = isolated_context();
    let effect_demands = Arc::new(AtomicUsize::new(0));
    let handler_demands = Arc::new(AtomicUsize::new(0));

    let handler = fixed_list_handler(&context, number(42));
    let _handler_owner = crate::runtime::RuntimeValueRoot::new(context.values(), handler.clone());
    let observed_handler = Arc::clone(&handler_demands);
    let counted_handler = Value::Lazy(LazyValue::semantic_thunk(
        context.values(),
        "counted list-effect handler",
        move |_| {
            observed_handler.fetch_add(1, Ordering::SeqCst);
            Ok(handler.clone())
        },
    ));
    let effect = list_effect_value(counted_handler);
    let _effect_owner = crate::runtime::RuntimeValueRoot::new(context.values(), effect.clone());
    let observed_effect = Arc::clone(&effect_demands);
    let counted_effect = Value::Lazy(LazyValue::semantic_thunk(
        context.values(),
        "counted list effect",
        move |_| {
            observed_effect.fetch_add(1, Ordering::SeqCst);
            Ok(effect.clone())
        },
    ));
    let (retained, machine) = retained_list_effect_machine(
        &context,
        "retained list-effect run",
        ListEffectComputation::Run {
            effect: counted_effect,
        },
    );

    let value = drive_list_effect_after_route_loss(&context, &retained, machine);
    assert_eq!(value, Value::List(List::from_values(vec![number(42)])));
    assert_eq!(effect_demands.load(Ordering::SeqCst), 1);
    assert_eq!(handler_demands.load(Ordering::SeqCst), 1);

    let cached = context.values().with_runtime_value_access(|access| {
        lazy_machine(&context, LazyValue::from_root(&retained, &access))
    });
    assert_eq!(
        drive_list_effect_after_route_loss(&context, &retained, cached),
        Value::List(List::from_values(vec![number(42)]))
    );
    assert_eq!(effect_demands.load(Ordering::SeqCst), 1);
    assert_eq!(handler_demands.load(Ordering::SeqCst), 1);
}

#[test]
fn list_effect_sequence_and_cut_checkpoints_survive_deferred_chunks_and_route_loss() {
    let context = isolated_context();

    let sequence_chunk = PromisedValue::new(context.values(), "retained sequence chunk");
    let _sequence_chunk_owner = sequence_chunk.root(context.values());
    let (sequence_retained, sequence_machine) = retained_list_effect_machine(
        &context,
        "retained list-effect sequence",
        ListEffectComputation::Sequence {
            results: List::from_thunk(ListThunk::Promised(sequence_chunk.clone())),
            continuation: Value::Builtin(Builtin::Add),
        },
    );
    let (sequence_machine, sequence_dependency) =
        poll_list_effect_until_blocked(&context, &sequence_retained, sequence_machine);
    assert!(matches!(sequence_dependency, WorkDependency::Promise(_)));
    let sequence_machine =
        resume_after_list_effect_route_loss(&context, &sequence_retained, sequence_machine);
    crate::core::set_test_promise(
        context.values(),
        &sequence_chunk,
        Value::List(List::from_values(vec![number(1)])),
    )
    .expect("the deferred sequence chunk should accept its assignment");
    assert!(matches!(
        drive_list_effect_after_route_loss(&context, &sequence_retained, sequence_machine,),
        Value::List(_)
    ));

    let cut_operation = PromisedValue::new(context.values(), "retained cut operation");
    let _cut_operation_owner = cut_operation.root(context.values());
    let (cut_retained, cut_machine) = retained_list_effect_machine(
        &context,
        "retained list-effect cut",
        ListEffectComputation::Cut {
            operation: Value::Promised(cut_operation.clone()),
        },
    );
    let (cut_machine, cut_dependency) =
        poll_list_effect_until_blocked(&context, &cut_retained, cut_machine);
    let cut_machine = resume_after_list_effect_route_loss(&context, &cut_retained, cut_machine);
    crate::core::set_test_promise(
        context.values(),
        &cut_operation,
        list_effect_value(fixed_list_handler(&context, number(43))),
    )
    .expect("the deferred cut operation should accept its assignment");
    if let WorkDependency::Wait(wait) = cut_dependency {
        pump_to_ready(&context, &wait);
    }
    assert_eq!(
        drive_list_effect_after_route_loss(&context, &cut_retained, cut_machine),
        Value::List(List::from_values(vec![number(43)]))
    );
}

#[test]
fn direct_result_list_effect_recipes_preserve_order_and_route_loss_progress() {
    let context = isolated_context();
    let first_chunk = PromisedValue::new(context.values(), "direct-result first chunk");
    let _first_chunk_owner = first_chunk.root(context.values());
    let results = List::concat(
        List::from_thunk(ListThunk::Promised(first_chunk.clone())),
        List::from_values(vec![number(2)]),
    );
    let continuation = crate::eval::test_support::closed_function_value_in(
        context.values(),
        1,
        crate::eval::test_support::TestExpr::List(Arc::from([Arc::new(
            crate::eval::test_support::TestExpr::Local(0),
        )])),
    );
    let (retained, machine) = retained_list_effect_machine(
        &context,
        "direct-result flat-map",
        ListEffectComputation::FlatMapResults {
            results,
            continuation,
        },
    );
    let (machine, dependency) = poll_list_effect_until_blocked(&context, &retained, machine);
    assert!(matches!(dependency, WorkDependency::Promise(_)));
    let machine = resume_after_list_effect_route_loss(&context, &retained, machine);
    crate::core::set_test_promise(
        context.values(),
        &first_chunk,
        Value::List(List::from_values(vec![number(1)])),
    )
    .expect("the direct-result prefix should accept its assignment");
    let mapped = drive_list_effect_after_route_loss(&context, &retained, machine);

    for (index, expected) in [number(1), number(2)].into_iter().enumerate() {
        let selected = Value::builtin_call(
            context.values(),
            Builtin::ListAt,
            vec![number(index as i64), mapped.clone()],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &selected)
                .expect("direct-result item should evaluate"),
            expected
        );
    }

    let (retained, machine) = retained_list_effect_machine(
        &context,
        "direct first result",
        ListEffectComputation::FirstResult {
            results: List::from_values(vec![number(3), number(4)]),
        },
    );
    assert_eq!(
        drive_list_effect_after_route_loss(&context, &retained, machine),
        Value::List(List::from_values(vec![number(3)]))
    );
}

#[test]
fn list_effect_fix_checkpoint_constructs_and_assigns_one_promise() {
    let context = isolated_context();
    let function_demands = Arc::new(AtomicUsize::new(0));
    let operation_demands = Arc::new(AtomicUsize::new(0));

    let handler = fixed_list_handler(&context, number(44));
    let effect = list_effect_value(handler);
    let _effect_owner = crate::runtime::RuntimeValueRoot::new(context.values(), effect.clone());
    let observed_operation = Arc::clone(&operation_demands);
    let operation =
        LazyValue::semantic_thunk(context.values(), "counted list-fix operation", move |_| {
            observed_operation.fetch_add(1, Ordering::SeqCst);
            Ok(effect.clone())
        });
    let function = crate::eval::test_support::closed_function_value_in(
        context.values(),
        1,
        crate::eval::test_support::TestExpr::Value(Value::Lazy(operation)),
    );
    let _function_owner = crate::runtime::RuntimeValueRoot::new(context.values(), function.clone());
    let observed_function = Arc::clone(&function_demands);
    let counted_function = Value::Lazy(LazyValue::semantic_thunk(
        context.values(),
        "counted list-fix function",
        move |_| {
            observed_function.fetch_add(1, Ordering::SeqCst);
            Ok(function.clone())
        },
    ));
    let (retained, machine) = retained_list_effect_machine(
        &context,
        "retained list-effect fix",
        ListEffectComputation::FixFunction {
            function: counted_function,
            alternative: 0,
        },
    );
    let lifecycle_before = context.values().managed_promise_lifecycle_counts_for_test();

    let fixed = drive_list_effect_after_route_loss(&context, &retained, machine);
    let first = Value::builtin_call(context.values(), Builtin::ListAt, vec![number(0), fixed]);
    assert_eq!(
        crate::eval::eval_value(&context, &first)
            .expect("the first fixed alternative should evaluate"),
        number(44)
    );
    assert_eq!(function_demands.load(Ordering::SeqCst), 1);
    assert_eq!(operation_demands.load(Ordering::SeqCst), 1);
    let lifecycle_after = context.values().managed_promise_lifecycle_counts_for_test();
    assert_eq!(lifecycle_after.0 - lifecycle_before.0, 1);
    assert_eq!(lifecycle_after.1 - lifecycle_before.1, 1);

    let cached = context.values().with_runtime_value_access(|access| {
        lazy_machine(&context, LazyValue::from_root(&retained, &access))
    });
    let fixed = drive_list_effect_after_route_loss(&context, &retained, cached);
    let first = Value::builtin_call(context.values(), Builtin::ListAt, vec![number(0), fixed]);
    assert_eq!(
        crate::eval::eval_value(&context, &first)
            .expect("the cached fixed alternative should evaluate"),
        number(44)
    );
    assert_eq!(
        context.values().managed_promise_lifecycle_counts_for_test(),
        lifecycle_after,
        "a later route must not manufacture or assign another fix promise"
    );
}

#[test]
fn list_effect_fix_allocates_one_future_for_each_observed_alternative() {
    let context = isolated_context();
    let handler = crate::eval::test_support::closed_function_value_in(
        context.values(),
        1,
        crate::eval::test_support::TestExpr::Value(Value::List(List::from_values(vec![
            number(51),
            number(52),
        ]))),
    );
    let effect = list_effect_value(handler);
    let function = crate::eval::test_support::closed_function_value_in(
        context.values(),
        1,
        crate::eval::test_support::TestExpr::Value(effect),
    );
    let fixed = crate::eval::eval_value(
        &context,
        &Value::builtin_call(context.values(), Builtin::ListEffectFix, vec![function]),
    )
    .expect("list-effect fix construction should evaluate");
    let lifecycle_before = context.values().managed_promise_lifecycle_counts_for_test();

    for (index, expected_count) in [(0, 1), (1, 2)] {
        let selected = Value::builtin_call(
            context.values(),
            Builtin::ListAt,
            vec![number(index), fixed.clone()],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &selected)
                .expect("the selected fixed alternative should evaluate"),
            number(51 + index)
        );
        let lifecycle = context.values().managed_promise_lifecycle_counts_for_test();
        assert_eq!(lifecycle.0 - lifecycle_before.0, expected_count);
        assert_eq!(lifecycle.1 - lifecycle_before.1, expected_count);
    }
}

#[test]
fn failed_host_call_is_not_replayed_after_publication() {
    let context = isolated_context();
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
    let context = isolated_context();
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
    let context = isolated_context();
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
