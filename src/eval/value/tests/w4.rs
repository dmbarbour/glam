use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::core::{Builtin, BuiltinCall, ListEffectComputation};
use crate::eval::list_machine::{ListFrontMachine, ListFrontPoll};

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
                panic!("deterministic W6G.1 fixture lost its producer: {wait:?}")
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

fn builder_effect_value(context: &EvalContext, operation: Value) -> Value {
    let handler = crate::eval::test_support::closed_function_value_in(
        context.values(),
        1,
        crate::eval::test_support::TestExpr::Value(operation),
    );
    list_effect_value(handler)
}

fn construction_results_effect(context: &EvalContext, results: List) -> Value {
    let handler = crate::eval::test_support::closed_function_value_in(
        context.values(),
        2,
        crate::eval::test_support::TestExpr::Value(Value::List(results)),
    );
    list_effect_value(handler)
}

fn assert_construction_promise_dependency(
    context: &EvalContext,
    dependency: WorkDependency,
    promise: &PromisedValue,
) {
    let WorkDependency::Promise(dependency) = dependency else {
        panic!("public construction must publish an exact promise dependency")
    };
    assert_eq!(dependency.id(), promise.id(context.values()));
}

fn builder_return_first_port_continuation(context: &EvalContext) -> Value {
    use crate::eval::test_support::TestExpr;

    let first = TestExpr::Apply(
        Arc::new(TestExpr::Value(Value::Builtin(Builtin::ListHead))),
        Arc::new(TestExpr::Local(0)),
    );
    let call = TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(Builtin::EffectCall))),
            Arc::new(TestExpr::Value(
                context
                    .values()
                    .key_value(&crate::core::Key::atom_from_text("r")),
            )),
        )),
        Arc::new(TestExpr::List(Arc::from([Arc::new(first)]))),
    );
    let effect = TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(Builtin::DictSingleton))),
            Arc::new(TestExpr::Value(
                context.values().key_value(&crate::core::keys::EFF),
            )),
        )),
        Arc::new(call),
    );
    crate::eval::test_support::closed_function_value_in(context.values(), 1, effect)
}

fn list_front(context: &EvalContext, list: Value) -> Option<(Value, Value)> {
    let mut machine = ListFrontMachine::unowned(crate::runtime::RuntimeValueRoot::new(
        context.values(),
        list,
    ));
    let poll = crate::evaluation::EvaluationPollContext::for_context(context);
    for attempt in 0..256 {
        let outcome = crate::eval::with_direct_evaluator(context, |evaluator| {
            machine.poll(
                &poll,
                evaluator,
                context,
                &mut crate::evaluation::EvaluationStepBudget::new(1),
            )
        });
        match outcome {
            ListFrontPoll::Ready(front) => {
                return front
                    .map(|(head, tail)| (head.clone_core_for_test(), tail.clone_core_for_test()));
            }
            ListFrontPoll::Pending(WorkDependency::Wait(wait)) => pump_to_ready(context, &wait),
            ListFrontPoll::Pending(WorkDependency::Promise(_)) => {
                panic!("the fix-list spine must not expose its promised element as a dependency")
            }
            ListFrontPoll::Pending(WorkDependency::Test(_)) => {
                panic!("the fix-list spine exposed a synthetic dependency")
            }
            ListFrontPoll::Yielded => {}
            ListFrontPoll::Failed(failure) => panic!("fix-list front failed: {failure}"),
        }
        assert_ne!(attempt, 255, "fix-list front exhausted its poll bound");
    }
    unreachable!("bounded fix-list front must complete or panic")
}

fn fix_function_returning_its_future_twice(context: &EvalContext) -> Value {
    let handler_code = Arc::new(crate::eval::test_support::lower_test_function_code_in(
        context.values(),
        1,
        crate::eval::test_support::TestExpr::List(Arc::from([
            Arc::new(crate::eval::test_support::TestExpr::Local(1)),
            Arc::new(crate::eval::test_support::TestExpr::Local(1)),
        ])),
    ));
    let handler = crate::eval::test_support::TestExpr::Function {
        code: handler_code,
        captures: Arc::from([Arc::new(crate::eval::test_support::TestExpr::Local(0))]),
    };
    let eff_key = context
        .values()
        .with_runtime_value_access(|access| access.values().key_value(&crate::core::keys::EFF));
    let effect = crate::eval::test_support::TestExpr::Apply(
        Arc::new(crate::eval::test_support::TestExpr::Apply(
            Arc::new(crate::eval::test_support::TestExpr::Value(Value::Builtin(
                Builtin::DictSingleton,
            ))),
            Arc::new(crate::eval::test_support::TestExpr::Value(eff_key)),
        )),
        Arc::new(handler),
    );
    crate::eval::test_support::closed_function_value_in(context.values(), 1, effect)
}

fn retained_application_machine(
    context: &EvalContext,
    function: Value,
    arguments: Arc<[Value]>,
) -> (ManagedLazyRoot, LazyTaskMachine) {
    let retained =
        LazyValue::from_application(context.values(), function, arguments).root(context.values());
    let lazy = context
        .values()
        .with_runtime_value_access(|access| LazyValue::from_root(&retained, &access));
    (retained, lazy_machine(context, lazy))
}

fn retained_lazy_machine(
    context: &EvalContext,
    value: Value,
) -> (ManagedLazyRoot, LazyTaskMachine) {
    let Value::Lazy(lazy) = value else {
        panic!("the retained fixture must be a lazy value")
    };
    let retained = lazy.root(context.values());
    (retained, lazy_machine(context, lazy))
}

fn resume_after_lazy_route_loss(
    context: &EvalContext,
    retained: &ManagedLazyRoot,
    machine: LazyTaskMachine,
) -> LazyTaskMachine {
    drop(machine);
    collect_between_handoffs(context);
    let lazy = context
        .values()
        .with_runtime_value_access(|access| LazyValue::from_root(retained, &access));
    lazy_machine(context, lazy)
}

fn assert_lazy_checkpoint_kind(
    context: &EvalContext,
    machine: &LazyTaskMachine,
    expected: ManagedLazyCheckpointKindTag,
) {
    context.values().with_runtime_value_access(|access| {
        let checkpoint = machine
            .lazy
            .access(&access)
            .expect("the fixture route must share its value domain")
            .checkpoint_snapshot()
            .expect("blocked lazy work must retain a managed checkpoint");
        assert_eq!(checkpoint.kind(), expected);
    });
}

