use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::core::{DeferredValueId, EvaluatedValue, LazyValue, PromisedValue, Value};
use crate::core_net::CoreWaitToken;
use crate::evaluation::{
    EvalContext, EvaluationPumpOutcome, EvaluationTaskCancellation, EvaluationWaitPoll,
    OwnedEvalContext,
};

fn reduce_checkpoint(values: &CoreValueFactory, runtime: &CoreRuntimeNet, pair: ActivePairKey) {
    let reduction = runtime
        .test_with_optional_mut(values, |net| net.reduce_pair(pair))
        .expect("the ready callable checkpoint must be claimable");
    assert!(matches!(
        reduction.kind,
        ReductionKind::CallableCheckpoint { .. }
    ));
}

fn progress_checkpoint(
    context: &EvalContext,
    runtime: &CoreRuntimeNet,
    pair: ActivePairKey,
    budget: usize,
) -> Result<(), EvaluationHalt> {
    super::super::with_direct_evaluator(context, |evaluator| {
        let mut budget = crate::evaluation::EvaluationStepBudget::new(budget);
        progress_callable_checkpoint(evaluator, runtime, pair, &mut budget)
    })
}

fn normalization_request_in(
    context: &EvalContext,
    runtime: &CoreRuntimeNet,
    interface: Port,
) -> NormalizationRequest {
    super::super::with_direct_evaluator(context, |evaluator| {
        NormalizationRequest::cursor_whnf(runtime, interface, evaluator)
    })
}

fn resume_blocked_checkpoint(
    context: &EvalContext,
    runtime: &CoreRuntimeNet,
    blocked: &crate::interaction_net::BlockedCallableCheckpoint<CoreWaitToken>,
) -> Result<(), EvaluationHalt> {
    super::super::with_direct_evaluator(context, |evaluator| {
        assert!(with_core_net_access(evaluator, runtime, |runtime| {
            runtime.retry_blocked_callable_checkpoint(blocked)
        }));
    });
    reduce_checkpoint(context.values(), runtime, blocked.call.pair);
    progress_checkpoint(context, runtime, blocked.call.pair, usize::MAX)
}

fn blocked_checkpoint(
    values: &CoreValueFactory,
    runtime: &CoreRuntimeNet,
    pair: ActivePairKey,
) -> crate::interaction_net::BlockedCallableCheckpoint<CoreWaitToken> {
    runtime
        .test_with(values, |net| net.blocked_callable_checkpoint(pair))
        .expect("the callable checkpoint must retain its exact dependency")
}

#[test]
fn callable_checkpoint_covers_lazy_and_mixed_dependency_chains_once() {
    let context = test_context();
    let terminal = Value::Builtin(Builtin::Add);

    let inner_runs = Arc::new(AtomicUsize::new(0));
    let observed_inner = Arc::clone(&inner_runs);
    let inner = LazyValue::semantic_thunk(context.values(), "NC5 inner lazy", move |_| {
        observed_inner.fetch_add(1, Ordering::SeqCst);
        Ok(terminal.clone())
    });
    let inner_id = inner.id(context.values());
    let outer_runs = Arc::new(AtomicUsize::new(0));
    let observed_outer = Arc::clone(&outer_runs);
    let outer_inner = Value::Lazy(inner);
    let outer = LazyValue::semantic_thunk(context.values(), "NC5 outer lazy", move |_| {
        observed_outer.fetch_add(1, Ordering::SeqCst);
        Ok(outer_inner.clone())
    });
    let outer_id = outer.id(context.values());
    let (runtime, call) = claimed_core_call_in(context.values(), Value::Lazy(outer));

    super::super::with_direct_evaluator(&context, |evaluator| {
        let mut budget = crate::evaluation::EvaluationStepBudget::new(usize::MAX);
        assert!(progress_exact_core_call_in(
            evaluator,
            &runtime,
            call,
            &mut budget,
        )?);
        Ok::<_, EvaluationHalt>(())
    })
    .unwrap();
    let blocked = blocked_checkpoint(context.values(), &runtime, call.pair);
    let observation = checkpoint_semantic_observation(&runtime, context.values(), call.pair);
    assert_eq!(observation.focus, Some(DeferredValueId::Lazy(outer_id)));
    assert_eq!(context.deferred_task_count(), 1);
    assert!(matches!(
        context.pump_wait(&blocked.wait.0, 256),
        EvaluationPumpOutcome::TargetReady
    ));
    assert_eq!(outer_runs.load(Ordering::SeqCst), 1);
    assert_eq!(inner_runs.load(Ordering::SeqCst), 1);
    assert!(observation.followed.is_empty());
    assert_ne!(outer_id, inner_id);

    resume_blocked_checkpoint(&context, &runtime, &blocked).unwrap();
    assert_eq!(
        observe_current_callable_path(&runtime, call),
        CurrentCallablePath::DirectOperator
    );

    let promise = PromisedValue::new(context.values(), "NC5 mixed promise");
    let promise_id = promise.id(context.values());
    let mixed_runs = Arc::new(AtomicUsize::new(0));
    let observed_mixed = Arc::clone(&mixed_runs);
    let promised = Value::Promised(promise.clone());
    let mixed = LazyValue::semantic_thunk(context.values(), "NC5 mixed lazy", move |_| {
        observed_mixed.fetch_add(1, Ordering::SeqCst);
        Ok(promised.clone())
    });
    let mixed_id = mixed.id(context.values());
    let (runtime, call) = claimed_core_call_in(context.values(), Value::Lazy(mixed));
    assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
    let blocked = blocked_checkpoint(context.values(), &runtime, call.pair);
    assert_eq!(
        checkpoint_semantic_observation(&runtime, context.values(), call.pair).focus,
        Some(DeferredValueId::Lazy(mixed_id))
    );
    assert!(matches!(
        context.pump_wait(&blocked.wait.0, 256),
        EvaluationPumpOutcome::NoProgress
    ));
    assert_eq!(mixed_runs.load(Ordering::SeqCst), 1);
    crate::core::set_test_promise(context.values(), &promise, Value::Builtin(Builtin::Add))
        .expect("the mixed dependency accepts its callable result");
    assert!(matches!(
        context.pump_wait(&blocked.wait.0, 256),
        EvaluationPumpOutcome::TargetReady
    ));
    resume_blocked_checkpoint(&context, &runtime, &blocked).unwrap();
    assert_eq!(
        observe_current_callable_path(&runtime, call),
        CurrentCallablePath::DirectOperator
    );
    assert_eq!(mixed_runs.load(Ordering::SeqCst), 1);
    assert_ne!(
        DeferredValueId::Lazy(mixed_id),
        DeferredValueId::Promise(promise_id)
    );
}