fn resume_after_builtin_route_loss(
    context: &EvalContext,
    retained: &ManagedLazyRoot,
    machine: LazyTaskMachine,
) -> LazyTaskMachine {
    assert_lazy_checkpoint_kind(context, &machine, ManagedLazyCheckpointKindTag::Builtin);
    let machine = resume_after_lazy_route_loss(context, retained, machine);
    assert_lazy_checkpoint_kind(context, &machine, ManagedLazyCheckpointKindTag::Builtin);
    machine
}

fn poll_until_blocked_after_route_loss(
    context: &EvalContext,
    retained: &ManagedLazyRoot,
    mut machine: LazyTaskMachine,
    route_losses: &mut usize,
) -> (LazyTaskMachine, WorkDependency) {
    let poll = crate::evaluation::EvaluationPollContext::for_context(context);
    for attempt in 0..512 {
        match machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)) {
            EvaluationMachinePoll::Yielded => {
                machine = resume_after_lazy_route_loss(context, retained, machine);
                *route_losses += 1;
            }
            EvaluationMachinePoll::Blocked(block) => {
                let dependency = block
                    .dependency
                    .expect("the blocked fixture must expose an exact dependency");
                match dependency {
                    WorkDependency::Wait(wait) => {
                        pump_to_ready(context, &wait);
                        machine = resume_after_lazy_route_loss(context, retained, machine);
                        *route_losses += 1;
                    }
                    WorkDependency::Promise(promise) => {
                        return (machine, WorkDependency::Promise(promise));
                    }
                    WorkDependency::Test(_) => {
                        panic!("the fixture exposed a synthetic dependency")
                    }
                }
            }
            EvaluationMachinePoll::Complete(_) => panic!("the fixture completed before blocking"),
            EvaluationMachinePoll::Failed(failure) => panic!("the fixture failed: {failure}"),
            EvaluationMachinePoll::ScheduleSpark(_)
            | EvaluationMachinePoll::Exit(_)
            | EvaluationMachinePoll::Cancelled => {
                panic!("the fixture crossed an unexpected orchestration boundary")
            }
        }
        assert_ne!(attempt, 511, "the fixture did not reach its dependency");
    }
    unreachable!("the bounded fixture must block or panic")
}

fn poll_until_stalled_child_wait_after_route_loss(
    context: &EvalContext,
    retained: &ManagedLazyRoot,
    mut machine: LazyTaskMachine,
    route_losses: &mut usize,
) -> (LazyTaskMachine, crate::evaluation::EvaluationWaitToken) {
    let poll = crate::evaluation::EvaluationPollContext::for_context(context);
    for attempt in 0..512 {
        match machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)) {
            EvaluationMachinePoll::Yielded => {
                machine = resume_after_lazy_route_loss(context, retained, machine);
                *route_losses += 1;
            }
            EvaluationMachinePoll::Blocked(block) => {
                let Some(WorkDependency::Wait(wait)) = block.dependency else {
                    panic!("nested construction must expose its exact child wait")
                };
                match context.pump_wait(&wait, 256) {
                    crate::evaluation::EvaluationPumpOutcome::NoProgress => return (machine, wait),
                    crate::evaluation::EvaluationPumpOutcome::TargetReady
                    | crate::evaluation::EvaluationPumpOutcome::BudgetExhausted => {
                        machine = resume_after_lazy_route_loss(context, retained, machine);
                        *route_losses += 1;
                    }
                    crate::evaluation::EvaluationPumpOutcome::Busy => {
                        panic!("deterministic nested construction found a claimed producer")
                    }
                }
            }
            EvaluationMachinePoll::Complete(_) => panic!("construction completed before its wait"),
            EvaluationMachinePoll::Failed(failure) => {
                panic!("construction failed before its wait: {failure}")
            }
            EvaluationMachinePoll::ScheduleSpark(_)
            | EvaluationMachinePoll::Exit(_)
            | EvaluationMachinePoll::Cancelled => {
                panic!("construction crossed an unexpected orchestration boundary")
            }
        }
        assert_ne!(
            attempt, 511,
            "nested construction did not reach its stalled wait"
        );
    }
    unreachable!("bounded nested construction must block or panic")
}

fn drive_after_route_loss(
    context: &EvalContext,
    retained: &ManagedLazyRoot,
    mut machine: LazyTaskMachine,
    route_losses: &mut usize,
) -> Value {
    let poll = crate::evaluation::EvaluationPollContext::for_context(context);
    for attempt in 0..2048 {
        match machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)) {
            EvaluationMachinePoll::Yielded => {
                machine = resume_after_lazy_route_loss(context, retained, machine);
                *route_losses += 1;
            }
            EvaluationMachinePoll::Blocked(block) => {
                let dependency = block
                    .dependency
                    .expect("the blocked fixture must expose an exact dependency");
                match dependency {
                    WorkDependency::Wait(wait) => {
                        pump_to_ready(context, &wait);
                        machine = resume_after_lazy_route_loss(context, retained, machine);
                        *route_losses += 1;
                    }
                    WorkDependency::Promise(_) => {
                        panic!("the self-contained fixture left an unresolved promise")
                    }
                    WorkDependency::Test(_) => {
                        panic!("the self-contained fixture exposed a synthetic dependency")
                    }
                }
            }
            EvaluationMachinePoll::Complete(value) => return value.clone_core_for_test(),
            EvaluationMachinePoll::Failed(failure) => panic!("the fixture failed: {failure}"),
            EvaluationMachinePoll::ScheduleSpark(_)
            | EvaluationMachinePoll::Exit(_)
            | EvaluationMachinePoll::Cancelled => {
                panic!("the fixture crossed an unexpected orchestration boundary")
            }
        }
        assert_ne!(attempt, 2047, "the fixture exhausted its route-loss bound");
    }
    unreachable!("the bounded fixture must complete or panic")
}

fn drive_failure_after_route_loss(
    context: &EvalContext,
    retained: &ManagedLazyRoot,
    mut machine: LazyTaskMachine,
    route_losses: &mut usize,
) -> Arc<EvaluationFailure> {
    let poll = crate::evaluation::EvaluationPollContext::for_context(context);
    for attempt in 0..2048 {
        match machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)) {
            EvaluationMachinePoll::Yielded => {
                machine = resume_after_lazy_route_loss(context, retained, machine);
                *route_losses += 1;
            }
            EvaluationMachinePoll::Blocked(block) => {
                let dependency = block
                    .dependency
                    .expect("the blocked failure fixture must expose an exact dependency");
                match dependency {
                    WorkDependency::Wait(wait) => {
                        pump_to_ready(context, &wait);
                        machine = resume_after_lazy_route_loss(context, retained, machine);
                        *route_losses += 1;
                    }
                    WorkDependency::Promise(_) => {
                        panic!("the failure fixture left an unresolved promise")
                    }
                    WorkDependency::Test(_) => {
                        panic!("the failure fixture exposed a synthetic dependency")
                    }
                }
            }
            EvaluationMachinePoll::Failed(failure) => return failure.into_failure(),
            EvaluationMachinePoll::Complete(_) => {
                panic!("the failure fixture unexpectedly completed")
            }
            EvaluationMachinePoll::ScheduleSpark(_)
            | EvaluationMachinePoll::Exit(_)
            | EvaluationMachinePoll::Cancelled => {
                panic!("the failure fixture crossed an unexpected orchestration boundary")
            }
        }
        assert_ne!(
            attempt, 2047,
            "the failure fixture exhausted its route-loss bound"
        );
    }
    unreachable!("the bounded fixture must fail or panic")
}

fn counted_success(
    context: &EvalContext,
    label: &'static str,
    count: &Arc<AtomicUsize>,
    value: Value,
) -> Value {
    let value = crate::runtime::RuntimeValueRoot::new(context.values(), value);
    let observed = Arc::clone(count);
    Value::Lazy(LazyValue::semantic_thunk(
        context.values(),
        label,
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(value.clone_core_for_test())
        },
    ))
}

fn counted_failure(
    context: &EvalContext,
    label: &'static str,
    count: &Arc<AtomicUsize>,
    message: &'static str,
) -> Value {
    let observed = Arc::clone(count);
    Value::Lazy(LazyValue::semantic_thunk(
        context.values(),
        label,
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Err(EvaluationHalt::new(message))
        },
    ))
}

fn assert_transparent_failure(
    context: &EvalContext,
    failure: &EvaluationFailure,
    expected_message: &str,
) {
    assert!(failure.to_string().contains(expected_message), "{failure}");
    context.values().with_runtime_value_access(|access| {
        assert!(
            failure.contexts_in(&access).is_empty(),
            "private builder operands must propagate failures without operation-local contexts"
        );
    });
}

fn builder_failure_across_route_loss(context: &EvalContext, call: Value) -> Arc<EvaluationFailure> {
    let (retained, machine) = retained_lazy_machine(context, call);
    let mut route_losses = 0;
    let failure = drive_failure_after_route_loss(context, &retained, machine, &mut route_losses);
    assert!(
        route_losses > 0,
        "the builder failure must cross a forced route loss"
    );
    failure
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
    let function = fix_function_returning_its_future_twice(&context);
    let fixed = crate::eval::eval_value(
        &context,
        &Value::builtin_call(context.values(), Builtin::ListEffectFix, vec![function]),
    )
    .expect("list-effect fix construction should evaluate");
    let lifecycle_before = context.values().managed_promise_lifecycle_counts_for_test();

    let (first, tail) = list_front(&context, fixed).expect("the first alternative must exist");
    let Value::Promised(first) = first else {
        panic!("the first alternative must expose its own future")
    };
    let (second, exhausted) =
        list_front(&context, tail).expect("the second alternative must exist");
    let Value::Promised(second) = second else {
        panic!("the second alternative must expose its own future")
    };
    assert_ne!(
        first.id(context.values()),
        second.id(context.values()),
        "each observed alternative must allocate a distinct future"
    );
    for future in [&first, &second] {
        let assigned = future
            .assignment(context.values())
            .expect("the future must be assigned when its alternative publishes")
            .expect("the selected future must be assigned successfully");
        let Value::Promised(assigned) = assigned else {
            panic!("the selected head must assign the alternative's own future")
        };
        assert_eq!(future.id(context.values()), assigned.id(context.values()));
    }
    let lifecycle = context.values().managed_promise_lifecycle_counts_for_test();
    assert_eq!(lifecycle.0 - lifecycle_before.0, 2);
    assert_eq!(lifecycle.1 - lifecycle_before.1, 2);

    let lifecycle_before_exhaustion = lifecycle;
    assert!(
        list_front(&context, exhausted.clone()).is_none(),
        "the third alternative must publish the empty fixed tail"
    );
    let lifecycle_after_exhaustion = context.values().managed_promise_lifecycle_counts_for_test();
    assert_eq!(
        lifecycle_after_exhaustion.0 - lifecycle_before_exhaustion.0,
        1
    );
    assert_eq!(
        lifecycle_after_exhaustion.1 - lifecycle_before_exhaustion.1,
        1
    );
    assert!(list_front(&context, exhausted).is_none());
    assert_eq!(
        context.values().managed_promise_lifecycle_counts_for_test(),
        lifecycle_after_exhaustion,
        "the memoized exhausted tail must not allocate or publish another future"
    );
}

#[test]
fn builder_checkpoint_survives_path_and_state_dependencies_without_replay() {
    let context = isolated_context();
    let visible = crate::core::Key::atom_from_text("visible");
    let path_promise = PromisedValue::new(context.values(), "builder path dependency");
    let path_owner = path_promise.root(context.values());
    let state_promise = PromisedValue::new(context.values(), "builder state dependency");
    let state_owner = state_promise.root(context.values());
    let path_value = Value::List(List::from_values(vec![
        visible.to_value_with(context.values()),
    ]));
    let path_root = crate::runtime::RuntimeValueRoot::new(context.values(), path_value);
    let state = context.values().with_runtime_value_access(|access| {
        crate::eval::builtins::initial_state_for_test(
            &access,
            Value::Dict(Dict::new_sync().insert(visible, number(73))),
        )
    });
    let state_root = crate::runtime::RuntimeValueRoot::new(context.values(), state);

    let (path, state) = context.values().with_runtime_value_access(|access| {
        (
            Value::Promised(PromisedValue::from_root(&path_owner, &access)),
            Value::Promised(PromisedValue::from_root(&state_owner, &access)),
        )
    });
    let call = Value::builtin_call(
        context.values(),
        Builtin::InteractionNetBuilderGet,
        vec![path, state],
    );
    let (retained, machine) = retained_lazy_machine(&context, call);
    let mut route_losses = 0;

    let (machine, dependency) =
        poll_until_blocked_after_route_loss(&context, &retained, machine, &mut route_losses);
    let WorkDependency::Promise(dependency) = dependency else {
        panic!("builder path demand must publish its exact promise dependency")
    };
    assert_eq!(dependency.id(), path_promise.id(context.values()));
    let machine = resume_after_builtin_route_loss(&context, &retained, machine);
    route_losses += 1;
    crate::core::set_test_promise(
        context.values(),
        &path_promise,
        path_root.clone_core_for_test(),
    )
    .expect("the path dependency should accept its assignment");

    let (machine, dependency) =
        poll_until_blocked_after_route_loss(&context, &retained, machine, &mut route_losses);
    let WorkDependency::Promise(dependency) = dependency else {
        panic!("builder state demand must publish its exact promise dependency")
    };
    assert_eq!(dependency.id(), state_promise.id(context.values()));
    let machine = resume_after_builtin_route_loss(&context, &retained, machine);
    route_losses += 1;
    crate::core::set_test_promise(
        context.values(),
        &state_promise,
        state_root.clone_core_for_test(),
    )
    .expect("the state dependency should accept its assignment");

    let results = drive_after_route_loss(&context, &retained, machine, &mut route_losses);
    let (outcome, tail) =
        list_front(&context, results).expect("builder get must return one outcome");
    assert!(list_front(&context, tail).is_none());
    let [value, _state] = context.values().with_runtime_value_access(|access| {
        crate::eval::builtins::decode_outcome_for_test(&access, &outcome)
            .expect("builder get must retain the strict outcome schema")
    });
    assert_eq!(value, number(73));
    assert!(
        route_losses >= 3,
        "the fixture must lose routes before and after exact dependency publication"
    );
}