#[test]
fn callable_checkpoint_covers_promise_spills_cycles_and_terminal_failures() {
    let context = test_context();
    let terminal = PromisedValue::new(context.values(), "NC5 terminal promise");
    crate::core::set_test_promise(context.values(), &terminal, Value::Builtin(Builtin::Add))
        .expect("terminal promise accepts its callable result");
    let terminal_id = terminal.id(context.values());
    let first = PromisedValue::new(context.values(), "NC5 first promise");
    let terminal_value = context
        .values()
        .with_runtime_value_access(|access| Value::Promised(terminal.duplicate_in(&access)));
    crate::core::set_test_promise(context.values(), &first, terminal_value)
        .expect("first promise delegates to terminal promise");
    let first_id = first.id(context.values());
    let (runtime, call) = claimed_core_call_in(context.values(), Value::Promised(first));

    super::super::with_direct_evaluator(&context, |evaluator| {
        let mut budget = crate::evaluation::EvaluationStepBudget::new(1);
        progress_exact_core_call_in(evaluator, &runtime, call, &mut budget)
    })
    .unwrap();
    let observation = checkpoint_semantic_observation(&runtime, context.values(), call.pair);
    assert_eq!(
        observation.focus,
        Some(DeferredValueId::Promise(terminal_id))
    );
    assert_eq!(
        observation.followed,
        [DeferredValueId::Promise(first_id)].into_iter().collect()
    );
    assert_eq!(observation.cycle_promise, Some(first_id));
    reduce_checkpoint(context.values(), &runtime, call.pair);
    progress_checkpoint(&context, &runtime, call.pair, usize::MAX).unwrap();
    assert_eq!(
        observe_current_callable_path(&runtime, call),
        CurrentCallablePath::DirectOperator
    );

    let cycle = PromisedValue::new(context.values(), "NC5 repeated promise");
    let cycle_id = cycle.id(context.values());
    let cycle_value = context
        .values()
        .with_runtime_value_access(|access| Value::Promised(cycle.duplicate_in(&access)));
    crate::core::set_test_promise(context.values(), &cycle, cycle_value)
        .expect("promise accepts its own semantic identity");
    let (runtime, call) = claimed_core_call_in(context.values(), Value::Promised(cycle));
    assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
    let blocked = blocked_checkpoint(context.values(), &runtime, call.pair);
    let observation = checkpoint_semantic_observation(&runtime, context.values(), call.pair);
    assert_eq!(observation.focus, Some(DeferredValueId::Promise(cycle_id)));
    assert_eq!(observation.cycle_promise, Some(cycle_id));
    assert_eq!(
        observation.followed,
        [DeferredValueId::Promise(cycle_id)].into_iter().collect()
    );
    assert!(matches!(
        context.pump_wait(&blocked.wait.0, 256),
        EvaluationPumpOutcome::NoProgress
    ));
    assert!(matches!(
        context.poll_wait(&blocked.wait.0),
        EvaluationWaitPoll::Pending(_)
    ));
    assert_eq!(
        context.deferred_task_count(),
        1,
        "a repeated promise remains one externally resolvable follower rather than being reconstructed"
    );

    let failed_lazy = LazyValue::error(context.values(), "NC5 cached lazy failure");
    let leading = PromisedValue::new(context.values(), "NC5 failure leader");
    crate::core::set_test_promise(context.values(), &leading, Value::Lazy(failed_lazy))
        .expect("leader accepts failed lazy focus");
    let (runtime, call) = claimed_core_call_in(context.values(), Value::Promised(leading));
    super::super::with_direct_evaluator(&context, |evaluator| {
        let mut budget = crate::evaluation::EvaluationStepBudget::new(1);
        progress_exact_core_call_in(evaluator, &runtime, call, &mut budget)
    })
    .unwrap();
    reduce_checkpoint(context.values(), &runtime, call.pair);
    let error = progress_checkpoint(&context, &runtime, call.pair, usize::MAX)
        .expect_err("cached lazy failure must terminalize the checkpoint");
    assert!(error.to_string().contains("NC5 cached lazy failure"));

    let failed_promise = PromisedValue::new(context.values(), "NC5 assigned failure");
    crate::core::fail_test_promise_message(
        context.values(),
        &failed_promise,
        "NC5 assigned promise failure",
    )
    .expect("promise accepts its failure");
    let leading =
        LazyValue::semantic_thunk(context.values(), "NC5 promise failure leader", move |_| {
            Ok(Value::Promised(failed_promise.clone()))
        });
    let (runtime, call) = claimed_core_call_in(context.values(), Value::Lazy(leading));
    assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
    let blocked = blocked_checkpoint(context.values(), &runtime, call.pair);
    assert!(matches!(
        context.pump_wait(&blocked.wait.0, 256),
        EvaluationPumpOutcome::TargetReady
    ));
    let error = resume_blocked_checkpoint(&context, &runtime, &blocked)
        .expect_err("assigned promise failure must terminalize the checkpoint");
    assert!(error.to_string().contains("NC5 assigned promise failure"));
}