#[test]
fn builder_wire_checkpoint_preserves_left_to_right_operand_and_state_dependencies() {
    let context = isolated_context();
    let left_promise = PromisedValue::new(context.values(), "builder wire left dependency");
    let left_owner = left_promise.root(context.values());
    let right_promise = PromisedValue::new(context.values(), "builder wire right dependency");
    let right_owner = right_promise.root(context.values());
    let state_promise = PromisedValue::new(context.values(), "builder wire state dependency");
    let state_owner = state_promise.root(context.values());
    let (state, [left, right]) = context.values().with_runtime_value_access(|access| {
        crate::eval::builtins::construction_state_and_ports_for_test(&access)
    });
    let left_root = crate::runtime::RuntimeValueRoot::new(context.values(), left);
    let right_root = crate::runtime::RuntimeValueRoot::new(context.values(), right);
    let state_root = crate::runtime::RuntimeValueRoot::new(context.values(), state);

    let call = context.values().with_runtime_value_access(|access| {
        Value::builtin_call_in(
            &access,
            Builtin::InteractionNetBuilderWire,
            vec![
                Value::Promised(PromisedValue::from_root(&left_owner, &access)),
                Value::Promised(PromisedValue::from_root(&right_owner, &access)),
                Value::Promised(PromisedValue::from_root(&state_owner, &access)),
            ],
        )
    });
    let (retained, mut machine) = retained_lazy_machine(&context, call);
    let mut route_losses = 0;

    for (expected, promise, value) in [
        (left_promise.id(context.values()), &left_promise, &left_root),
        (
            right_promise.id(context.values()),
            &right_promise,
            &right_root,
        ),
        (
            state_promise.id(context.values()),
            &state_promise,
            &state_root,
        ),
    ] {
        let (blocked, dependency) =
            poll_until_blocked_after_route_loss(&context, &retained, machine, &mut route_losses);
        let WorkDependency::Promise(dependency) = dependency else {
            panic!("builder wire demand must publish its exact promise dependency")
        };
        assert_eq!(dependency.id(), expected);
        machine = resume_after_builtin_route_loss(&context, &retained, blocked);
        route_losses += 1;
        crate::core::set_test_promise(context.values(), promise, value.clone_core_for_test())
            .expect("the builder wire dependency should accept its assignment");
    }

    let results = drive_after_route_loss(&context, &retained, machine, &mut route_losses);
    let (outcome, tail) =
        list_front(&context, results).expect("builder wire must return one outcome");
    assert!(list_front(&context, tail).is_none());
    let [unit, state] = context.values().with_runtime_value_access(|access| {
        crate::eval::builtins::decode_outcome_for_test(&access, &outcome)
            .expect("builder wire must retain the strict outcome schema")
    });
    assert_eq!(unit, context.values().unit());
    context.values().with_runtime_value_access(|access| {
        assert_eq!(
            crate::eval::builtins::construction_journal_lengths_for_test(&access, &state)
                .expect("builder wire state must retain strict journals"),
            (0, 1)
        );
    });
    assert!(
        route_losses >= 4,
        "the fixture must lose routes around all three exact dependencies"
    );
}

#[test]
fn builder_copy_checkpoint_preserves_count_then_state_dependencies_without_replay() {
    let context = isolated_context();
    let count_promise = PromisedValue::new(context.values(), "builder copy count dependency");
    let count_owner = count_promise.root(context.values());
    let state_promise = PromisedValue::new(context.values(), "builder copy state dependency");
    let state_owner = state_promise.root(context.values());
    let state = context.values().with_runtime_value_access(|access| {
        crate::eval::builtins::initial_state_for_test(&access, Value::Dict(Dict::new_sync()))
    });
    let count_root = crate::runtime::RuntimeValueRoot::new(context.values(), number(2));
    let state_root = crate::runtime::RuntimeValueRoot::new(context.values(), state);
    let call = context.values().with_runtime_value_access(|access| {
        Value::builtin_call_in(
            &access,
            Builtin::InteractionNetBuilderCopy,
            vec![
                Value::Promised(PromisedValue::from_root(&count_owner, &access)),
                Value::Promised(PromisedValue::from_root(&state_owner, &access)),
            ],
        )
    });
    let (retained, mut machine) = retained_lazy_machine(&context, call);
    let mut route_losses = 0;

    for (expected, promise, value) in [
        (
            count_promise.id(context.values()),
            &count_promise,
            &count_root,
        ),
        (
            state_promise.id(context.values()),
            &state_promise,
            &state_root,
        ),
    ] {
        let (blocked, dependency) =
            poll_until_blocked_after_route_loss(&context, &retained, machine, &mut route_losses);
        let WorkDependency::Promise(dependency) = dependency else {
            panic!("builder copy demand must publish its exact promise dependency")
        };
        assert_eq!(dependency.id(), expected);
        machine = resume_after_builtin_route_loss(&context, &retained, blocked);
        route_losses += 1;
        crate::core::set_test_promise(context.values(), promise, value.clone_core_for_test())
            .expect("the builder copy dependency should accept its assignment");
    }

    let results = drive_after_route_loss(&context, &retained, machine, &mut route_losses);
    let (outcome, tail) =
        list_front(&context, results).expect("builder copy must return one outcome");
    assert!(list_front(&context, tail).is_none());
    let [ports, state] = context.values().with_runtime_value_access(|access| {
        crate::eval::builtins::decode_outcome_for_test(&access, &outcome)
            .expect("builder copy must retain the strict outcome schema")
    });
    let mut remaining_ports = ports;
    for _ in 0..3 {
        let (_port, tail) = list_front(&context, remaining_ports)
            .expect("builder copy must return all requested ports");
        remaining_ports = tail;
    }
    assert!(
        list_front(&context, remaining_ports).is_none(),
        "builder copy must return exactly input plus two output ports"
    );
    context.values().with_runtime_value_access(|access| {
        assert_eq!(
            crate::eval::builtins::construction_journal_lengths_for_test(&access, &state)
                .expect("builder copy state must retain strict journals"),
            (1, 0)
        );
    });
    assert!(
        route_losses >= 3,
        "the fixture must lose routes around both exact dependencies"
    );
}

#[test]
fn builder_operand_failures_are_ordered_transparent_and_do_not_observe_later_operands() {
    let context = isolated_context();

    {
        let count_demands = Arc::new(AtomicUsize::new(0));
        let state_demands = Arc::new(AtomicUsize::new(0));
        let state = context.values().with_runtime_value_access(|access| {
            crate::eval::builtins::initial_state_for_test(&access, Value::Dict(Dict::new_sync()))
        });
        let call = Value::builtin_call(
            context.values(),
            Builtin::InteractionNetBuilderCopy,
            vec![
                counted_failure(
                    &context,
                    "failed builder copy count",
                    &count_demands,
                    "copy count failure",
                ),
                counted_success(&context, "later builder copy state", &state_demands, state),
            ],
        );
        let failure = builder_failure_across_route_loss(&context, call);
        assert_transparent_failure(&context, &failure, "copy count failure");
        assert_eq!(count_demands.load(Ordering::SeqCst), 1);
        assert_eq!(state_demands.load(Ordering::SeqCst), 0);
    }

    for failed_position in 0..3 {
        let left_demands = Arc::new(AtomicUsize::new(0));
        let right_demands = Arc::new(AtomicUsize::new(0));
        let state_demands = Arc::new(AtomicUsize::new(0));
        let (state, [left, right]) = context.values().with_runtime_value_access(|access| {
            crate::eval::builtins::construction_state_and_ports_for_test(&access)
        });
        let counts = [&left_demands, &right_demands, &state_demands];
        let labels = [
            "builder wire left",
            "builder wire right",
            "builder wire state",
        ];
        let values = [left, right, state];
        let arguments = values
            .into_iter()
            .enumerate()
            .map(|(position, value)| {
                if position == failed_position {
                    counted_failure(
                        &context,
                        labels[position],
                        counts[position],
                        "wire operand failure",
                    )
                } else {
                    counted_success(&context, labels[position], counts[position], value)
                }
            })
            .collect();
        let call = Value::builtin_call(
            context.values(),
            Builtin::InteractionNetBuilderWire,
            arguments,
        );
        let failure = builder_failure_across_route_loss(&context, call);
        assert_transparent_failure(&context, &failure, "wire operand failure");
        let observed = [
            left_demands.load(Ordering::SeqCst),
            right_demands.load(Ordering::SeqCst),
            state_demands.load(Ordering::SeqCst),
        ];
        assert_eq!(
            observed,
            std::array::from_fn(|position| usize::from(position <= failed_position)),
            "wire operands must be observed left-to-right only through the failure boundary"
        );
    }
}