struct SameRuntimeFixture {
    _assembler: crate::api::Assembler,
    runtime: crate::api::EvaluationRuntime,
}

impl SameRuntimeFixture {
    fn new() -> Self {
        let runtime = crate::api::EvaluationRuntime::new(0).expect("test runtime should build");
        let assembler = crate::api::Assembler::builder()
            .evaluation_runtime(runtime.clone())
            .build()
            .expect("test assembler should seal the runtime reflection profile");
        Self {
            _assembler: assembler,
            runtime,
        }
    }

    fn context(&self) -> OwnedEvalContext {
        OwnedEvalContext::new(
            self.runtime
                .new_evaluation_session()
                .expect("same-runtime test session should build"),
        )
    }
}

fn block_task_promise(
    observer: &EvalContext,
    promise: PromisedValue,
) -> (
    CoreRuntimeNet,
    Call,
    crate::interaction_net::BlockedCallableCheckpoint<CoreWaitToken>,
) {
    let (runtime, call) = claimed_core_call_in(observer.values(), Value::Promised(promise));
    assert!(progress_exact_core_call(observer, &runtime, call).unwrap());
    let blocked = blocked_checkpoint(observer.values(), &runtime, call.pair);
    (runtime, call, blocked)
}

fn assert_task_terminal_fails_checkpoint(
    observer: &EvalContext,
    runtime: &CoreRuntimeNet,
    call: Call,
    blocked: &crate::interaction_net::BlockedCallableCheckpoint<CoreWaitToken>,
    expected: &str,
) {
    assert!(matches!(
        observer.pump_wait(&blocked.wait.0, 256),
        EvaluationPumpOutcome::TargetReady
    ));
    let interface = runtime.test_with(observer.values(), RuntimeNet::exposed);
    let error = normalization_request_in(observer, runtime, interface)
        .drive(observer)
        .expect_err("task terminal must fail its exact callable checkpoint");
    assert!(error.to_string().contains(expected), "{error}");
    assert_eq!(
        observe_current_callable_path_in(runtime, observer.values(), call),
        CurrentCallablePath::Failed
    );
}