#[test]
fn builder_control_key_failures_are_transparent_and_precede_later_operands() {
    let context = isolated_context();
    for builtin in [
        Builtin::InteractionNetBuilderReset,
        Builtin::InteractionNetBuilderShift,
    ] {
        let key_demands = Arc::new(AtomicUsize::new(0));
        let operation_demands = Arc::new(AtomicUsize::new(0));
        let state_demands = Arc::new(AtomicUsize::new(0));
        let state = context.values().with_runtime_value_access(|access| {
            crate::eval::builtins::initial_state_for_test(&access, Value::Dict(Dict::new_sync()))
        });
        let call = Value::builtin_call(
            context.values(),
            builtin,
            vec![
                counted_failure(
                    &context,
                    "failed builder control key",
                    &key_demands,
                    "control key failure",
                ),
                counted_success(
                    &context,
                    "later builder control operation",
                    &operation_demands,
                    context.values().unit(),
                ),
                counted_success(
                    &context,
                    "later builder control state",
                    &state_demands,
                    state,
                ),
            ],
        );
        let failure = builder_failure_across_route_loss(&context, call);
        assert_transparent_failure(&context, &failure, "control key failure");
        assert_eq!(key_demands.load(Ordering::SeqCst), 1);
        assert_eq!(operation_demands.load(Ordering::SeqCst), 0);
        assert_eq!(state_demands.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn builder_checkpoint_observes_a_lazy_reset_key_once_across_route_loss() {
    let context = isolated_context();
    let key_demands = Arc::new(AtomicUsize::new(0));
    let observed_key_demands = Arc::clone(&key_demands);
    let key = Value::Lazy(LazyValue::semantic_thunk(
        context.values(),
        "counted builder reset key",
        move |_| {
            observed_key_demands.fetch_add(1, Ordering::SeqCst);
            Ok(Value::binary_from_text("route-loss prompt"))
        },
    ));
    let state = context.values().with_runtime_value_access(|access| {
        crate::eval::builtins::initial_state_for_test(&access, Value::Dict(Dict::new_sync()))
    });
    let operation = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::InteractionNetBuilderReturn,
        arguments: Arc::from([number(74)]),
    });
    let operation = builder_effect_value(&context, operation);
    let call = Value::builtin_call(
        context.values(),
        Builtin::InteractionNetBuilderReset,
        vec![key, operation, state],
    );
    let (retained, machine) = retained_lazy_machine(&context, call);
    let mut route_losses = 0;
    let results = drive_after_route_loss(&context, &retained, machine, &mut route_losses);
    let (outcome, tail) =
        list_front(&context, results).expect("builder reset must return one outcome");
    assert!(list_front(&context, tail).is_none());
    let [value, _state] = context.values().with_runtime_value_access(|access| {
        crate::eval::builtins::decode_outcome_for_test(&access, &outcome)
            .expect("builder reset must retain the strict outcome schema")
    });
    assert_eq!(value, number(74));
    assert_eq!(key_demands.load(Ordering::SeqCst), 1);
    assert!(
        route_losses > 0,
        "the lazy reset-key observation must cross a forced route loss"
    );
}

#[test]
fn later_builder_fix_alternative_survives_route_loss_without_replay() {
    let context = isolated_context();
    let state = context.values().with_runtime_value_access(|access| {
        crate::eval::builtins::initial_state_for_test(&access, Value::Dict(Dict::new_sync()))
    });
    let function_demands = Arc::new(AtomicUsize::new(0));
    let continuation_demands = Arc::new(AtomicUsize::new(0));
    let (operation, state) = context.values().with_runtime_value_access(|access| {
        let returned = |value| {
            Value::PartialBuiltin(BuiltinCall {
                builtin: Builtin::InteractionNetBuilderReturn,
                arguments: Arc::from([number(value)]),
            })
        };
        let choices = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::InteractionNetBuilderAlt,
            arguments: Arc::from([
                builder_effect_value(&context, returned(81)),
                builder_effect_value(&context, returned(82)),
            ]),
        });
        let function = crate::eval::test_support::closed_function_value_in(
            context.values(),
            1,
            crate::eval::test_support::TestExpr::Value(builder_effect_value(&context, choices)),
        );
        let function = crate::runtime::RuntimeValueRoot::new(context.values(), function);
        let observed_function = Arc::clone(&function_demands);
        let function = Value::Lazy(LazyValue::semantic_thunk(
            context.values(),
            "counted builder fix function",
            move |_| {
                observed_function.fetch_add(1, Ordering::SeqCst);
                Ok(function.clone_core_for_test())
            },
        ));
        let fixed = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::InteractionNetBuilderFix,
            arguments: Arc::from([function]),
        });

        let continuation = crate::eval::test_support::closed_function_value_in(
            context.values(),
            1,
            crate::eval::test_support::TestExpr::Apply(
                Arc::new(crate::eval::test_support::TestExpr::Value(
                    Value::PartialBuiltin(BuiltinCall {
                        builtin: Builtin::DictSingleton,
                        arguments: Arc::from([context.values().key_value(&crate::core::keys::EFF)]),
                    }),
                )),
                Arc::new(crate::eval::test_support::TestExpr::Apply(
                    Arc::new(crate::eval::test_support::TestExpr::Value(
                        Value::PartialBuiltin(BuiltinCall {
                            builtin: Builtin::EffectCall,
                            arguments: Arc::from([context
                                .values()
                                .key_value(&crate::core::Key::atom_from_text("r"))]),
                        }),
                    )),
                    Arc::new(crate::eval::test_support::TestExpr::List(Arc::from([
                        Arc::new(crate::eval::test_support::TestExpr::Local(0)),
                    ]))),
                )),
            ),
        );
        let continuation = crate::runtime::RuntimeValueRoot::new(context.values(), continuation);
        let observed_continuation = Arc::clone(&continuation_demands);
        let continuation = Value::Lazy(LazyValue::semantic_thunk(
            context.values(),
            "counted builder fix continuation",
            move |_| {
                observed_continuation.fetch_add(1, Ordering::SeqCst);
                Ok(continuation.clone_core_for_test())
            },
        ));
        let operation = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::InteractionNetBuilderSeq,
            arguments: Arc::from([builder_effect_value(&context, fixed), continuation]),
        });
        (operation, access.duplicate_value(&state))
    });
    let results = Value::Lazy(LazyValue::from_application(
        context.values(),
        operation,
        Arc::from([state]),
    ));
    let lifecycle_before = context.values().managed_promise_lifecycle_counts_for_test();
    let (retained, machine) = retained_application_machine(
        &context,
        Value::Builtin(Builtin::ListAt),
        Arc::from([number(1), results]),
    );
    let mut route_losses = 0;
    let outcome = drive_after_route_loss(&context, &retained, machine, &mut route_losses);
    let [value, _state] = context.values().with_runtime_value_access(|access| {
        crate::eval::builtins::decode_outcome_for_test(&access, &outcome)
            .expect("the selected builder fix alternative must use the outcome schema")
    });
    let value = crate::eval::eval_value(&context, &value)
        .expect("the selected builder continuation value should evaluate");
    assert_eq!(value, number(82));
    assert_eq!(function_demands.load(Ordering::SeqCst), 1);
    assert_eq!(continuation_demands.load(Ordering::SeqCst), 1);
    let lifecycle_after = context.values().managed_promise_lifecycle_counts_for_test();
    assert_eq!(lifecycle_after.0 - lifecycle_before.0, 2);
    assert_eq!(lifecycle_after.1 - lifecycle_before.1, 2);
    assert!(
        route_losses > 0,
        "the later builder-fix alternative must survive at least one forced route loss"
    );
}

#[test]
fn public_pure_construction_survives_route_loss_without_repeating_effect_or_continuation() {
    let context = isolated_context();
    let effect_demands = Arc::new(AtomicUsize::new(0));
    let continuation_demands = Arc::new(AtomicUsize::new(0));
    let effect = context.values().with_runtime_value_access(|access| {
        let continuation = builder_return_first_port_continuation(&context);
        let continuation = crate::runtime::RuntimeValueRoot::new(context.values(), continuation);
        let observed_continuation = Arc::clone(&continuation_demands);
        let continuation = Value::Lazy(LazyValue::semantic_thunk(
            context.values(),
            "counted construction continuation",
            move |_| {
                observed_continuation.fetch_add(1, Ordering::SeqCst);
                Ok(continuation.clone_core_for_test())
            },
        ));
        let data = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::InteractionNetBuilderData,
            arguments: Arc::from([Value::binary_from_text("route-loss data")]),
        });
        let sequence = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::InteractionNetBuilderSeq,
            arguments: Arc::from([builder_effect_value(&context, data), continuation]),
        });
        let effect = builder_effect_value(&context, sequence);
        let effect = crate::runtime::RuntimeValueRoot::new(context.values(), effect);
        let observed_effect = Arc::clone(&effect_demands);
        let effect = Value::Lazy(LazyValue::semantic_thunk(
            context.values(),
            "counted construction effect",
            move |_| {
                observed_effect.fetch_add(1, Ordering::SeqCst);
                Ok(effect.clone_core_for_test())
            },
        ));
        Value::builtin_call_in(&access, Builtin::InteractionNet, vec![effect])
    });
    let (retained, machine) = retained_lazy_machine(&context, effect);
    let mut route_losses = 0;
    let value = drive_after_route_loss(&context, &retained, machine, &mut route_losses);
    let Value::Net(first_net) = value else {
        panic!("construction must complete with a net")
    };
    // The route which performed synchronous replay is gone. Collection and a
    // fresh demand must observe the same cached net, not rerun construction.
    collect_between_handoffs(&context);
    let repeated = context
        .values()
        .with_runtime_value_access(|access| Value::Lazy(LazyValue::from_root(&retained, &access)));
    let Value::Net(second_net) = crate::eval::eval_value(&context, &repeated)
        .expect("a second demand should observe the completed construction")
    else {
        panic!("memoized construction must remain a net")
    };
    assert!(first_net.runtime().ptr_eq(second_net.runtime()));
    assert!(route_losses > 0);
    assert_eq!(effect_demands.load(Ordering::SeqCst), 1);
    assert_eq!(continuation_demands.load(Ordering::SeqCst), 1);
}

#[test]
fn public_pure_construction_retains_both_selector_observations_across_route_loss() {
    let context = isolated_context();
    let first_demands = Arc::new(AtomicUsize::new(0));
    let second_demands = Arc::new(AtomicUsize::new(0));
    let observed_first = Arc::clone(&first_demands);
    let first = LazyValue::semantic_thunk(
        context.values(),
        "counted first construction result",
        move |_| {
            observed_first.fetch_add(1, Ordering::SeqCst);
            Ok(Value::List(List::from_values(vec![number(1)])))
        },
    );
    let observed_second = Arc::clone(&second_demands);
    let second = LazyValue::semantic_thunk(
        context.values(),
        "counted second construction result",
        move |_| {
            observed_second.fetch_add(1, Ordering::SeqCst);
            Ok(Value::List(List::from_values(vec![number(2)])))
        },
    );
    let results = Value::List(List::concat(
        List::from_thunk(first.into()),
        List::from_thunk(second.into()),
    ));
    let handler = crate::eval::test_support::closed_function_value_in(
        context.values(),
        2,
        crate::eval::test_support::TestExpr::Value(results),
    );
    let effect = list_effect_value(handler);
    let call = Value::builtin_call(context.values(), Builtin::InteractionNet, vec![effect]);
    let (retained, machine) = retained_lazy_machine(&context, call);
    let mut route_losses = 0;
    let failure = drive_failure_after_route_loss(&context, &retained, machine, &mut route_losses);
    assert!(failure.to_string().contains("produced multiple results"));
    assert_eq!(first_demands.load(Ordering::SeqCst), 1);
    assert_eq!(second_demands.load(Ordering::SeqCst), 1);
    assert!(route_losses >= 2);
    let frame = crate::diagnostic::evaluation_context_frame("net_construction");
    assert_eq!(
        failure
            .contexts()
            .iter()
            .filter(|context| *context == &frame)
            .count(),
        1
    );
}

#[test]
fn public_pure_construction_retains_exposed_port_demand_across_route_loss() {
    let context = isolated_context();
    let port_demands = Arc::new(AtomicUsize::new(0));
    let observed_port = Arc::clone(&port_demands);
    let port = Value::Lazy(LazyValue::semantic_thunk(
        context.values(),
        "counted exposed construction port",
        move |_| {
            observed_port.fetch_add(1, Ordering::SeqCst);
            Err(EvaluationHalt::new("exposed port failed"))
        },
    ));
    let operation = Value::PartialBuiltin(BuiltinCall {
        builtin: Builtin::InteractionNetBuilderReturn,
        arguments: Arc::from([port]),
    });
    let effect = builder_effect_value(&context, operation);
    let call = Value::builtin_call(context.values(), Builtin::InteractionNet, vec![effect]);
    let (retained, machine) = retained_lazy_machine(&context, call);
    let mut route_losses = 0;
    let failure = drive_failure_after_route_loss(&context, &retained, machine, &mut route_losses);
    assert!(failure.to_string().contains("exposed port failed"));
    assert_eq!(port_demands.load(Ordering::SeqCst), 1);
    assert!(route_losses > 0);
    let frame = crate::diagnostic::evaluation_context_frame("net_construction");
    assert_eq!(
        failure
            .contexts()
            .iter()
            .filter(|context| *context == &frame)
            .count(),
        1
    );
}

#[test]
fn public_construction_waits_for_first_result_before_ready_right_branch() {
    let context = isolated_context();
    let first = PromisedValue::new(context.values(), "first construction result promise");
    let _first_owner = first.root(context.values());
    let right_demands = Arc::new(AtomicUsize::new(0));
    let right = counted_success(
        &context,
        "ready right construction result",
        &right_demands,
        Value::List(List::from_values(vec![number(2)])),
    );
    let Value::Lazy(right) = right else {
        unreachable!("counted right result must be lazy")
    };
    let results = List::concat(
        List::from_thunk(ListThunk::Promised(first.clone())),
        List::from_thunk(right.into()),
    );
    let effect = construction_results_effect(&context, results);
    let call = Value::builtin_call(context.values(), Builtin::InteractionNet, vec![effect]);
    let (retained, machine) = retained_lazy_machine(&context, call);
    let mut route_losses = 0;

    let (machine, dependency) =
        poll_until_blocked_after_route_loss(&context, &retained, machine, &mut route_losses);
    assert_construction_promise_dependency(&context, dependency, &first);
    assert_eq!(
        right_demands.load(Ordering::SeqCst),
        0,
        "a ready right branch cannot overtake a blocked first result"
    );
    let machine = resume_after_builtin_route_loss(&context, &retained, machine);
    route_losses += 1;
    crate::core::set_test_promise(
        context.values(),
        &first,
        Value::List(List::from_values(vec![number(1)])),
    )
    .expect("the first construction result should accept its assignment");

    let failure = drive_failure_after_route_loss(&context, &retained, machine, &mut route_losses);
    assert!(failure.to_string().contains("produced multiple results"));
    assert_eq!(right_demands.load(Ordering::SeqCst), 1);
    assert!(route_losses >= 2);
}