#[test]
fn callable_checkpoint_propagates_task_cancellation_abandonment_and_failure() {
    let fixture = SameRuntimeFixture::new();
    let observer = fixture.context();

    let owner = fixture.context();
    let (promise, task, owner_context) = owner
        .task_owned_promise("NC5 cancelled task promise")
        .expect("task promise should register");
    let (runtime, call, blocked) = block_task_promise(&observer, promise);
    assert_eq!(task.cancel(), EvaluationTaskCancellation::Requested);
    assert_task_terminal_fails_checkpoint(&observer, &runtime, call, &blocked, "was cancelled");
    drop((owner_context, owner));

    let (runtime, call, blocked) = {
        let owner = fixture.context();
        let (promise, _task, owner_context) = owner
            .task_owned_promise("NC5 abandoned task promise")
            .expect("task promise should register");
        let blocked = block_task_promise(&observer, promise);
        drop(owner_context);
        blocked
    };
    assert_task_terminal_fails_checkpoint(&observer, &runtime, call, &blocked, "was abandoned");

    let owner = fixture.context();
    let (promise, task, owner_context) = owner
        .task_owned_promise("NC5 failed task promise")
        .expect("task promise should register");
    let (runtime, call, blocked) = block_task_promise(&observer, promise);
    owner.fail_wait(task.wait(), "NC5 producer task failure");
    assert_task_terminal_fails_checkpoint(
        &observer,
        &runtime,
        call,
        &blocked,
        "NC5 producer task failure",
    );
    drop((owner_context, owner));
}

#[test]
fn callable_checkpoint_admits_each_lazy_source_family_once() {
    let context = test_context();
    let host_calls = Arc::new(AtomicUsize::new(0));
    let observed_host_calls = Arc::clone(&host_calls);
    let host_values = context.values().clone();

    let application = LazyValue::from_application(
        context.values(),
        Value::Builtin(Builtin::Add),
        Arc::from([Value::Number(1.into())]),
    );
    let reflection = match Value::reflection_task_result(
        context.values(),
        context.values().unit(),
    ) {
        Value::Lazy(lazy) => lazy,
        _ => unreachable!("reflection task results are lazy"),
    };
    let access = LazyValue::from_access(
        context.values(),
        Arc::<[crate::core_net::CoreDataKey]>::from([]),
        Arc::from([Value::Builtin(Builtin::Add)]),
    );
    let host = LazyValue::host_call(context.values(), "NC5 host source", move |bundle| {
        assert!(bundle.into_roots().is_empty());
        observed_host_calls.fetch_add(1, Ordering::SeqCst);
        Ok(crate::runtime::RuntimeValueRoot::new(
            &host_values,
            Value::Builtin(Builtin::Add),
        ))
    });
    let mut net = NetBuilder::<CoreSpecialization>::new();
    let result = net.data(Value::Builtin(Builtin::Add));
    let net = context
        .values()
        .instantiate_core_net(&net.finish(result));
    let net = LazyValue::from_net_computation(context.values(), NetValue::new(net));

    for (name, lazy) in [
        ("application", application),
        ("reflection", reflection),
        ("static access", access),
        ("host", host),
        ("net", net),
    ] {
        let lazy_id = lazy.id(context.values());
        let lazy_root = lazy.root(context.values());
        let (runtime, call) = claimed_core_call_in(context.values(), Value::Lazy(lazy));
        assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
        let blocked = blocked_checkpoint(context.values(), &runtime, call.pair);
        assert_eq!(
            checkpoint_semantic_observation(&runtime, context.values(), call.pair).focus,
            Some(DeferredValueId::Lazy(lazy_id)),
            "{name}"
        );
        let second = crate::eval::lazy_root_wait(&context, &lazy_root)
            .expect("canonical lazy producer remains admissible");
        assert_eq!(second.get(), blocked.wait.0.get(), "{name}");
        assert_eq!(second.producer(), blocked.wait.0.producer(), "{name}");

        let interface = runtime.test_with(context.values(), RuntimeNet::exposed);
        let parked = normalization_request_in(&context, &runtime, interface)
            .drive(&context)
            .expect_err("the still-unresolved producer must park again");
        let repeated = parked
            .blocked_on()
            .expect("the stale poll retains the exact canonical producer");
        assert_eq!(repeated.0.get(), blocked.wait.0.get(), "{name}");
        assert_eq!(
            runtime.test_with(context.values(), RuntimeNet::callable_checkpoint_count),
            1,
            "{name}"
        );
    }
    assert_eq!(host_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cached_lazy_failure_is_already_evaluated_by_construction() {
    let context = test_context();
    let lazy = LazyValue::semantic_thunk(context.values(), "NC5 cache fixture", |_| {
        Ok(Value::Builtin(Builtin::Add))
    });
    crate::core::cache_test_lazy(
        context.values(),
        &lazy,
        Ok(EvaluatedValue::try_from(Value::Builtin(Builtin::Add)).unwrap()),
    )
    .expect("fresh cache fixture accepts its result");
    let (runtime, call) = claimed_core_call_in(context.values(), Value::Lazy(lazy));
    assert!(progress_exact_core_call(&context, &runtime, call).unwrap());
    assert_eq!(
        observe_current_callable_path(&runtime, call),
        CurrentCallablePath::DirectOperator
    );
}