#[test]
fn public_construction_retains_first_result_while_second_promise_blocks() {
    let context = isolated_context();
    let second = PromisedValue::new(context.values(), "second construction result promise");
    let _second_owner = second.root(context.values());
    let first_demands = Arc::new(AtomicUsize::new(0));
    let first = counted_success(
        &context,
        "counted first construction result before second promise",
        &first_demands,
        Value::List(List::from_values(vec![number(1)])),
    );
    let Value::Lazy(first) = first else {
        unreachable!("counted first result must be lazy")
    };
    let results = List::concat(
        List::from_thunk(first.into()),
        List::from_thunk(ListThunk::Promised(second.clone())),
    );
    let effect = construction_results_effect(&context, results);
    let call = Value::builtin_call(context.values(), Builtin::InteractionNet, vec![effect]);
    let (retained, machine) = retained_lazy_machine(&context, call);
    let mut route_losses = 0;

    let (machine, dependency) =
        poll_until_blocked_after_route_loss(&context, &retained, machine, &mut route_losses);
    assert_construction_promise_dependency(&context, dependency, &second);
    assert_eq!(first_demands.load(Ordering::SeqCst), 1);
    let machine = resume_after_builtin_route_loss(&context, &retained, machine);
    route_losses += 1;
    crate::core::set_test_promise(
        context.values(),
        &second,
        Value::List(List::from_values(vec![number(2)])),
    )
    .expect("the second construction result should accept its assignment");

    let failure = drive_failure_after_route_loss(&context, &retained, machine, &mut route_losses);
    assert!(failure.to_string().contains("produced multiple results"));
    assert_eq!(
        first_demands.load(Ordering::SeqCst),
        1,
        "the first result must not be recomputed after the second wait"
    );
    assert!(route_losses >= 2);
}

#[test]
fn public_construction_exposed_port_promise_retains_one_context_after_route_loss() {
    let context = isolated_context();
    let exposed = PromisedValue::new(context.values(), "exposed construction port promise");
    let exposed_owner = exposed.root(context.values());
    let effect = context.values().with_runtime_value_access(|access| {
        let return_port = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::InteractionNetBuilderReturn,
            arguments: Arc::from([Value::Promised(PromisedValue::from_root(
                &exposed_owner,
                &access,
            ))]),
        });
        builder_effect_value(&context, return_port)
    });
    let call = Value::builtin_call(context.values(), Builtin::InteractionNet, vec![effect]);
    let (retained, machine) = retained_lazy_machine(&context, call);
    let mut route_losses = 0;

    let (machine, dependency) =
        poll_until_blocked_after_route_loss(&context, &retained, machine, &mut route_losses);
    assert_construction_promise_dependency(&context, dependency, &exposed);
    let machine = resume_after_builtin_route_loss(&context, &retained, machine);
    route_losses += 1;
    crate::core::set_test_promise(context.values(), &exposed, number(42))
        .expect("the exposed-port promise should accept its assignment");

    let failure = drive_failure_after_route_loss(&context, &retained, machine, &mut route_losses);
    assert!(failure.to_string().contains("requires a construction port"));
    let frame = crate::diagnostic::evaluation_context_frame("net_construction");
    assert_eq!(
        failure
            .contexts()
            .iter()
            .filter(|context| *context == &frame)
            .count(),
        1
    );
    assert!(route_losses >= 2);
}

#[test]
fn public_construction_builder_operand_promise_resumes_same_program() {
    let context = isolated_context();
    let count = PromisedValue::new(context.values(), "construction copy count promise");
    let count_owner = count.root(context.values());
    let effect = context.values().with_runtime_value_access(|access| {
        let copy = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::InteractionNetBuilderCopy,
            arguments: Arc::from([Value::Promised(PromisedValue::from_root(
                &count_owner,
                &access,
            ))]),
        });
        let sequence = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::InteractionNetBuilderSeq,
            arguments: Arc::from([
                builder_effect_value(&context, copy),
                builder_return_first_port_continuation(&context),
            ]),
        });
        builder_effect_value(&context, sequence)
    });
    let call = Value::builtin_call(context.values(), Builtin::InteractionNet, vec![effect]);
    let (retained, machine) = retained_lazy_machine(&context, call);
    let mut route_losses = 0;

    let (machine, wait) = poll_until_stalled_child_wait_after_route_loss(
        &context,
        &retained,
        machine,
        &mut route_losses,
    );
    assert!(
        wait.terminal_poll().is_none(),
        "the child wait must remain pending on the unassigned copy count"
    );
    let machine = resume_after_builtin_route_loss(&context, &retained, machine);
    route_losses += 1;
    crate::core::set_test_promise(context.values(), &count, number(0))
        .expect("the copy-count promise should accept its assignment");
    pump_to_ready(&context, &wait);

    let net = drive_after_route_loss(&context, &retained, machine, &mut route_losses);
    assert!(matches!(net, Value::Net(_)));
    assert!(route_losses >= 2);
}

#[test]
fn public_construction_checkpoint_cycle_is_reclaimed_after_roots_drop() {
    let context = isolated_context();
    let values = context.values();
    let count = PromisedValue::new(values, "construction checkpoint cycle count");
    let count_owner = count.root(values);
    let effect = values.with_runtime_value_access(|access| {
        let copy = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::InteractionNetBuilderCopy,
            arguments: Arc::from([Value::Promised(PromisedValue::from_root(
                &count_owner,
                &access,
            ))]),
        });
        let sequence = Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::InteractionNetBuilderSeq,
            arguments: Arc::from([
                builder_effect_value(&context, copy),
                builder_return_first_port_continuation(&context),
            ]),
        });
        builder_effect_value(&context, sequence)
    });
    let call = Value::builtin_call(values, Builtin::InteractionNet, vec![effect]);
    let (retained, mut machine) = retained_lazy_machine(&context, call);
    let poll = crate::evaluation::EvaluationPollContext::for_context(&context);
    assert!(matches!(
        machine.poll(&poll, &mut crate::evaluation::EvaluationStepBudget::new(1)),
        EvaluationMachinePoll::Yielded
    ));
    assert_lazy_checkpoint_kind(&context, &machine, ManagedLazyCheckpointKindTag::Builtin);

    let backedge = values
        .with_runtime_value_access(|access| Value::Lazy(LazyValue::from_root(&retained, &access)));
    crate::core::set_test_promise(values, &count, backedge)
        .expect("the count promise should accept a backedge to its construction");
    let live = values
        .collect_managed_for_test()
        .expect("the rooted construction checkpoint cycle should be collectible");

    drop((machine, retained, count, count_owner));
    let dead = values
        .collect_managed_for_test()
        .expect("the unrooted construction checkpoint cycle should be collectible");
    // The machine, retained lazy handle, and promise owner are distinct roots.
    assert_eq!(dead.root_entries() + 3, live.root_entries());
    assert!(
        dead.marked_slots() + 2 <= live.marked_slots(),
        "the lazy checkpoint and promise cycle must become unreachable"
    );
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
