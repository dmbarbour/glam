use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::core::{
    Dict, EvaluatedValue, EvaluationFailure, FixpointComputation, Key, LazyValue, Value, keys,
};
use crate::evaluation::{
    EvaluationMachinePoll, EvaluationTaskMachine, EvaluationWaitPoll, ReflectionTaskLauncher,
    ReflectionTaskResultPolicy,
};
use crate::number::Number;

use super::*;

fn unit_value() -> Value {
    crate::core::test_value_factory().unit()
}

fn initial_metadata() -> Value {
    Value::initial_metadata_carrier(&crate::core::test_value_factory())
}

fn closed_net(build: impl FnOnce(&mut NetBuilder<CoreSpecialization>) -> Port) -> NetValue {
    let mut builder = NetBuilder::new();
    let exposed = build(&mut builder);
    NetValue::new(builder.finish(exposed).instantiate_shared())
}

fn fixture_computation(expr: TestExpr) -> Value {
    lower_test_computation_value(expr)
}

fn apply_test_values(function: Value, arguments: impl IntoIterator<Item = Value>) -> Value {
    apply_values(&test_context(), function, arguments.into_iter().collect())
        .expect("test application should accept a callable value")
}

fn cached_value(lazy: &LazyValue) -> Value {
    lazy.cached()
        .expect("lazy value should be cached")
        .expect("lazy value should succeed")
        .into_value()
}

struct DropSignal(Arc<AtomicBool>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
enum GateFailureStage {
    LauncherConstruction,
    TaskPoll,
}

struct GateFailureLauncher {
    failure: Arc<EvaluationFailure>,
    stage: GateFailureStage,
    builds: Arc<AtomicUsize>,
}

impl ReflectionTaskLauncher for GateFailureLauncher {
    fn build(
        &self,
        _context: EvalContext,
        _effect: Value,
        _result_policy: ReflectionTaskResultPolicy,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        match self.stage {
            GateFailureStage::LauncherConstruction => Err(self.failure.clone()),
            GateFailureStage::TaskPoll => Ok(Box::new(GateFailureMachine(self.failure.clone()))),
        }
    }
}

struct GateFailureMachine(Arc<EvaluationFailure>);

impl EvaluationTaskMachine for GateFailureMachine {
    fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
        EvaluationMachinePoll::Failed(self.0.clone())
    }
}

#[derive(Clone)]
enum FixtureTaskTerminal {
    Complete(Value),
    Failed(Arc<EvaluationFailure>),
    Cancelled,
}

struct FixtureTaskLauncher {
    terminal: FixtureTaskTerminal,
    builds: Arc<AtomicUsize>,
    result_policies: Arc<Mutex<Vec<ReflectionTaskResultPolicy>>>,
}

impl ReflectionTaskLauncher for FixtureTaskLauncher {
    fn build(
        &self,
        _context: EvalContext,
        _effect: Value,
        result_policy: ReflectionTaskResultPolicy,
    ) -> Result<Box<dyn EvaluationTaskMachine>, Arc<EvaluationFailure>> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        self.result_policies
            .lock()
            .expect("fixture result policies were poisoned")
            .push(result_policy);
        Ok(Box::new(FixtureTaskMachine {
            terminal: Some(self.terminal.clone()),
        }))
    }
}

struct FixtureTaskMachine {
    terminal: Option<FixtureTaskTerminal>,
}

impl EvaluationTaskMachine for FixtureTaskMachine {
    fn poll(&mut self, _step_budget: usize) -> EvaluationMachinePoll {
        match self
            .terminal
            .take()
            .expect("a terminal fixture task must be polled only once")
        {
            FixtureTaskTerminal::Complete(value) => EvaluationMachinePoll::Complete(value),
            FixtureTaskTerminal::Failed(error) => EvaluationMachinePoll::Failed(error),
            FixtureTaskTerminal::Cancelled => EvaluationMachinePoll::Cancelled,
        }
    }
}

#[test]
fn terminal_lazy_evaluation_releases_successful_and_failed_sources() {
    let context = test_context();
    let success_dropped = Arc::new(AtomicBool::new(false));
    let success_signal = DropSignal(success_dropped.clone());
    let success = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "successful source release",
        move |_| {
            let _keep_signal_captured = &success_signal;
            Ok(unit_value())
        },
    );

    assert_eq!(
        eval_lazy(&context, &success).expect("lazy source should succeed"),
        unit_value()
    );
    assert!(success.source_snapshot().is_none());
    assert!(
        success_dropped.load(Ordering::Acquire),
        "successful production should release its source captures"
    );

    let failure_dropped = Arc::new(AtomicBool::new(false));
    let failure_signal = DropSignal(failure_dropped.clone());
    let failure = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "failed source release",
        move |_| {
            let _keep_signal_captured = &failure_signal;
            Err(EvaluationHalt::new("expected lazy failure"))
        },
    );

    let error = eval_lazy(&context, &failure).expect_err("lazy source should fail");
    assert_eq!(error.to_string(), "expected lazy failure");
    assert!(failure.source_snapshot().is_none());
    assert!(
        failure_dropped.load(Ordering::Acquire),
        "failed production should release its source captures"
    );
}

#[test]
fn evaluation_context_frames_use_an_atom_operation_and_optional_named_arguments() {
    assert_eq!(
        evaluation_context_frame("list_index"),
        Value::Dict(Dict::new_sync().insert(
            (*keys::EVAL).clone(),
            Value::Dict(Dict::new_sync().insert(
                (*keys::OP).clone(),
                Key::atom_from_text("list_index").to_value_with(&crate::core::test_value_factory()),
            )),
        ))
    );

    let args = Dict::new_sync().insert((*keys::PATH).clone(), Value::binary_from_text("conf.env"));
    assert_eq!(
        evaluation_context_frame_with_args("path_lookup", args.clone()),
        Value::Dict(
            Dict::new_sync().insert(
                (*keys::EVAL).clone(),
                Value::Dict(
                    Dict::new_sync()
                        .insert(
                            (*keys::OP).clone(),
                            Key::atom_from_text("path_lookup")
                                .to_value_with(&crate::core::test_value_factory()),
                        )
                        .insert((*keys::ARGS).clone(), Value::Dict(args)),
                ),
            )
        )
    );
}

#[test]
fn raw_net_values_are_opaque_while_net_computations_expose_data() {
    let net = closed_net(|builder| builder.data(n(42)));
    let raw = Value::Net(net.clone());

    assert_eq!(eval_value(&test_context(), &raw).unwrap(), raw);

    let computation = Value::Lazy(LazyValue::from_net_computation(
        &crate::core::test_value_factory(),
        net,
    ));
    assert_eq!(eval_value(&test_context(), &computation).unwrap(), n(42));
    assert_eq!(eval_value(&test_context(), &computation).unwrap(), n(42));
}

#[test]
fn net_arity_functions_attach_to_applications_through_cursors() {
    let identity = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    let expression = TestExpr::Apply(
        Arc::new(TestExpr::Value(apply_test_values(
            Value::Builtin(Builtin::NetArity),
            [n(1), Value::Net(identity)],
        ))),
        Arc::new(TestExpr::Value(n(42))),
    );

    assert_eq!(eval_closed_expr(&expression).unwrap(), n(42));
}

#[test]
fn net_arity_contextualizes_failure_while_demanding_its_arity() {
    let net = closed_net(|builder| builder.data(n(42)));
    let error = apply_values(
        &test_context(),
        Value::Builtin(Builtin::NetArity),
        vec![
            Value::error(
                &crate::core::test_value_factory(),
                "arity computation failed",
            ),
            Value::Net(net),
        ],
    )
    .expect_err("failure while evaluating net arity must propagate");
    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("net_arity")]
    );
}

#[test]
fn observing_a_function_net_preserves_the_net_value() {
    let identity = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    let expected = identity.clone();

    assert_eq!(
        eval_value(&test_context(), &Value::Net(identity)).unwrap(),
        Value::Net(expected)
    );
}

#[test]
fn net_backed_lazy_values_require_an_exposed_data_node() {
    let identity = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    let value = Value::Lazy(LazyValue::from_net_computation(
        &crate::core::test_value_factory(),
        identity,
    ));

    let error = eval_value(&test_context(), &value)
        .expect_err("a net computation must expose data rather than a bind");
    assert_eq!(
        error.to_string(),
        "lazy net computation exposed a bind instead of data"
    );
    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("net_computation")]
    );
}

#[test]
fn net_backed_lazy_values_reject_non_data_normal_forms() {
    let inert = closed_net(|builder| builder.copy(0).input);
    let value = Value::Lazy(LazyValue::from_net_computation(
        &crate::core::test_value_factory(),
        inert,
    ));

    let error = eval_value(&test_context(), &value)
        .expect_err("an inert net computation must not produce a value");
    assert_eq!(
        error.to_string(),
        "lazy net computation reached a non-data normal form"
    );
    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("net_computation")]
    );
}

#[test]
fn early_function_data_is_left_to_ordinary_stuck_net_semantics() {
    let one_argument_stage = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        let erase = builder.copy(0);
        builder.wire(argument, erase.input);
        let data = builder.data(n(42));
        builder.wire(result, data);
        application
    });
    let function = FunctionValue::new(one_argument_stage, 2);
    let partial = apply_function_values(&test_context(), function, vec![n(0)])
        .expect("partial application should not inspect the staged interface");
    let Value::Function(partial) = partial else {
        panic!("partial application should retain an ordinary function value")
    };
    assert_eq!(partial.remaining_arity(), 1);

    assert_eq!(
        eval_value(
            &test_context(),
            &apply_function_values(&test_context(), partial, vec![n(1)]).unwrap(),
        )
        .unwrap_err()
        .to_string(),
        "application requires a function value, received Number"
    );
}

#[test]
fn net_arity_bridges_opaque_nets_to_computations_and_functions() {
    let data_net = closed_net(|builder| builder.data(n(42)));
    let computation = apply_test_values(
        Value::Builtin(Builtin::NetArity),
        [n(0), Value::Net(data_net)],
    );
    assert_eq!(eval_value(&test_context(), &computation).unwrap(), n(42));

    let identity = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    let function = apply_test_values(
        Value::Builtin(Builtin::NetArity),
        [n(1), Value::Net(identity)],
    );
    let Value::Function(function) = eval_value(&test_context(), &function).unwrap() else {
        panic!("positive net arity should produce a function value")
    };
    assert_eq!(function.remaining_arity(), 1);
    let result = apply_test_values(Value::Function(function), [n(43)]);
    assert_eq!(eval_value(&test_context(), &result).unwrap(), n(43));
}

#[test]
fn saturated_function_calls_reject_a_remaining_bind() {
    let two_argument_stage = closed_net(|builder| {
        let spine = builder.bind_spine(2);
        for argument in &spine.arguments {
            let eraser = builder.copy(0);
            builder.wire(*argument, eraser.input);
        }
        let result = builder.data(n(42));
        builder.wire(spine.result, result);
        spine.input
    });
    let malformed = FunctionValue::new(two_argument_stage, 1);
    let result = apply_function_values(&test_context(), malformed, vec![n(0)]).unwrap();

    assert_eq!(
        eval_value(&test_context(), &result)
            .unwrap_err()
            .to_string(),
        "function call exposed a bind instead of data"
    );
}

#[test]
fn zero_arity_apply_operator_is_data_identity() {
    let operator = apply_arity_operator(0, Arc::from([]));
    let data = n(42);

    assert_eq!(
        apply_core_operator(&test_context(), &operator, &data).unwrap(),
        OperatorYield::Data(data)
    );
}

#[test]
fn compiled_function_values_reuse_one_shared_interaction_net() {
    let function = closed_function_value(1, TestExpr::Local(0));
    let (Value::Function(first), Value::Function(second)) = (
        eval_value(&test_context(), &function).unwrap(),
        eval_value(&test_context(), &function).unwrap(),
    ) else {
        panic!("closed functions should evaluate to shared function stages");
    };
    assert!(first.stage().runtime().ptr_eq(second.stage().runtime()));
}

#[test]
fn curried_function_partial_application_retains_a_shared_stage() {
    let function = closed_function_value(3, TestExpr::Local(2));
    let partially_applied = eval_value(&test_context(), &apply_test_values(function, [n(11)]))
        .expect("first application should construct the remaining function stage");
    let Value::Function(first_stage) = &partially_applied else {
        panic!("partial application should produce another function stage");
    };
    assert_eq!(first_stage.remaining_arity(), 2);
    let cloned_stage = partially_applied.clone();
    let Value::Function(cloned_stage) = cloned_stage else {
        unreachable!()
    };
    assert!(
        first_stage
            .stage()
            .runtime()
            .ptr_eq(cloned_stage.stage().runtime())
    );

    let result = apply_test_values(partially_applied, [n(22), n(33)]);
    assert_eq!(eval_value(&test_context(), &result).unwrap(), n(11));
}

#[test]
fn function_application_accepts_a_cursor_backed_function_argument_without_forcing_it() {
    let ignores_first = closed_function_value(2, TestExpr::Local(0));
    let forwards_argument = closed_function_value(
        1,
        TestExpr::Apply(
            Arc::new(TestExpr::Value(ignores_first)),
            Arc::new(TestExpr::Local(0)),
        ),
    );
    let unresolved_function = closed_function_value(1, TestExpr::Local(0));

    let partial = eval_value(
        &test_context(),
        &apply_test_values(forwards_argument, [unresolved_function]),
    )
    .expect("net attachment must not demand a callable argument as embedded data");
    assert!(matches!(partial, Value::Function(_)));

    assert_eq!(
        eval_value(&test_context(), &apply_test_values(partial, [n(42)])).unwrap(),
        n(42)
    );
}

#[test]
fn batched_application_spine_keeps_unused_arguments_lazy() {
    let forced = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lazy_argument = |label: &'static str| {
        let forced = forced.clone();
        Value::deferred(&crate::core::test_value_factory(), label, move |_| {
            forced.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(99))
        })
    };
    let function = closed_function_value(3, TestExpr::Local(2));
    let application = apply_test_values(
        function,
        [n(11), lazy_argument("second"), lazy_argument("third")],
    );

    assert_eq!(eval_value(&test_context(), &application).unwrap(), n(11));
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn batched_application_preserves_captured_access() {
    let key = Key::atom_from_text("answer");
    let function = closed_function_value(
        2,
        TestExpr::Access(
            Arc::new(TestExpr::Local(1)),
            Arc::from([TestKey::Key(key.clone())]),
        ),
    );
    let dict = Value::Dict(Dict::new_sync().insert(key, n(42)));
    let application = apply_test_values(function, [dict, n(0)]);

    assert_eq!(eval_value(&test_context(), &application).unwrap(), n(42));
}

#[test]
fn compiling_a_function_does_not_evaluate_its_body() {
    let function = closed_function_value(
        1,
        TestExpr::Value(Value::error(
            &crate::core::test_value_factory(),
            "unreached body",
        )),
    );

    assert!(matches!(function, Value::Function(_)));
}

fn n(value: i64) -> Value {
    Value::Number(value.into())
}

#[test]
fn promised_values_fail_fast_without_poisoning_later_assignment() {
    let promised = PromisedValue::new(&crate::core::test_value_factory(), "test promised value");
    let value = Value::Promised(promised.clone());

    assert_eq!(
        eval_value(&test_context(), &value).unwrap_err().to_string(),
        "promised value was observed before initialization"
    );
    assert_eq!(promised.assignment(), None);
    promised.set(n(42)).unwrap();
    assert_eq!(eval_value(&test_context(), &value).unwrap(), n(42));
}

#[test]
fn deferred_computation_blockage_does_not_poison_its_lazy_cache() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let observer = session.with_new_task().unwrap();
    let promise = PromisedValue::fixpoint(&owner, "deferred computation input").unwrap();
    let promised_value = Value::Promised(promise.clone());
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted_attempts = attempts.clone();
    let lazy = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "promise-demanding deferred computation",
        move |context| {
            counted_attempts.fetch_add(1, Ordering::SeqCst);
            eval_value(context, &promised_value)
        },
    );
    let value = Value::Lazy(lazy.clone());

    let blocked =
        eval_value(&observer, &value).expect_err("the unresolved input promise should block");
    assert!(blocked.blocked_on().is_some());
    assert!(
        lazy.cached().is_none(),
        "a retryable deferred error must not poison the terminal lazy cache"
    );
    assert!(
        session.lazy_failure(&lazy).is_none(),
        "the scheduler must not record a permanent lazy failure while its input may change"
    );

    promise.set(n(42)).unwrap();
    assert_eq!(eval_value(&observer, &value).unwrap(), n(42));
    assert!(
        attempts.load(Ordering::SeqCst) >= 2,
        "resuming the producer should retry the deferred computation"
    );

    let completed_attempts = attempts.load(Ordering::SeqCst);
    assert_eq!(eval_value(&observer, &value).unwrap(), n(42));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        completed_attempts,
        "the successful terminal cache should prevent another thunk invocation"
    );
}

#[test]
fn deferred_computation_caches_one_structured_failure() {
    let context = test_context();
    let detail = Key::atom_from_text("detail");
    let emission = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::MSG).clone(),
                Value::Dict(Dict::new_sync().insert(
                    (*keys::TEXT).clone(),
                    Value::binary_from_text("structured deferred failure"),
                )),
            )
            .insert(detail.clone(), n(7)),
    );
    let frame = evaluation_context_frame("deferred_test");
    let thunk_emission = emission.clone();
    let thunk_frame = frame.clone();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted_attempts = attempts.clone();
    let lazy = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "structured deferred failure",
        move |_| {
            counted_attempts.fetch_add(1, Ordering::SeqCst);
            Err(EvaluationHalt::from_value(thunk_emission.clone())
                .with_context(thunk_frame.clone()))
        },
    );
    let value = Value::Lazy(lazy.clone());

    let error =
        eval_value(&context, &value).expect_err("the deferred computation should fail permanently");
    let observed_failure = error.into_permanent_failure();
    let cached_failure = lazy
        .cached()
        .expect("a permanent deferred failure should be cached")
        .expect_err("the cached result should be the failure");

    assert!(Arc::ptr_eq(&observed_failure, &cached_failure));
    assert_eq!(cached_failure.emission_value(), Some(&emission));
    assert_eq!(cached_failure.contexts(), [frame]);
    let Value::Dict(diagnostic) = failure_diagnostic_value(&cached_failure) else {
        panic!("a structured failure should project to a diagnostic dictionary")
    };
    assert_eq!(diagnostic.get(&detail), Some(&n(7)));

    assert_eq!(
        eval_value(&context, &value)
            .expect_err("the cached value should remain failed")
            .into_permanent_failure(),
        cached_failure
    );
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "a cached permanent failure must not invoke its thunk again"
    );
}

#[test]
fn deferred_list_effect_work_blocks_and_resumes() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let observer = session.with_new_task().unwrap();
    let promise = PromisedValue::fixpoint(&owner, "deferred list effect input").unwrap();
    let handled = apply_builtin(
        &observer,
        Builtin::ListEffect,
        Vec::new(),
        Value::Promised(promise.clone()),
    )
    .expect("constructing the lazy list-effect result should not demand its operation");
    let Value::List(results) = handled else {
        panic!("the list effect handler should return a list")
    };

    let blocked = list_to_value_items(&observer, &results)
        .expect_err("observing the list should block on its unresolved effect");
    assert!(blocked.blocked_on().is_some());

    let return_effect = effect_value(closed_function_value(
        1,
        TestExpr::Apply(
            Arc::new(TestExpr::Access(
                Arc::new(TestExpr::Local(0)),
                Arc::from([TestKey::Key((*keys::R).clone())]),
            )),
            Arc::new(TestExpr::Value(n(42))),
        ),
    ));
    promise.set(return_effect).unwrap();

    assert_eq!(
        list_to_value_items(&observer, &results)
            .expect("the list effect should resume after its operation is assigned"),
        vec![n(42)]
    );
}

#[test]
fn interaction_net_construction_dependency_does_not_poison_its_lazy_value() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let observer = session.with_new_task().unwrap();
    let promise = PromisedValue::fixpoint(&owner, "pending net effect").unwrap();
    let lazy = LazyValue::from_net_construction(
        &crate::core::test_value_factory(),
        Value::Promised(promise),
    );
    let value = Value::Lazy(lazy.clone());

    let blocked = eval_value(&observer, &value)
        .expect_err("net construction should block on its unresolved effect");
    assert!(blocked.blocked_on().is_some());
    assert!(
        lazy.cached().is_none(),
        "a retryable construction dependency must not become a cached failure"
    );
}

#[test]
fn deferred_computation_caches_one_text_failure() {
    let context = test_context();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted_attempts = attempts.clone();
    let lazy = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "text deferred failure",
        move |_| {
            counted_attempts.fetch_add(1, Ordering::SeqCst);
            Err(EvaluationHalt::new("text deferred failure"))
        },
    );
    let value = Value::Lazy(lazy.clone());

    let first = eval_value(&context, &value)
        .expect_err("the deferred computation should fail")
        .into_permanent_failure();
    let cached = lazy
        .cached()
        .expect("the deferred failure should be cached")
        .expect_err("the terminal cache should contain a failure");
    assert!(Arc::ptr_eq(&first, &cached));

    let second = eval_value(&context, &value)
        .expect_err("the cached deferred computation should remain failed")
        .into_permanent_failure();
    assert!(Arc::ptr_eq(&cached, &second));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn deferred_computation_preserves_context_annotation_frames() {
    let context = test_context();
    let frame = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("deferred"),
        Value::binary_from_text("context annotation"),
    ));
    let annotation = Value::Dict(Dict::new_sync().insert((*keys::CONTEXT).clone(), frame.clone()));
    let lazy = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "context-annotated deferred failure",
        move |context| {
            apply_builtin(
                context,
                Builtin::Anno,
                vec![annotation.clone()],
                Value::error(
                    &crate::core::test_value_factory(),
                    "annotated deferred failure",
                ),
            )
        },
    );

    let failure = eval_value(&context, &Value::Lazy(lazy))
        .expect_err("the annotated deferred computation should fail")
        .into_permanent_failure();
    assert_eq!(failure.contexts(), [frame]);
}

#[test]
fn computed_lazy_waits_on_an_empty_promise_without_caching_its_error() {
    let context = test_context();
    let promise = PromisedValue::new(&crate::core::test_value_factory(), "late assignment");
    let lazy = LazyValue::from_access(
        &crate::core::test_value_factory(),
        Arc::from([]),
        Arc::from([Value::Promised(promise.clone())]),
    );
    let value = Value::Lazy(lazy.clone());

    let blocked = eval_value(&context, &value).expect_err("empty promise should block its lazy");
    assert!(blocked.blocked_on().is_some());
    assert!(lazy.cached().is_none());
    assert_eq!(promise.assignment(), None);

    promise.set(n(42)).unwrap();
    assert_eq!(eval_value(&context, &value).unwrap(), n(42));
    assert_eq!(
        lazy.cached(),
        Some(Ok(EvaluatedValue::try_from(n(42)).unwrap()))
    );
}

#[test]
fn promised_assignment_follows_a_lazy_without_resolving_the_raw_assignment() {
    let context = test_context();
    let target = LazyValue::deferred(&crate::core::test_value_factory(), "promise target", |_| {
        Ok(n(42))
    });
    let promise = PromisedValue::new(&crate::core::test_value_factory(), "forwarding promise");
    promise.set(Value::Lazy(target.clone())).unwrap();

    assert_eq!(
        eval_value(&context, &Value::Promised(promise.clone())).unwrap(),
        n(42)
    );
    assert_eq!(promise.assignment(), Some(Ok(Value::Lazy(target.clone()))));
    assert_eq!(
        target.cached(),
        Some(Ok(EvaluatedValue::try_from(n(42)).unwrap()))
    );
}

#[test]
fn promised_failure_preserves_structured_diagnostic_and_identity() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let observer = session.with_new_task().unwrap();
    let promise = PromisedValue::fixpoint(&owner, "structured promise failure").unwrap();
    let wait = promise
        .task()
        .expect("task-owned promise should expose its wait")
        .wait()
        .clone();
    let detail = Key::atom_from_text("detail");
    let emission = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::MSG).clone(),
                Value::Dict(Dict::new_sync().insert(
                    (*keys::TEXT).clone(),
                    Value::binary_from_text("structured promise failure"),
                )),
            )
            .insert(detail.clone(), n(7)),
    );
    let frame = evaluation_context_frame("promise_test");
    let failure =
        Arc::new(EvaluationFailure::emission(emission.clone()).with_context(frame.clone()));

    promise
        .fail(failure.clone())
        .expect("new promise should accept one permanent failure");

    let observed = eval_value(&observer, &Value::Promised(promise))
        .expect_err("failed promise should expose its permanent failure")
        .into_permanent_failure();
    assert!(Arc::ptr_eq(&failure, &observed));
    assert_eq!(observed.emission_value(), Some(&emission));
    assert_eq!(observed.contexts(), [frame]);

    let EvaluationWaitPoll::Failed(wait_failure) = session.poll_wait(&wait) else {
        panic!("the promise wait should publish the same permanent failure")
    };
    assert!(Arc::ptr_eq(&failure, &wait_failure));

    let Value::Dict(diagnostic) = failure_diagnostic_value(&observed) else {
        panic!("a structured promise failure should project to a diagnostic dictionary")
    };
    assert_eq!(diagnostic.get(&detail), Some(&n(7)));
}

#[test]
fn promise_only_cycle_remains_blocked_without_poisoning_its_assignment() {
    let context = test_context();
    let promise = PromisedValue::new(&crate::core::test_value_factory(), "promise cycle");
    promise
        .set(Value::Promised(promise.clone()))
        .expect("promise should accept its own named assignment");

    let error = eval_value(&context, &Value::Promised(promise.clone()))
        .expect_err("strict promise recursion should remain blocked");
    assert!(error.blocked_on().is_some());
    assert!(context.promise_failure(&promise).is_none());
    assert!(matches!(
        promise.assignment(),
        Some(Ok(Value::Promised(assigned))) if assigned == promise
    ));
}

#[test]
fn mixed_promise_lazy_cycle_remains_retryable_without_poisoning_the_lazy() {
    let context = test_context();
    let promise = PromisedValue::new(&crate::core::test_value_factory(), "mixed promise");
    let lazy = LazyValue::from_access(
        &crate::core::test_value_factory(),
        Arc::from([]),
        Arc::from([Value::Promised(promise.clone())]),
    );
    promise.set(Value::Lazy(lazy.clone())).unwrap();

    let error = eval_value(&context, &Value::Promised(promise.clone()))
        .expect_err("strict mixed recursion should remain blocked");
    assert!(error.blocked_on().is_some());
    assert!(context.promise_failure(&promise).is_none());
    assert!(context.lazy_failure(&lazy).is_none());
    assert!(lazy.cached().is_none());
    assert!(matches!(
        promise.assignment(),
        Some(Ok(Value::Lazy(assigned))) if assigned == lazy
    ));
}

#[test]
fn task_owned_fixpoint_rejects_recursive_demand_and_blocks_other_tasks() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let observer = session.with_new_task().unwrap();
    let fixpoint = PromisedValue::fixpoint(&owner, "test fixpoint").unwrap();
    let wait = fixpoint
        .task()
        .expect("task-owned fixpoint should expose its wait")
        .wait()
        .clone();
    let value = Value::Promised(fixpoint.clone());

    let recursive = eval_value(&owner, &value).unwrap_err();
    assert!(
        recursive
            .to_string()
            .contains("recursively observed itself")
    );

    let blocked = eval_value(&observer, &value).unwrap_err();
    assert!(blocked.blocked_on().is_some());
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 1);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 1);

    fixpoint.set(n(42)).unwrap();
    assert_eq!(eval_value(&observer, &value).unwrap(), n(42));
    assert_eq!(
        session.poll_wait(&wait),
        EvaluationWaitPoll::Complete(n(42)),
        "the retired promise wait must preserve late terminal observation"
    );
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 0);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 0);
}

#[test]
fn failed_task_fails_its_unresolved_fixpoint_promises() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let observer = session.with_new_task().unwrap();
    let fixpoint = PromisedValue::fixpoint(&owner, "test fixpoint").unwrap();
    let wait = fixpoint
        .task()
        .expect("task-owned fixpoint should expose its wait")
        .wait()
        .clone();
    let value = Value::Promised(fixpoint);

    assert!(
        eval_value(&observer, &value)
            .unwrap_err()
            .blocked_on()
            .is_some()
    );
    owner.fail_unresolved_promises(Arc::new(EvaluationFailure::message(
        "producer failed deliberately",
    )));
    assert_eq!(
        eval_value(&observer, &value).unwrap_err().to_string(),
        "producer failed deliberately"
    );
    assert!(matches!(
        session.poll_wait(&wait),
        EvaluationWaitPoll::Failed(error)
            if error.to_string() == "producer failed deliberately"
    ));
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 0);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 0);
}

#[test]
fn explicitly_failed_task_promise_retires_its_wait_record() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let fixpoint = PromisedValue::fixpoint(&owner, "test fixpoint").unwrap();
    let wait = fixpoint
        .task()
        .expect("task-owned fixpoint should expose its wait")
        .wait()
        .clone();

    fixpoint
        .fail_message("fixpoint failed deliberately")
        .unwrap();

    assert!(matches!(
        session.poll_wait(&wait),
        EvaluationWaitPoll::Failed(error)
            if error.to_string() == "fixpoint failed deliberately"
    ));
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 0);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 0);
}

#[test]
fn producer_failure_retires_every_owned_promise_wait() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let promises = [
        PromisedValue::fixpoint(&owner, "first fixpoint").unwrap(),
        PromisedValue::fixpoint(&owner, "second fixpoint").unwrap(),
    ];
    let waits = promises.each_ref().map(|promise| {
        promise
            .task()
            .expect("task-owned fixpoint should expose its wait")
            .wait()
            .clone()
    });

    owner.fail_unresolved_promises(Arc::new(EvaluationFailure::message(
        "producer failed all fixpoints",
    )));

    for wait in waits {
        assert!(matches!(
            session.poll_wait(&wait),
            EvaluationWaitPoll::Failed(error)
                if error.to_string() == "producer failed all fixpoints"
        ));
    }
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 0);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 0);
}

#[test]
fn promise_change_notification_retires_an_abandoned_task_promise() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let wait = {
        let promise = PromisedValue::fixpoint(&owner, "abandoned fixpoint").unwrap();
        promise
            .task()
            .expect("task-owned fixpoint should expose its wait")
            .wait()
            .clone()
    };
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_terminal, 1);
    assert_eq!(counts.owned_promise_waits, 1);

    session.notify_promise_changed();

    assert!(matches!(
        session.poll_wait(&wait),
        EvaluationWaitPoll::Failed(error)
            if error.to_string() == "promised value no longer exists"
    ));
    let counts = session.task_registry_counts();
    assert_eq!(counts.promises_active, 0);
    assert_eq!(counts.promises_terminal, 0);
    assert_eq!(counts.owned_promise_waits, 0);
}

#[test]
fn value_fixpoint_reports_its_strict_lazy_dependency_cycle() {
    let context = test_context();
    let function = closed_function_value(1, TestExpr::Local(0));
    let fixpoint = Value::Lazy(LazyValue::computed_fixpoint(
        &crate::core::test_value_factory(),
        "recursive value fixpoint",
        FixpointComputation::Function(function),
    ));

    let error = eval_value(&context, &fixpoint).unwrap_err();
    assert!(
        error.to_string().contains("lazy dependency cycle"),
        "{error}"
    );
    assert!(error.blocked_on().is_none());

    let Value::Lazy(fixpoint_lazy) = &fixpoint else {
        unreachable!("test fixture is a lazy fixpoint")
    };
    let failure = context
        .lazy_failure(fixpoint_lazy)
        .expect("the fixpoint task should retain its structured failure");
    let cycle = failure
        .dependency_cycle_value()
        .expect("strict recursion should retain a dependency cycle");
    assert!(
        cycle
            .members
            .iter()
            .any(|member| member.id == fixpoint_lazy.id())
    );

    let observer = context.with_new_task().unwrap();
    assert_eq!(
        eval_value(&observer, &fixpoint).unwrap_err().to_string(),
        error.to_string()
    );
}

#[test]
fn fixpoint_builtin_reports_a_strict_lazy_dependency_cycle() {
    let expression = builtin1_expr(Builtin::Fixpoint, function_expr(1, TestExpr::Local(0)));

    let error = eval_closed_expr(&expression).unwrap_err();
    assert!(
        error.to_string().contains("lazy dependency cycle"),
        "{error}"
    );
    assert!(error.blocked_on().is_none());
}

#[test]
fn suspended_value_fixpoint_keeps_one_knot_for_concurrent_observers() {
    let session = test_context();
    let owner = session.with_new_task().unwrap();
    let observer = session.with_new_task().unwrap();
    let gate = reflection_annotation(&owner, n(0), n(42));
    let function = closed_function_value(1, TestExpr::Value(gate));
    let fixpoint = Value::Lazy(LazyValue::computed_fixpoint(
        &crate::core::test_value_factory(),
        "suspended value fixpoint",
        FixpointComputation::Function(function),
    ));

    let producer_block = eval_value(&owner, &fixpoint).unwrap_err();
    let producer_wait = producer_block
        .blocked_on()
        .expect("producer should suspend on its reflection gate");
    let observer_block = eval_value(&observer, &fixpoint).unwrap_err();
    let fixpoint_wait = observer_block
        .blocked_on()
        .expect("observer should wait on the fixpoint itself");
    assert_eq!(
        producer_wait, fixpoint_wait,
        "all observers should wait on the session-owned lazy task"
    );

    owner.complete_wait(&producer_wait.0);
    assert_eq!(eval_value(&owner, &fixpoint).unwrap(), n(42));
    assert_eq!(eval_value(&observer, &fixpoint).unwrap(), n(42));
}

#[test]
fn computed_fixpoint_uses_session_local_waits_while_sharing_its_result() {
    let first = test_context();
    let second = test_context();
    let promise = PromisedValue::new(
        &crate::core::test_value_factory(),
        "cross-session fixpoint input",
    );
    let function = closed_function_value(1, TestExpr::Value(Value::Promised(promise.clone())));
    let lazy = LazyValue::computed_fixpoint(
        &crate::core::test_value_factory(),
        "cross-session value fixpoint",
        FixpointComputation::Function(function),
    );
    let fixpoint = Value::Lazy(lazy.clone());

    let first_block =
        eval_value(&first, &fixpoint).expect_err("the first session should wait for the promise");
    let second_block = eval_value(&second, &fixpoint)
        .expect_err("the second session should own an independent wait");
    assert!(first_block.blocked_on().is_some());
    assert!(second_block.blocked_on().is_some());
    assert!(lazy.cached().is_none());

    promise.set(n(42)).unwrap();
    assert_eq!(eval_value(&first, &fixpoint).unwrap(), n(42));
    assert_eq!(eval_value(&second, &fixpoint).unwrap(), n(42));
    assert_eq!(cached_value(&lazy), n(42));
}

#[test]
fn computed_fixpoint_preserves_a_forwarded_structured_failure() {
    let context = test_context();
    let source = LazyValue::error(&crate::core::test_value_factory(), "fixpoint source failed");
    let function = closed_function_value(1, TestExpr::Value(Value::Lazy(source.clone())));
    let fixpoint = LazyValue::computed_fixpoint(
        &crate::core::test_value_factory(),
        "failed value fixpoint",
        FixpointComputation::Function(function),
    );

    let error = eval_value(&context, &Value::Lazy(fixpoint.clone()))
        .expect_err("the source failure should fail the fixpoint");
    assert_eq!(error.to_string(), "fixpoint source failed");

    let source_failure = source.cached().unwrap().unwrap_err();
    let fixpoint_failure = fixpoint.cached().unwrap().unwrap_err();
    assert!(Arc::ptr_eq(&source_failure, &fixpoint_failure));
}

#[test]
fn deferred_values_use_the_context_that_forces_them() {
    let context = test_context();
    let expected_context = context.clone();
    let value = Value::deferred(
        &crate::core::test_value_factory(),
        "context-sensitive test value",
        move |actual_context| {
            assert!(actual_context.shares_session_with(&expected_context));
            Ok(n(42))
        },
    );

    assert_eq!(eval_value(&context, &value).unwrap(), n(42));
}

#[test]
fn forcing_a_lazy_value_reaches_outer_whnf_without_forcing_lazy_fields() {
    let context = test_context();
    let field_forces = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted_field_forces = field_forces.clone();
    let field = Value::deferred(
        &crate::core::test_value_factory(),
        "lazy dictionary field",
        move |_| {
            counted_field_forces.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(42))
        },
    );
    let expected_field = field.clone();
    let forwarded = Value::deferred(
        &crate::core::test_value_factory(),
        "forwarded dictionary",
        move |_| {
            Ok(Value::Dict(
                Dict::new_sync().insert(Key::atom_from_text("field"), field.clone()),
            ))
        },
    );
    let root = Value::deferred(
        &crate::core::test_value_factory(),
        "forwarding root",
        move |_| Ok(forwarded.clone()),
    );

    let forced = eval_value(&context, &root).expect("root should reach dictionary WHNF");
    let Value::Dict(dict) = forced else {
        panic!("forcing should expose the outer dictionary")
    };
    assert_eq!(
        dict.get(&Key::atom_from_text("field")),
        Some(&expected_field)
    );
    assert_eq!(field_forces.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn guarded_lazy_self_reference_reaches_dictionary_whnf() {
    let context = test_context();
    let self_reference = Arc::new(std::sync::OnceLock::<LazyValue>::new());
    let captured = self_reference.clone();
    let lazy = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "guarded self reference",
        move |_| {
            Ok(Value::Dict(
                Dict::new_sync().insert(
                    Key::atom_from_text("tail"),
                    Value::Lazy(
                        captured
                            .get()
                            .expect("guarded self reference should be installed")
                            .clone(),
                    ),
                ),
            ))
        },
    );
    self_reference
        .set(lazy.clone())
        .expect("guarded self reference should be installed once");

    let forced = eval_value(&context, &Value::Lazy(lazy.clone()))
        .expect("a lazy reference under a dictionary constructor is guarded");
    let Value::Dict(dict) = forced else {
        panic!("guarded recursive value should expose a dictionary")
    };
    assert_eq!(
        dict.get(&Key::atom_from_text("tail")),
        Some(&Value::Lazy(lazy))
    );
}

#[test]
fn lazy_aliases_share_and_cache_their_final_whnf() {
    let context = test_context();
    let target = Value::deferred(&crate::core::test_value_factory(), "alias target", |_| {
        Ok(n(42))
    });
    let Value::Lazy(target_lazy) = &target else {
        unreachable!()
    };
    let target_lazy = target_lazy.clone();
    let root = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "shallow alias",
        move |_| Ok(target.clone()),
    );
    let value = Value::Lazy(root.clone());

    assert_eq!(context.deferred_task_count(), 0);
    assert_eq!(eval_value(&context, &value).unwrap(), n(42));
    assert_eq!(
        context.deferred_task_count(),
        0,
        "completed alias producers should retire from the session"
    );
    assert_eq!(cached_value(&target_lazy), n(42));
    assert_eq!(cached_value(&root), n(42));
    assert_eq!(eval_value(&context, &value).unwrap(), n(42));
    assert_eq!(
        context.deferred_task_count(),
        0,
        "cached observation should not register new deferred work"
    );
}

#[test]
fn demanded_forwarding_chain_caches_whnf_in_every_lazy_member() {
    let context = test_context();
    let identity = closed_function_value(1, TestExpr::Local(0));
    let leaf = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "forwarding leaf",
        |_| Ok(n(42)),
    );
    let middle = LazyValue::from_application(
        &crate::core::test_value_factory(),
        identity.clone(),
        Arc::from([Value::Lazy(leaf.clone())]),
    );
    let root = LazyValue::from_application(
        &crate::core::test_value_factory(),
        identity,
        Arc::from([Value::Lazy(middle.clone())]),
    );

    assert_eq!(
        eval_value(&context, &Value::Lazy(root.clone())).unwrap(),
        n(42)
    );
    assert_eq!(cached_value(&leaf), n(42));
    assert_eq!(cached_value(&middle), n(42));
    assert_eq!(cached_value(&root), n(42));
}

#[test]
fn forwarding_chain_preserves_one_structured_failure() {
    let context = test_context();
    let identity = closed_function_value(1, TestExpr::Local(0));
    let leaf = LazyValue::error(&crate::core::test_value_factory(), "shared failure");
    let root = LazyValue::from_application(
        &crate::core::test_value_factory(),
        identity,
        Arc::from([Value::Lazy(leaf.clone())]),
    );

    let error = eval_value(&context, &Value::Lazy(root.clone()))
        .expect_err("forwarding into an error should fail");
    assert_eq!(error.to_string(), "shared failure");

    let leaf_failure = leaf.cached().unwrap().unwrap_err();
    let root_failure = root.cached().unwrap().unwrap_err();
    assert!(Arc::ptr_eq(&leaf_failure, &root_failure));
}

#[test]
fn concurrent_lazy_observers_receive_one_wait_without_parking() {
    let context = test_context();
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let producer_release = release.clone();
    let (started_sender, started_receiver) = std::sync::mpsc::channel();
    let lazy = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "contended lazy",
        move |_| {
            started_sender
                .send(())
                .expect("test should still be waiting for its producer");
            let (lock, changed) = &*producer_release;
            let mut released = lock.lock().expect("test release lock was poisoned");
            while !*released {
                released = changed
                    .wait(released)
                    .expect("test release lock was poisoned");
            }
            Ok(n(42))
        },
    );
    let value = Value::Lazy(lazy);
    let producer_context = context.clone();
    let producer_value = value.clone();
    let producer = std::thread::spawn(move || eval_value(&producer_context, &producer_value));
    started_receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("producer should claim the lazy task");

    let (observed_sender, observed_receiver) = std::sync::mpsc::channel();
    let observer_context = context.clone();
    let observer_value = value.clone();
    let observer = std::thread::spawn(move || {
        observed_sender
            .send(eval_value(&observer_context, &observer_value))
            .expect("test should still be waiting for its observer");
    });
    let observed = observed_receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("a contending observer must return instead of parking");
    let first_wait = observed
        .expect_err("contending observation should block cooperatively")
        .blocked_on()
        .expect("contending observation should expose the lazy task wait");
    let second_wait = eval_value(&context, &value)
        .expect_err("a second contending observation should also block")
        .blocked_on()
        .expect("all contending observations should expose the wait");
    assert_eq!(first_wait, second_wait);
    assert_eq!(
        context.pump_wait(&first_wait.0, 256),
        crate::evaluation::EvaluationPumpOutcome::Busy,
        "a claimed producer is busy rather than quiescent"
    );

    let (lock, changed) = &*release;
    *lock.lock().expect("test release lock was poisoned") = true;
    changed.notify_all();
    observer.join().expect("observer should finish");
    assert_eq!(
        producer.join().expect("producer should finish").unwrap(),
        n(42)
    );
}

#[test]
fn ready_lazy_errors_fail_when_observed() {
    let value = Value::error(&crate::core::test_value_factory(), "deliberate failure");

    assert_eq!(
        eval_value(&test_context(), &value).unwrap_err().to_string(),
        "deliberate failure"
    );
}

fn function_expr(arity: usize, body: TestExpr) -> TestExpr {
    let code = Arc::new(lower_test_function_code(arity, body));
    let captures = (0..code.capture_count())
        .map(TestExpr::Local)
        .map(Arc::new)
        .collect::<Vec<_>>();
    TestExpr::Function {
        code,
        captures: Arc::from(captures),
    }
}

fn k(value: i64) -> Key {
    Key::Number(value.into())
}

fn builtin2_expr(builtin: Builtin, left: TestExpr, right: TestExpr) -> TestExpr {
    TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(builtin))),
            Arc::new(left),
        )),
        Arc::new(right),
    )
}

fn builtin1_expr(builtin: Builtin, value: TestExpr) -> TestExpr {
    TestExpr::Apply(
        Arc::new(TestExpr::Value(Value::Builtin(builtin))),
        Arc::new(value),
    )
}

fn run_pattern_builtin(builtin: Builtin, value: Value) -> Result<Vec<Value>, EvaluationHalt> {
    let handled = eval_closed_expr(&builtin1_expr(
        Builtin::ListEffect,
        builtin1_expr(builtin, TestExpr::Value(value)),
    ))?;
    let Value::List(results) = handled else {
        panic!("the list effect handler should return a list");
    };
    list_to_value_items(&test_context(), &results)
}

fn run_pattern_builtin2(
    builtin: Builtin,
    first: Value,
    second: Value,
) -> Result<Vec<Value>, EvaluationHalt> {
    let handled = eval_closed_expr(&builtin1_expr(
        Builtin::ListEffect,
        builtin2_expr(builtin, TestExpr::Value(first), TestExpr::Value(second)),
    ))?;
    let Value::List(results) = handled else {
        panic!("the list effect handler should return a list");
    };
    list_to_value_items(&test_context(), &results)
}

fn run_pattern_equal(expected: Value, value: Value) -> Result<Vec<Value>, EvaluationHalt> {
    run_pattern_builtin2(Builtin::PatternEqual, expected, value)
}

fn builtin3_expr(builtin: Builtin, first: TestExpr, second: TestExpr, third: TestExpr) -> TestExpr {
    TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Apply(
                Arc::new(TestExpr::Value(Value::Builtin(builtin))),
                Arc::new(first),
            )),
            Arc::new(second),
        )),
        Arc::new(third),
    )
}

fn singleton_expr(key: Value, value: TestExpr) -> TestExpr {
    builtin2_expr(Builtin::DictSingleton, TestExpr::Value(key), value)
}

fn dict_union_expr(left: TestExpr, right: TestExpr) -> TestExpr {
    builtin2_expr(Builtin::DictUnion, left, right)
}

fn dict_update_expr(path: TestExpr, new_value: TestExpr, dict: TestExpr) -> TestExpr {
    builtin3_expr(Builtin::DictUpdate, path, new_value, dict)
}

fn global_access(path: Vec<TestKey>) -> TestExpr {
    TestExpr::Access(Arc::new(TestExpr::Local(0)), Arc::from(path))
}

fn key_value(key: &Key) -> Value {
    match key {
        Key::Atom(atom) => Value::Atom(*atom),
        Key::Number(number) => Value::Number(number.clone()),
        Key::Binary(bytes) => Value::Binary(bytes.clone()),
        Key::AbstractGlobalPath(parts) => Value::Atom(crate::core::Atom::from_key(
            &Key::AbstractGlobalPath(parts.clone()),
        )),
        Key::List(items) => Value::List(List::from_values(items.iter().map(key_value).collect())),
        Key::Dict(entries) => Value::Dict(
            entries
                .iter()
                .fold(crate::core::Dict::new_sync(), |dict, (key, value)| {
                    dict.insert(key.clone(), key_value(value))
                }),
        ),
    }
}

fn key_path_expr(path: Vec<Key>) -> TestExpr {
    TestExpr::Value(Value::List(List::from_values(
        path.iter().map(key_value).collect(),
    )))
}

fn module_value_expr(value: &Value) -> TestExpr {
    match value {
        Value::Dict(dict) => {
            let mut items = dict.iter();
            let Some((first_key, first_value)) = items.next() else {
                return TestExpr::Value(Value::Dict(crate::core::Dict::new_sync()));
            };

            let mut expr = singleton_expr(key_value(first_key), module_value_expr(first_value));
            for (key, value) in items {
                expr = dict_union_expr(
                    expr,
                    singleton_expr(key_value(key), module_value_expr(value)),
                );
            }
            expr
        }
        _ => TestExpr::Value(value.clone()),
    }
}

fn fixpoint_dict(dict: Dict) -> TestExpr {
    TestExpr::Apply(
        Arc::new(TestExpr::Value(Value::Builtin(Builtin::Fixpoint))),
        Arc::new(function_expr(1, module_value_expr(&Value::Dict(dict)))),
    )
}

fn apply_rooted_fixture(root: &Value, expr: TestExpr) -> Value {
    apply_values(
        &test_context(),
        closed_function_value(1, expr),
        vec![root.clone()],
    )
    .expect("rooted test expression should lower to a callable function")
}

#[test]
fn evaluates_recursive_dictionary_net() {
    let asm = Dict::new_sync().insert(
        crate::core::Key::atom_from_text("result"),
        Value::binary_from_text("Hello, World!"),
    );
    let root = Dict::new_sync().insert(crate::core::Key::atom_from_text("asm"), Value::Dict(asm));

    let value = eval_closed_expr(&fixpoint_dict(root)).expect("term should evaluate");
    let asm = value
        .get_atom_path(&[crate::core::Atom::from_key(
            &crate::core::Key::binary_from_text("asm"),
        )])
        .expect("asm should exist");
    let asm = eval_value(&test_context(), asm)
        .expect("asm binding should evaluate lazily to a dictionary");
    let Value::Dict(asm) = asm else {
        panic!("asm should evaluate to a dictionary");
    };

    assert!(matches!(value, Value::Dict(_)));
    assert_eq!(
        asm.get(&crate::core::Key::atom_from_text("result")),
        Some(&Value::binary_from_text("Hello, World!"))
    );
}

#[test]
fn evaluates_binary_literals() {
    let value = eval_closed_expr(&TestExpr::Value(Value::binary_from_text("oops")))
        .expect("binary literal should evaluate");

    assert_eq!(value, Value::binary_from_text("oops"));
}

#[test]
fn appends_lists() {
    let expr = TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(Builtin::Append))),
            Arc::new(TestExpr::Value(Value::List(List::from_values(vec![
                n(1),
                n(2),
            ])))),
        )),
        Arc::new(TestExpr::Value(Value::List(List::from_values(vec![n(3)])))),
    );

    let value = eval_closed_expr(&expr).expect("append should evaluate");

    let Value::List(list) = value else {
        panic!("append should produce a list");
    };
    let mut values = Vec::new();
    list.for_each_segment(&mut |_bytes| Ok::<_, ()>(()), &mut |segment| {
        values.extend(segment.iter().cloned());
        Ok(())
    })
    .expect("should walk list");
    assert_eq!(values, vec![n(1), n(2), n(3)]);
}

#[test]
fn evaluates_mixed_list_segments() {
    let expr = TestExpr::List(Arc::from([
        Arc::new(TestExpr::Value(n(1))),
        Arc::new(TestExpr::Value(Value::binary_from_text("Hi"))),
        Arc::new(TestExpr::Value(n(2))),
        Arc::new(TestExpr::Value(Value::binary_from_text("!"))),
    ]));

    let value = eval_closed_expr(&expr).expect("list should evaluate");

    let Value::List(list) = value else {
        panic!("list expression should produce a list");
    };
    let mut saw_bytes = Vec::new();
    let mut saw_values = Vec::new();
    list.for_each_segment(
        &mut |bytes| {
            saw_bytes.push(bytes.to_vec());
            Ok::<_, ()>(())
        },
        &mut |segment| {
            saw_values.push(segment.to_vec());
            Ok(())
        },
    )
    .expect("should walk list");

    assert_eq!(
        saw_values,
        vec![
            vec![n(1)],
            vec![Value::binary_from_text("Hi")],
            vec![n(2)],
            vec![Value::binary_from_text("!")]
        ]
    );
    assert!(saw_bytes.is_empty());
}

#[test]
fn appends_list_and_binary() {
    let expr = TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(Builtin::Append))),
            Arc::new(TestExpr::Value(Value::List(List::from_values(vec![
                n(72),
                n(105),
            ])))),
        )),
        Arc::new(TestExpr::Value(Value::binary_from_text("!"))),
    );

    let value = eval_closed_expr(&expr).expect("append should evaluate");

    assert!(matches!(value, Value::List(_)));
}

#[test]
fn append_preserves_lazy_list_chunks_until_observed() {
    let expr = builtin2_expr(
        Builtin::Append,
        TestExpr::Value(Value::List(List::from_values(vec![n(72)]))),
        builtin2_expr(
            Builtin::Append,
            TestExpr::Value(Value::binary_from_text("i")),
            TestExpr::Value(Value::binary_from_text("!")),
        ),
    );

    let value = eval_closed_expr(&expr).expect("append should evaluate lazily");

    let Value::List(list) = value else {
        panic!("append should produce a list");
    };
    assert_eq!(list.known_len(), None);
    assert_eq!(
        list_output_bytes(&test_context(), &list).expect("lazy chunk should force"),
        b"Hi!"
    );
}

#[test]
fn binary_output_does_not_flatten_nested_binary_values() {
    let list = List::from_values(vec![
        Value::binary_from_text("A"),
        n(10),
        Value::binary_from_text("B"),
    ]);

    let error = list_output_bytes(&test_context(), &list)
        .expect_err("nested binary values must not be flattened during extraction");
    assert!(error.to_string().contains("byte integers"));
    assert_eq!(failure_context_items(&error), []);
}

#[test]
fn binary_output_contextualizes_only_nested_evaluation_failures() {
    let list = List::from_values(vec![Value::error(
        &crate::core::test_value_factory(),
        "byte computation failed",
    )]);

    let error = list_output_bytes(&test_context(), &list)
        .expect_err("a failed byte computation must propagate");

    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("binary_extraction")]
    );
}

#[test]
fn list_concat_explicitly_flattens_one_level() {
    let outer = Value::List(List::from_values(vec![
        Value::binary_from_text("A"),
        Value::List(List::from_bytes(Bytes::from_static(b"B"))),
    ]));
    let flattened = eval_closed_expr(&builtin1_expr(Builtin::ListConcat, TestExpr::Value(outer)))
        .expect("list concat should evaluate");
    let Value::List(flattened) = flattened else {
        panic!("list concat should return a list");
    };

    assert_eq!(
        list_output_bytes(&test_context(), &flattened).unwrap(),
        b"AB"
    );
}

#[test]
fn lazy_list_chunks_error_when_they_do_not_evaluate_to_lists() {
    let expr = builtin2_expr(
        Builtin::Append,
        TestExpr::Value(Value::binary_from_text("Hi")),
        builtin2_expr(Builtin::Add, TestExpr::Value(n(1)), TestExpr::Value(n(1))),
    );

    let value = eval_closed_expr(&expr).expect("append should preserve lazy chunk");
    let Value::List(list) = value else {
        panic!("append should produce a list");
    };

    let err = list_output_bytes(&test_context(), &list)
        .expect_err("bad lazy chunk should fail when observed");
    assert!(
        err.to_string()
            .contains("lazy list chunk must evaluate to a list or binary value")
    );
}

#[test]
fn promised_list_chunks_remain_assignable_after_early_observation() {
    let promise = PromisedValue::new(&crate::core::test_value_factory(), "promised list tail");
    let list = append_sequence(Value::Promised(promise.clone()))
        .expect("a promise remains a valid deferred list tail");

    assert!(
        list_output_bytes(&test_context(), &list)
            .expect_err("an empty list promise should fail fast")
            .to_string()
            .contains("promised value was observed before initialization")
    );
    promise
        .set(Value::Binary(Bytes::from_static(b"assigned")))
        .expect("early observation must not fill the promise");
    assert_eq!(
        list_output_bytes(&test_context(), &list).expect("assigned list promise should resolve"),
        b"assigned"
    );
}

#[test]
fn split_end_does_not_force_lazy_left_branch_when_suffix_is_in_right_branch() {
    let lazy_left = List::from_thunk(
        LazyValue::error(&crate::core::test_value_factory(), "left branch was forced").into(),
    );
    let list = List::concat(lazy_left, List::from_bytes(Bytes::from_static(b"abc")));
    let split = eval_closed_expr(&builtin2_expr(
        Builtin::ListSplitEnd,
        TestExpr::Value(n(1)),
        TestExpr::Value(Value::List(list)),
    ))
    .expect("split_end should not force left branch");

    let Value::Dict(split) = split else {
        panic!("split_end should produce a dictionary");
    };
    let Value::List(suffix) = split
        .get(&Key::atom_from_text("right"))
        .expect("split should include right suffix")
    else {
        panic!("right suffix should be a list");
    };
    assert_eq!(
        list_output_bytes(&test_context(), suffix).expect("right suffix should render"),
        b"c"
    );
}

#[test]
fn evaluates_arithmetic_builtins() {
    let expr = builtin2_expr(
        Builtin::Subtract,
        builtin2_expr(
            Builtin::Add,
            TestExpr::Value(n(1)),
            builtin2_expr(
                Builtin::Multiply,
                TestExpr::Value(n(2)),
                TestExpr::Value(n(3)),
            ),
        ),
        builtin2_expr(
            Builtin::Divide,
            TestExpr::Value(n(4)),
            TestExpr::Value(n(5)),
        ),
    );

    let value = eval_closed_expr(&expr).expect("arithmetic should evaluate");

    assert_eq!(value, Value::Number(Number::parse("31/5").unwrap()));
}

#[test]
fn lazy_arguments_share_forced_values() {
    let force_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = force_count.clone();
    let counted = TestExpr::Value(Value::deferred(
        &crate::core::test_value_factory(),
        "counted",
        move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(2))
        },
    ));
    let expr = TestExpr::Apply(
        Arc::new(function_expr(
            1,
            builtin2_expr(Builtin::Add, TestExpr::Local(0), TestExpr::Local(0)),
        )),
        Arc::new(counted),
    );

    let value = eval_closed_expr(&expr).expect("lambda body should evaluate");

    assert_eq!(value, n(4));
    assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn equality_errors_when_dictionary_comparison_reaches_functions() {
    let function = closed_function_value(1, TestExpr::Local(0));
    let left = Value::Dict(Dict::new_sync().insert(Key::atom_from_text("f"), function.clone()));
    let right = Value::Dict(Dict::new_sync().insert(Key::atom_from_text("f"), function));
    let err = eval_closed_expr(&builtin2_expr(
        Builtin::Equal,
        TestExpr::Value(left),
        TestExpr::Value(right),
    ))
    .expect_err("function-valued fields should not be equatable");

    assert!(err.to_string().contains("cannot compare function values"));
}

#[test]
fn ordinary_observers_do_not_unseal_metadata_carriers() {
    let carrier = initial_metadata();

    for builtin in [Builtin::Equal, Builtin::NotEqual, Builtin::Greater] {
        let error = eval_closed_expr(&builtin2_expr(
            builtin,
            TestExpr::Value(carrier.clone()),
            TestExpr::Value(carrier.clone()),
        ))
        .expect_err("comparison must not expose sealed carrier identity");
        assert!(
            error.to_string().contains("cannot compare sealed values"),
            "{error}"
        );
    }

    assert!(
        run_pattern_equal(unit_value(), carrier.clone())
            .expect("a sealed carrier should be an ordinary pattern mismatch")
            .is_empty()
    );
    for builtin in [Builtin::PatternIsList, Builtin::PatternIsDict] {
        assert!(
            run_pattern_builtin(builtin, carrier.clone())
                .expect("sealed carriers should mismatch ordinary shape patterns")
                .is_empty()
        );
    }

    let unit_error = eval_closed_expr(&builtin3_expr(
        Builtin::AssertUnit,
        TestExpr::Value(Value::binary_from_text("sealed result")),
        TestExpr::Value(carrier.clone()),
        TestExpr::Value(n(42)),
    ))
    .expect_err("a sealed unit carrier must not satisfy a unit assertion");
    assert_eq!(
        unit_error.to_string(),
        "sealed result: unit expected, received Sealed"
    );

    let application_error = apply_value(&test_context(), carrier.clone(), n(0))
        .expect_err("a sealed unit carrier must not be callable");
    assert_eq!(
        application_error.to_string(),
        "application requires a function value, received Sealed"
    );
    let Err(net_call_error) = lower_core_callable(&test_context(), carrier.clone()) else {
        panic!("an interaction-net call must not unseal metadata");
    };
    assert_eq!(
        net_call_error.to_string(),
        "application requires a function value, received Sealed"
    );

    let key_error = value_to_key(&test_context(), &carrier)
        .expect_err("a sealed unit carrier must not become a dictionary key");
    assert_eq!(
        key_error.to_string(),
        "dictionary keys must evaluate to keyable values"
    );
}

#[test]
fn binary_validation_does_not_disclose_sealed_metadata() {
    let hidden = Value::metadata_carrier(Value::binary_from_text("private trace"));
    let list = List::from_values(vec![hidden]);

    let error =
        list_output_bytes(&test_context(), &list).expect_err("sealed values are not binary bytes");
    assert!(error.to_string().contains("got Sealed(..)"), "{error}");
    assert!(!error.to_string().contains("private trace"), "{error}");
}

#[test]
fn evaluates_extended_math_builtins() {
    let floor = eval_closed_expr(&builtin1_expr(
        Builtin::Floor,
        TestExpr::Value(Value::Number(Number::parse("_7/2").unwrap())),
    ))
    .expect("floor should evaluate");
    let modulus = eval_closed_expr(&builtin2_expr(
        Builtin::Mod,
        TestExpr::Value(Value::Number(Number::parse("17/5").unwrap())),
        TestExpr::Value(Value::Number(Number::parse("3/2").unwrap())),
    ))
    .expect("mod should evaluate");

    assert_eq!(floor, Value::Number((-4).into()));
    assert_eq!(modulus, Value::Number(Number::parse("2/5").unwrap()));
}

#[test]
fn evaluates_slice_and_map_builtins() {
    let slice = eval_closed_expr(&builtin3_expr(
        Builtin::Slice,
        TestExpr::Value(n(1)),
        TestExpr::Value(n(4)),
        TestExpr::Value(Value::binary_from_text("World!")),
    ))
    .expect("slice should evaluate");
    let mapped = eval_closed_expr(&builtin2_expr(
        Builtin::Map,
        function_expr(
            1,
            TestExpr::Apply(
                Arc::new(TestExpr::Apply(
                    Arc::new(TestExpr::Value(Value::Builtin(Builtin::Add))),
                    Arc::new(TestExpr::Local(0)),
                )),
                Arc::new(TestExpr::Value(n(1))),
            ),
        ),
        TestExpr::Value(Value::List(List::from_values(vec![n(1), n(2), n(3)]))),
    ))
    .expect("map should evaluate");
    let binary_len = eval_closed_expr(&builtin1_expr(
        Builtin::ListLen,
        TestExpr::Value(Value::binary_from_text("World!")),
    ))
    .expect("binary len should evaluate");
    let list_len = eval_closed_expr(&builtin1_expr(
        Builtin::ListLen,
        TestExpr::Value(Value::List(List::concat(
            List::from_values(vec![n(1), n(2)]),
            List::from_bytes(Bytes::from_static(b"Hi")),
        ))),
    ))
    .expect("list len should evaluate");

    assert_eq!(slice, Value::binary_from_text("orl"));
    let Value::List(mapped) = mapped else {
        panic!("map should produce a list");
    };
    let items = list_to_value_items(&test_context(), &mapped)
        .expect("mapped list should be readable")
        .iter()
        .map(|value| eval_value(&test_context(), value))
        .collect::<Result<Vec<_>, _>>()
        .expect("mapped values should evaluate");
    assert_eq!(items, vec![n(2), n(3), n(4)]);
    assert_eq!(binary_len, n(6));
    assert_eq!(list_len, n(4));
}

#[test]
fn evaluates_zero_based_list_at_for_lists_and_compact_binaries() {
    let binary_item = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(n(1)),
        TestExpr::Value(Value::binary_from_text("ABC")),
    ))
    .expect("list at should index compact binary data");
    let mixed_item = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(n(2)),
        TestExpr::Value(Value::List(List::concat(
            List::from_values(vec![n(10)]),
            List::from_bytes(Bytes::from_static(b"AB")),
        ))),
    ))
    .expect("list at should index mixed list segments");

    assert_eq!(binary_item, n(i64::from(b'B')));
    assert_eq!(mixed_item, n(i64::from(b'B')));

    let out_of_bounds = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(n(3)),
        TestExpr::Value(Value::binary_from_text("ABC")),
    ))
    .expect_err("list at should reject an index at the list length");
    assert_eq!(
        out_of_bounds.to_string(),
        "list at builtin index is out of bounds"
    );

    let negative = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(n(-1)),
        TestExpr::Value(Value::binary_from_text("ABC")),
    ))
    .expect_err("list at should reject negative indices");
    assert_eq!(
        negative.to_string(),
        "list at builtin requires non-negative integer indices"
    );
}

#[test]
fn compiler_pattern_list_predicates_return_pass_fail_effects() {
    for value in [
        Value::binary_from_text(""),
        Value::binary_from_text("A"),
        Value::List(List::empty()),
        Value::List(List::from_values(vec![n(1)])),
        Value::List(List::from_thunk(
            LazyValue::error(
                &crate::core::test_value_factory(),
                "list predicate forced its contents",
            )
            .into(),
        )),
    ] {
        assert_eq!(
            run_pattern_builtin(Builtin::PatternIsList, value)
                .expect("logical list values should match"),
            [unit_value()]
        );
    }
    assert!(
        run_pattern_builtin(Builtin::PatternIsList, n(1))
            .expect("a kind mismatch should be an ordinary failure")
            .is_empty()
    );

    for value in [Value::binary_from_text(""), Value::List(List::empty())] {
        assert_eq!(
            run_pattern_builtin(Builtin::PatternListIsEmpty, value)
                .expect("empty logical lists should match"),
            [unit_value()]
        );
    }
    for value in [
        Value::binary_from_text("A"),
        Value::List(List::from_values(vec![n(1)])),
        n(1),
    ] {
        assert!(
            run_pattern_builtin(Builtin::PatternListIsEmpty, value)
                .expect("nonempty and wrong-kind values should mismatch")
                .is_empty()
        );
    }
}

#[test]
fn compiler_pattern_equality_mismatches_incompatible_values() {
    let atom = key_value(&Key::atom_from_text("tag"));
    for (expected, actual) in [
        (unit_value(), unit_value()),
        (n(42), n(42)),
        (atom.clone(), atom),
        (
            Value::binary_from_text("AB"),
            Value::List(List::from_values(vec![n(65), n(66)])),
        ),
    ] {
        assert_eq!(
            run_pattern_equal(expected, actual).expect("matching literals should succeed"),
            [unit_value()]
        );
    }

    for (expected, actual) in [
        (n(42), Value::binary_from_text("42")),
        (Value::binary_from_text("AB"), n(42)),
        (
            Value::binary_from_text("AB"),
            Value::List(List::from_values(vec![n(65), Value::Builtin(Builtin::Add)])),
        ),
    ] {
        assert!(
            run_pattern_equal(expected, actual)
                .expect("literal kind and value mismatches should be ordinary failure")
                .is_empty()
        );
    }
    assert_eq!(
        run_pattern_equal(
            n(1),
            Value::error(&crate::core::test_value_factory(), "literal input failed")
        )
        .expect_err("forcing failures must propagate")
        .to_string(),
        "literal input failed"
    );
}

#[test]
fn compiler_pattern_path_equality_matches_keyable_lists_directionally() {
    let foo = key_value(&Key::atom_from_text("foo"));
    let expected = Value::List(List::from_values(vec![foo.clone(), n(42)]));
    assert_eq!(
        run_pattern_builtin2(
            Builtin::PatternPathEqual,
            expected.clone(),
            Value::List(List::from_values(vec![foo.clone(), n(42)])),
        )
        .expect("equal computed paths should match"),
        [unit_value()]
    );
    for actual in [
        Value::List(List::from_values(vec![foo, n(43)])),
        Value::List(List::from_values(vec![Value::Builtin(Builtin::Add)])),
        n(42),
    ] {
        assert!(
            run_pattern_builtin2(Builtin::PatternPathEqual, expected.clone(), actual)
                .expect("a different or non-keyable subject path should mismatch")
                .is_empty()
        );
    }
    assert_eq!(
        run_pattern_builtin2(
            Builtin::PatternPathEqual,
            Value::List(List::from_values(vec![Value::Builtin(Builtin::Add)])),
            Value::List(List::empty()),
        )
        .expect_err("an invalid computed expected path should remain an error")
        .to_string(),
        "dictionary keys must evaluate to keyable values"
    );
    assert_eq!(
        run_pattern_builtin2(
            Builtin::PatternPathEqual,
            expected,
            Value::List(List::from_values(vec![Value::error(
                &crate::core::test_value_factory(),
                "quoted path value failed"
            )])),
        )
        .expect_err("forcing failures in the subject path must propagate")
        .to_string(),
        "quoted path value failed"
    );
}

#[test]
fn compiler_pattern_dictionary_operations_preserve_remainders() {
    let foo = Key::atom_from_text("foo");
    let bar = Key::atom_from_text("bar");
    let keep = Key::atom_from_text("keep");
    let other = Key::atom_from_text("other");
    let child = Dict::new_sync()
        .insert(bar.clone(), n(7))
        .insert(keep.clone(), n(8));
    let dict = Dict::new_sync()
        .insert(foo.clone(), Value::Dict(child))
        .insert(other.clone(), n(9));
    let path = Value::List(List::from_values(vec![key_value(&foo), key_value(&bar)]));

    let [parts]: [Value; 1] =
        run_pattern_builtin2(Builtin::PatternDictTryTake, path, Value::Dict(dict))
            .expect("a present static dictionary path should match")
            .try_into()
            .expect("successful extraction should return one parts value");
    let Value::Dict(parts) = parts else {
        panic!("dictionary extraction should return a parts dictionary");
    };
    assert_eq!(parts.get(&*keys::VALUE), Some(&n(7)));
    let Some(Value::Dict(rest)) = parts.get(&*keys::REST) else {
        panic!("dictionary extraction should retain a dictionary remainder");
    };
    assert_eq!(rest.get(&other), Some(&n(9)));
    let Some(Value::Dict(child)) = rest.get(&foo) else {
        panic!("the nonempty nested remainder should retain its parent path");
    };
    assert_eq!(child.get(&keep), Some(&n(8)));
    assert!(!child.contains_key(&bar));
}

#[test]
fn compiler_pattern_dictionary_mismatches_are_pass_fail() {
    let key = Key::atom_from_text("key");
    let path = Value::List(List::from_values(vec![key_value(&key)]));
    for value in [
        Value::Dict(Dict::new_sync()),
        Value::Dict(Dict::new_sync().insert(key.clone(), Value::Dict(Dict::new_sync()))),
        n(1),
    ] {
        assert!(
            run_pattern_builtin2(Builtin::PatternDictTryTake, path.clone(), value)
                .expect("missing, undefined, and wrong-kind paths should mismatch")
                .is_empty()
        );
    }

    assert_eq!(
        run_pattern_builtin(Builtin::PatternIsDict, Value::Dict(Dict::new_sync()))
            .expect("dictionary values should pass the kind check"),
        [unit_value()]
    );
    assert!(
        run_pattern_builtin(Builtin::PatternIsDict, n(1))
            .expect("wrong-kind values should mismatch")
            .is_empty()
    );

    let logically_empty = Dict::new_sync().insert(
        key,
        Value::Lazy(LazyValue::deferred(
            &crate::core::test_value_factory(),
            "empty dictionary field",
            |_| Ok(Value::Dict(Dict::new_sync())),
        )),
    );
    assert_eq!(
        run_pattern_builtin(Builtin::PatternDictIsEmpty, Value::Dict(logically_empty))
            .expect("nested and deferred undefined values should be logically empty"),
        [unit_value()]
    );
    assert!(
        run_pattern_builtin(
            Builtin::PatternDictIsEmpty,
            Value::Dict(Dict::new_sync().insert(Key::atom_from_text("present"), n(1))),
        )
        .expect("a present dictionary value should mismatch empty")
        .is_empty()
    );
    assert_eq!(
        run_pattern_builtin(
            Builtin::PatternDictIsEmpty,
            Value::Dict(Dict::new_sync().insert(
                Key::atom_from_text("broken"),
                Value::error(&crate::core::test_value_factory(), "dict value failed")
            ),),
        )
        .expect_err("forcing failures while establishing emptiness must propagate")
        .to_string(),
        "dict value failed"
    );
}

#[test]
fn compiler_pattern_optional_dictionary_operations_preserve_absence_and_errors() {
    let foo = Key::atom_from_text("foo");
    let bar = Key::atom_from_text("bar");
    let keep = Key::atom_from_text("keep");
    let path = Value::List(List::from_values(vec![key_value(&foo), key_value(&bar)]));

    let absent = Dict::new_sync().insert(
        foo.clone(),
        Value::Dict(Dict::new_sync().insert(keep.clone(), n(8))),
    );
    let [parts]: [Value; 1] = run_pattern_builtin2(
        Builtin::PatternDictTryTakeOptional,
        path.clone(),
        Value::Dict(absent.clone()),
    )
    .expect("an optional absent path should succeed")
    .try_into()
    .expect("optional extraction should return one parts value");
    let Value::Dict(parts) = parts else {
        panic!("optional dictionary extraction should return a parts dictionary");
    };
    assert_eq!(
        parts.get(&*keys::VALUE),
        Some(&Value::Dict(Dict::new_sync()))
    );
    assert_eq!(parts.get(&*keys::REST), Some(&Value::Dict(absent)));

    let present = Dict::new_sync().insert(
        foo.clone(),
        Value::Dict(
            Dict::new_sync()
                .insert(bar.clone(), n(7))
                .insert(keep, n(8)),
        ),
    );
    let [parts]: [Value; 1] = run_pattern_builtin2(
        Builtin::PatternDictTryTakeOptional,
        path.clone(),
        Value::Dict(present),
    )
    .expect("an optional present path should extract normally")
    .try_into()
    .expect("optional extraction should return one parts value");
    let Value::Dict(parts) = parts else {
        panic!("optional dictionary extraction should return a parts dictionary");
    };
    assert_eq!(parts.get(&*keys::VALUE), Some(&n(7)));

    let wrong_intermediate = Value::Dict(Dict::new_sync().insert(foo.clone(), n(1)));
    assert!(
        run_pattern_builtin2(
            Builtin::PatternDictTryTakeOptional,
            path.clone(),
            wrong_intermediate,
        )
        .expect("a non-dictionary path prefix should mismatch")
        .is_empty()
    );

    let failed = Value::Dict(Dict::new_sync().insert(
        foo,
        Value::error(
            &crate::core::test_value_factory(),
            "optional dict path failed",
        ),
    ));
    assert_eq!(
        run_pattern_builtin2(Builtin::PatternDictTryTakeOptional, path, failed)
            .expect_err("forcing failures along an optional path must propagate")
            .to_string(),
        "optional dict path failed"
    );
}

#[test]
fn compiler_pattern_list_decomposition_preserves_compact_remainders() {
    let [uncons]: [Value; 1] =
        run_pattern_builtin(Builtin::PatternListTryUncons, Value::binary_from_text("AB"))
            .expect("a compact binary should uncons")
            .try_into()
            .expect("a successful match should have one result");
    let Value::Dict(uncons) = uncons else {
        panic!("uncons should return a parts dictionary");
    };
    assert_eq!(uncons.get(&*keys::HEAD), Some(&n(i64::from(b'A'))));
    assert_eq!(
        uncons.get(&*keys::TAIL),
        Some(&Value::binary_from_text("B"))
    );

    let [uncons]: [Value; 1] = run_pattern_builtin(
        Builtin::PatternListTryUncons,
        Value::List(List::from_values(vec![n(1), n(2)])),
    )
    .expect("a flat value list should uncons")
    .try_into()
    .expect("a successful match should have one result");
    let Value::Dict(uncons) = uncons else {
        panic!("uncons should return a parts dictionary");
    };
    assert_eq!(uncons.get(&*keys::HEAD), Some(&n(1)));
    let Some(Value::List(tail)) = uncons.get(&*keys::TAIL) else {
        panic!("a value-list remainder should stay a list");
    };
    assert_eq!(
        pop_list_front(&test_context(), tail)
            .expect("flat tail should be readable")
            .map(|(head, _)| head),
        Some(n(2))
    );

    let [unsnoc]: [Value; 1] =
        run_pattern_builtin(Builtin::PatternListTryUnsnoc, Value::binary_from_text("AB"))
            .expect("a compact binary should unsnoc")
            .try_into()
            .expect("a successful match should have one result");
    let Value::Dict(unsnoc) = unsnoc else {
        panic!("unsnoc should return a parts dictionary");
    };
    assert_eq!(
        unsnoc.get(&*keys::INIT),
        Some(&Value::binary_from_text("A"))
    );
    assert_eq!(unsnoc.get(&*keys::LAST), Some(&n(i64::from(b'B'))));
}

#[test]
fn compiler_pattern_list_decomposition_mismatches_without_masking_failures() {
    for builtin in [Builtin::PatternListTryUncons, Builtin::PatternListTryUnsnoc] {
        for value in [
            Value::binary_from_text(""),
            Value::List(List::empty()),
            n(1),
        ] {
            assert!(
                run_pattern_builtin(builtin, value)
                    .expect("empty and wrong-kind values should mismatch")
                    .is_empty()
            );
        }
        assert_eq!(
            run_pattern_builtin(
                builtin,
                Value::error(&crate::core::test_value_factory(), "pattern input failed")
            )
            .expect_err("forcing failures must propagate")
            .to_string(),
            "pattern input failed"
        );
    }
}

#[test]
fn compiler_pattern_unsnoc_does_not_force_an_unrelated_prefix_hole() {
    let list = List::concat(
        List::from_thunk(
            LazyValue::error(&crate::core::test_value_factory(), "prefix was forced").into(),
        ),
        List::from_values(vec![n(9)]),
    );
    let [parts]: [Value; 1] = run_pattern_builtin(Builtin::PatternListTryUnsnoc, Value::List(list))
        .expect("a known suffix should unsnoc without its prefix")
        .try_into()
        .expect("a successful match should have one result");
    let Value::Dict(parts) = parts else {
        panic!("unsnoc should return a parts dictionary");
    };
    assert_eq!(parts.get(&*keys::LAST), Some(&n(9)));
    assert!(matches!(parts.get(&*keys::INIT), Some(Value::List(_))));
}

#[test]
fn text_lines_preserves_empty_and_trailing_lines() {
    let lines = eval_closed_expr(&builtin1_expr(
        Builtin::TextLines,
        TestExpr::Value(Value::binary_from_text("first\n\nthird\n")),
    ))
    .expect("text lines should split compact binary text");
    let Value::List(lines) = lines else {
        panic!("text lines should produce a list");
    };

    assert_eq!(
        list_to_value_items(&test_context(), &lines).expect("line list should be readable"),
        vec![
            Value::binary_from_text("first"),
            Value::binary_from_text(""),
            Value::binary_from_text("third"),
            Value::binary_from_text(""),
        ]
    );
}

#[test]
fn evaluates_split_and_split_end_builtins() {
    let split = eval_closed_expr(&builtin2_expr(
        Builtin::ListSplit,
        TestExpr::Value(n(2)),
        TestExpr::Value(Value::binary_from_text("Hello")),
    ))
    .expect("split should evaluate");
    let split_end = eval_closed_expr(&builtin2_expr(
        Builtin::ListSplitEnd,
        TestExpr::Value(n(2)),
        TestExpr::Value(Value::List(List::concat(
            List::from_values(vec![n(1), n(2)]),
            List::from_bytes(Bytes::from_static(b"abc")),
        ))),
    ))
    .expect("split_end should evaluate");

    let Value::Dict(split) = split else {
        panic!("split should return a dictionary");
    };
    assert_eq!(
        split.get(&Key::atom_from_text("left")),
        Some(&Value::binary_from_text("He"))
    );
    assert_eq!(
        split.get(&Key::atom_from_text("right")),
        Some(&Value::binary_from_text("llo"))
    );

    let Value::Dict(split_end) = split_end else {
        panic!("split_end should return a dictionary");
    };
    let Value::List(prefix) = split_end
        .get(&Key::atom_from_text("left"))
        .expect("split_end should include left")
    else {
        panic!("split_end left should be a list");
    };
    let Value::List(suffix) = split_end
        .get(&Key::atom_from_text("right"))
        .expect("split_end should include right")
    else {
        panic!("split_end right should be a list");
    };

    assert_eq!(
        list_to_value_items(&test_context(), prefix).expect("prefix should be readable"),
        vec![n(1), n(2), Value::Number(Number::from_u8(b'a'))]
    );
    assert_eq!(
        list_to_value_items(&test_context(), suffix).expect("suffix should be readable"),
        vec![
            Value::Number(Number::from_u8(b'b')),
            Value::Number(Number::from_u8(b'c'))
        ]
    );
}

#[test]
fn slice_builtin_shares_binary_storage() {
    let bytes = Bytes::from_static(b"Hello");
    let slice = eval_closed_expr(&builtin3_expr(
        Builtin::Slice,
        TestExpr::Value(n(1)),
        TestExpr::Value(n(4)),
        TestExpr::Value(Value::Binary(bytes.clone())),
    ))
    .expect("slice should evaluate");

    let Value::Binary(slice) = slice else {
        panic!("binary slice should remain binary");
    };
    assert_eq!(&slice[..], b"ell");
    assert_eq!(slice.as_ptr(), bytes[1..].as_ptr());
}

#[test]
fn evaluates_function_net_application_lazily() {
    let expr = TestExpr::Apply(
        Arc::new(function_expr(1, TestExpr::Local(0))),
        Arc::new(builtin2_expr(
            Builtin::Add,
            TestExpr::Value(n(1)),
            TestExpr::Value(n(2)),
        )),
    );

    let value = eval_closed_expr(&expr).expect("lambda application should evaluate");

    assert_eq!(value, n(3));
}

#[test]
fn function_nets_capture_outer_values() {
    let invoke = function_expr(
        1,
        TestExpr::Apply(
            Arc::new(TestExpr::Local(0)),
            Arc::new(TestExpr::Value(n(0))),
        ),
    );
    let returns_outer = function_expr(1, TestExpr::Local(1));
    let outer = function_expr(
        1,
        TestExpr::Apply(Arc::new(invoke), Arc::new(returns_outer)),
    );
    let value = eval_closed_expr(&TestExpr::Apply(
        Arc::new(outer),
        Arc::new(TestExpr::Value(n(42))),
    ))
    .expect("nested functions should evaluate");

    assert_eq!(value, n(42));
}

#[test]
fn partial_builtins_share_lazy_arguments() {
    let force_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = force_count.clone();
    let argument = TestExpr::Value(Value::deferred(
        &crate::core::test_value_factory(),
        "partial argument",
        move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(40))
        },
    ));
    let make_partial = function_expr(
        1,
        TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(Builtin::Add))),
            Arc::new(TestExpr::Local(0)),
        ),
    );
    let partial = eval_closed_expr(&TestExpr::Apply(Arc::new(make_partial), Arc::new(argument)))
        .expect("a partial builtin should retain its argument lazily");

    assert!(matches!(partial, Value::PartialBuiltin(_)));
    assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        apply_value(&test_context(), partial.clone(), n(2)).unwrap(),
        n(42)
    );
    assert_eq!(apply_value(&test_context(), partial, n(3)).unwrap(), n(43));
    assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn net_list_literals_store_lazy_values_without_exporting_list_holes() {
    let force_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = force_count.clone();
    let expression = TestExpr::Apply(
        Arc::new(function_expr(
            1,
            TestExpr::List(Arc::from([Arc::new(TestExpr::Local(0))])),
        )),
        Arc::new(TestExpr::Value(Value::deferred(
            &crate::core::test_value_factory(),
            "list value",
            move |_| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(n(42))
            },
        ))),
    );
    let Value::List(list) = eval_closed_expr(&expression).unwrap() else {
        panic!("net-backed list literal should produce a list");
    };
    let Some((item, tail)) = list
        .try_pop_front(&mut |_| -> Result<_, EvaluationHalt> {
            panic!("embedded lazy value must not become a list hole")
        })
        .unwrap()
    else {
        panic!("net-backed list literal should contain its argument");
    };
    let ListItem::Value(item) = item else {
        panic!("lazy argument should remain an ordinary list value")
    };
    assert!(matches!(item, Value::Lazy(_)));
    assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(eval_value(&test_context(), &item).unwrap(), n(42));
    assert_eq!(force_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(pop_list_front(&test_context(), &tail).unwrap().is_none());
}

#[test]
fn closed_semantic_list_holes_remain_host_observable() {
    let Value::Lazy(hole) =
        Value::deferred(&crate::core::test_value_factory(), "list hole", |_| {
            Ok(Value::List(List::from_values(vec![n(42)])))
        })
    else {
        unreachable!()
    };
    let list = List::from_thunk(hole.into());

    let (value, tail) = pop_list_front(&test_context(), &list).unwrap().unwrap();
    assert_eq!(value, n(42));
    assert!(pop_list_front(&test_context(), &tail).unwrap().is_none());
}

#[test]
fn dropped_arguments_do_not_prevent_later_bindings_from_resolving() {
    let function = closed_function_value(2, TestExpr::Local(0));
    let value = eval_value(&test_context(), &apply_test_values(function, [n(1), n(42)]))
        .expect("function with dropped argument should evaluate");

    assert_eq!(value, n(42));
}

#[test]
fn method_objects_apply_via_apply_member() {
    let method = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("apply"),
        closed_function_value(
            1,
            builtin2_expr(Builtin::Add, TestExpr::Local(0), TestExpr::Value(n(1))),
        ),
    ));
    let value = eval_closed_expr(&TestExpr::Apply(
        Arc::new(TestExpr::Value(method)),
        Arc::new(TestExpr::Value(n(41))),
    ))
    .expect("method object application should evaluate");

    assert_eq!(value, n(42));
}

#[test]
fn effect_values_apply_by_extending_the_effect_function() {
    let effect = effect_value(closed_function_value(
        1,
        TestExpr::Access(
            Arc::new(TestExpr::Local(0)),
            Arc::from([TestKey::Key(Key::atom_from_text("op"))]),
        ),
    ));
    let applied = eval_closed_expr(&TestExpr::Apply(
        Arc::new(TestExpr::Value(effect)),
        Arc::new(TestExpr::Value(n(41))),
    ))
    .expect("effect application should evaluate");
    let Value::Dict(effect) = applied else {
        panic!("effect application should produce an effect value");
    };
    let function = effect
        .get(&Key::atom_from_text("eff"))
        .expect("effect should contain an eff function")
        .clone();
    let api = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("op"),
        closed_function_value(
            1,
            builtin2_expr(Builtin::Add, TestExpr::Local(0), TestExpr::Value(n(1))),
        ),
    ));

    let value = apply_value(
        &test_context(),
        eval_value(&test_context(), &function).unwrap(),
        api,
    )
    .and_then(|value| eval_value(&test_context(), &value))
    .expect("extended effect function should evaluate with an API");
    assert_eq!(value, n(42));
}

#[test]
fn effect_application_requires_singleton_eff_tag() {
    let not_singleton = Value::Dict(
        Dict::new_sync()
            .insert(
                Key::atom_from_text("eff"),
                closed_function_value(1, TestExpr::Local(0)),
            )
            .insert(Key::atom_from_text("extra"), n(1)),
    );
    let err = eval_closed_expr(&TestExpr::Apply(
        Arc::new(TestExpr::Value(not_singleton)),
        Arc::new(TestExpr::Value(n(42))),
    ))
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "application requires a function value, received Dict"
    );
}

#[test]
fn non_callable_application_reports_semantic_value_kinds() {
    for (value, expected) in [
        (
            Value::Dict(Dict::new_sync()),
            "application requires a function value, received Undefined",
        ),
        (
            unit_value(),
            "application requires a function value, received Unit",
        ),
        (
            n(42),
            "application requires a function value, received Number",
        ),
    ] {
        let error = apply_value(&test_context(), value, n(0))
            .expect_err("applying a non-callable value should fail");
        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn tagged_payload_ignores_only_semantically_undefined_extra_entries() {
    let payload = n(42);
    let lazy_empty = Value::Lazy(LazyValue::deferred(
        &crate::core::test_value_factory(),
        "empty tag field",
        |_| Ok(Value::Dict(Dict::new_sync())),
    ));
    let recursively_empty =
        Value::Dict(Dict::new_sync().insert(Key::atom_from_text("nested"), lazy_empty));
    let tagged = Dict::new_sync()
        .insert((*keys::TUPLE).clone(), payload.clone())
        .insert(Key::atom_from_text("ignored"), recursively_empty.clone());

    assert_eq!(
        tagged
            .tagged_payload(&test_context(), &keys::TUPLE)
            .unwrap(),
        Some(payload)
    );
    assert_eq!(
        tagged
            .insert(Key::atom_from_text("defined"), n(1))
            .tagged_payload(&test_context(), &keys::TUPLE)
            .unwrap(),
        None
    );
    assert_eq!(
        Dict::new_sync()
            .insert((*keys::TUPLE).clone(), recursively_empty)
            .tagged_payload(&test_context(), &keys::TUPLE)
            .unwrap(),
        None
    );
}

#[test]
fn tuple_ordering_requires_a_singleton_tuple_tag() {
    let left = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::TUPLE).clone(),
                Value::List(List::from_values(vec![n(1)])),
            )
            .insert(Key::atom_from_text("extra"), n(1)),
    );
    let right = Value::Dict(Dict::new_sync().insert(
        (*keys::TUPLE).clone(),
        Value::List(List::from_values(vec![n(2)])),
    ));

    let err = eval_closed_expr(&builtin2_expr(
        Builtin::Less,
        TestExpr::Value(left),
        TestExpr::Value(right),
    ))
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "less-than builtin can only order dictionaries tagged as `tuple`"
    );
}

#[test]
fn local_dictionary_paths_resolve_without_a_global_root() {
    let dict = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("tail"),
        Value::binary_from_text("World"),
    ));
    let expr = TestExpr::Apply(
        Arc::new(function_expr(
            1,
            TestExpr::Access(
                Arc::new(TestExpr::Local(0)),
                Arc::from([TestKey::Key(Key::atom_from_text("tail"))]),
            ),
        )),
        Arc::new(TestExpr::Value(dict)),
    );

    let value = eval_closed_expr(&expr).expect("local dictionary path should evaluate");

    assert_eq!(value, Value::binary_from_text("World"));
}

#[test]
fn divide_builtin_rejects_zero() {
    let expr = builtin2_expr(
        Builtin::Divide,
        TestExpr::Value(n(1)),
        TestExpr::Value(n(0)),
    );
    let err = eval_closed_expr(&expr).expect_err("division by zero should fail");
    assert_eq!(err.to_string(), "divide builtin cannot divide by zero");
}

#[test]
fn evaluates_keyable_values_into_keys() {
    let key = eval_key(&Value::List(List::concat(
        List::from_values(vec![n(1)]),
        List::from_bytes(Bytes::from_static(b"Hi")),
    )))
    .expect("list should evaluate to a key");

    assert_eq!(
        key,
        Key::List(Arc::from([
            k(1),
            Key::Number(Number::from_u8(b'H')),
            Key::Number(Number::from_u8(b'i')),
        ]))
    );
}

#[test]
fn evaluates_lazy_values_before_key_validation() {
    let key = eval_key(&fixture_computation(TestExpr::Value(n(1))))
        .expect("lazy values should be allowed when they evaluate to keyable values");

    assert_eq!(key, k(1));
}

#[test]
fn dictionaries_remain_lazy_under_eval_value() {
    let value = Value::Dict(crate::core::Dict::new_sync().insert(
        Key::atom_from_text("answer"),
        fixture_computation(TestExpr::Value(n(42))),
    ));

    let evaluated = eval_value(&test_context(), &value).expect("dict should stay lazy");

    assert_eq!(evaluated, value);
}

#[test]
fn missing_access_can_evaluate_to_an_undefined_key() {
    let root = Value::Dict(crate::core::Dict::new_sync());
    let key = eval_key(&apply_rooted_fixture(
        &root,
        global_access(vec![TestKey::Key(Key::atom_from_text("missing"))]),
    ))
    .expect("missing names should now resolve to empty dictionaries");

    assert_eq!(key, Key::Dict(Arc::from([])));
}

#[test]
fn raw_value_to_key_rejects_lazy_values() {
    assert_eq!(
        Key::from_value(&fixture_computation(TestExpr::Value(n(1)))),
        None
    );
}

#[test]
fn eval_key_forces_nested_dictionary_values() {
    let key = eval_key(&Value::Dict(crate::core::Dict::new_sync().insert(
        Key::atom_from_text("answer"),
        fixture_computation(TestExpr::Value(n(42))),
    )))
    .expect("dict key should force nested values");

    assert_eq!(
        key,
        Key::Dict(Arc::from([(Key::atom_from_text("answer"), k(42),)]))
    );
}

#[test]
fn eval_key_elides_empty_dictionary_values_from_dict_keys() {
    let empty = eval_key(&Value::Dict(crate::core::Dict::new_sync()))
        .expect("empty dict should be keyable");
    let with_empty_field = eval_key(&Value::Dict(crate::core::Dict::new_sync().insert(
        Key::atom_from_text("key"),
        Value::Dict(crate::core::Dict::new_sync()),
    )))
    .expect("dict with empty field should be keyable");

    assert_eq!(empty, Key::Dict(Arc::from([])));
    assert_eq!(with_empty_field, Key::Dict(Arc::from([])));
}

#[test]
fn singleton_dict_filters_empty_dictionary_values() {
    let value = eval_closed_expr(&singleton_expr(
        Value::Atom(crate::core::Atom::from_key(
            &crate::core::Key::binary_from_text("gone"),
        )),
        TestExpr::Value(Value::Dict(crate::core::Dict::new_sync())),
    ))
    .expect("singleton dict should evaluate");

    assert_eq!(value, Value::Dict(crate::core::Dict::new_sync()));
}

#[test]
fn dictionary_unions_merge_nested_dictionaries_transitively() {
    let key = Key::atom_from_text("greeting");
    let hello = Key::atom_from_text("hello");
    let world = Key::atom_from_text("world");

    let expr = dict_union_expr(
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(
                key.clone(),
                Value::Dict(
                    crate::core::Dict::new_sync()
                        .insert(hello.clone(), Value::binary_from_text("Hello")),
                ),
            ),
        )),
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(
                key.clone(),
                Value::Dict(
                    crate::core::Dict::new_sync()
                        .insert(world.clone(), Value::binary_from_text("World")),
                ),
            ),
        )),
    );

    let value = eval_closed_expr(&expr).expect("dict union should evaluate");
    let greeting = value.get_key_path(&[key]).expect("greeting should exist");
    let Value::Lazy(greeting) = greeting else {
        panic!("greeting should stay lazy until demanded");
    };
    let greeting = eval_value(&test_context(), &Value::Lazy(greeting.clone()))
        .expect("nested dict union should evaluate when demanded");
    let Value::Dict(greeting) = greeting else {
        panic!("greeting should evaluate to a merged dictionary");
    };

    assert_eq!(
        greeting.get(&hello),
        Some(&Value::binary_from_text("Hello"))
    );
    assert_eq!(
        greeting.get(&world),
        Some(&Value::binary_from_text("World"))
    );
}

#[test]
fn dictionary_unions_treat_empty_dictionary_values_as_undefined() {
    let key = Key::atom_from_text("greeting");
    let expr = dict_union_expr(
        singleton_expr(
            Value::Atom(crate::core::Atom::from_key(
                &crate::core::Key::binary_from_text("greeting"),
            )),
            TestExpr::Value(Value::binary_from_text("Hello")),
        ),
        singleton_expr(
            Value::Atom(crate::core::Atom::from_key(
                &crate::core::Key::binary_from_text("greeting"),
            )),
            TestExpr::Value(Value::Dict(crate::core::Dict::new_sync())),
        ),
    );

    let value = eval_closed_expr(&expr).expect("dict union should evaluate");
    assert_eq!(
        value.get_key_path(&[key]),
        Some(&Value::binary_from_text("Hello"))
    );
}

#[test]
fn dictionary_unions_defer_ambiguous_keys_until_observed() {
    let key = Key::atom_from_text("greeting");
    let expr = dict_union_expr(
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
        )),
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("World")),
        )),
    );

    let value = eval_closed_expr(&expr).expect("outer dict union should stay evaluable");
    let ambiguous = value
        .get_key_path(&[key])
        .expect("ambiguous key should exist");
    let Value::Lazy(ambiguous) = ambiguous else {
        panic!("ambiguous duplicate should stay as a stuck expression");
    };

    let err = eval_value(&test_context(), &Value::Lazy(ambiguous.clone()))
        .expect_err("ambiguous key should fail only when demanded");

    assert_eq!(
        err.to_string(),
        "dictionary union is ambiguous at key `greeting`"
    );
}

#[test]
fn dictionary_updates_overwrite_duplicate_values() {
    let key = Key::atom_from_text("greeting");
    let expr = dict_update_expr(
        key_path_expr(vec![key.clone()]),
        TestExpr::Value(Value::binary_from_text("World")),
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
        )),
    );

    let value = eval_closed_expr(&expr).expect("dict update should evaluate");

    assert_eq!(
        value.get_key_path(&[key]),
        Some(&Value::binary_from_text("World"))
    );
}

#[test]
fn dictionary_updates_merge_nested_dictionaries_transitively() {
    let key = Key::atom_from_text("greeting");
    let hello = Key::atom_from_text("hello");
    let world = Key::atom_from_text("world");

    let expr = dict_update_expr(
        key_path_expr(vec![key.clone(), world.clone()]),
        TestExpr::Value(Value::binary_from_text("World")),
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(
                key.clone(),
                Value::Dict(
                    crate::core::Dict::new_sync()
                        .insert(hello.clone(), Value::binary_from_text("Hello")),
                ),
            ),
        )),
    );

    let value = eval_closed_expr(&expr).expect("dict update should evaluate");
    let greeting = value.get_key_path(&[key]).expect("greeting should exist");
    let Value::Dict(greeting) = greeting else {
        panic!("greeting should resolve directly to a dictionary");
    };

    assert_eq!(
        greeting.get(&hello),
        Some(&Value::binary_from_text("Hello"))
    );
    assert_eq!(
        greeting.get(&world),
        Some(&Value::binary_from_text("World"))
    );
}

#[test]
fn dictionary_updates_treat_empty_dictionary_values_as_undefined() {
    let key = Key::atom_from_text("greeting");
    let expr = dict_update_expr(
        key_path_expr(vec![key.clone()]),
        TestExpr::Value(Value::Dict(crate::core::Dict::new_sync())),
        TestExpr::Value(Value::Dict(
            crate::core::Dict::new_sync().insert(key.clone(), Value::binary_from_text("Hello")),
        )),
    );

    let value = eval_closed_expr(&expr).expect("dict update should evaluate");
    assert_eq!(value.get_key_path(&[key]), None);
}

#[test]
fn names_can_traverse_dictionary_union_bindings() {
    let d = Key::atom_from_text("d");
    let hello = Key::atom_from_text("hello");

    let root = crate::core::Dict::new_sync().insert(
        d.clone(),
        fixture_computation(dict_union_expr(
            TestExpr::Value(Value::Dict(
                crate::core::Dict::new_sync()
                    .insert(hello.clone(), Value::binary_from_text("Hello")),
            )),
            TestExpr::Value(Value::Dict(crate::core::Dict::new_sync())),
        )),
    );

    let value = eval_closed_expr(&fixpoint_dict(root)).expect("root should evaluate");
    let resolved = eval_value(
        &test_context(),
        &apply_rooted_fixture(
            &value,
            global_access(vec![TestKey::Key(d), TestKey::Key(hello)]),
        ),
    )
    .expect("dotted name should force intermediate dict unions");

    assert_eq!(resolved, Value::binary_from_text("Hello"));
}

#[test]
fn names_can_expand_list_valued_path_segments() {
    let foo = Key::atom_from_text("foo");
    let one = k(1);
    let two = k(2);
    let three = k(3);

    let nested = Value::Dict(
        crate::core::Dict::new_sync().insert(
            one.clone(),
            Value::Dict(
                crate::core::Dict::new_sync().insert(
                    two.clone(),
                    Value::Dict(
                        crate::core::Dict::new_sync()
                            .insert(three.clone(), Value::binary_from_text("World")),
                    ),
                ),
            ),
        ),
    );

    let root = crate::core::Dict::new_sync().insert(foo.clone(), nested);
    let value = eval_closed_expr(&fixpoint_dict(root)).expect("root should evaluate");
    let resolved = eval_value(
        &test_context(),
        &apply_rooted_fixture(
            &value,
            global_access(vec![
                TestKey::Key(foo),
                TestKey::PathIndex(Arc::new(TestExpr::Apply(
                    Arc::new(TestExpr::Apply(
                        Arc::new(TestExpr::Value(Value::Builtin(Builtin::Append))),
                        Arc::new(TestExpr::List(Arc::from([
                            Arc::new(TestExpr::Value(n(1))),
                            Arc::new(TestExpr::Value(n(2))),
                        ]))),
                    )),
                    Arc::new(TestExpr::List(Arc::from([Arc::new(TestExpr::Value(n(3)))]))),
                ))),
            ]),
        ),
    )
    .expect("list-valued path segment should expand into multiple lookups");

    assert_eq!(resolved, Value::binary_from_text("World"));
}

#[test]
fn missing_dictionary_members_resolve_to_empty_dictionary() {
    let root = Value::Dict(crate::core::Dict::new_sync().insert(
        Key::atom_from_text("present"),
        Value::Dict(crate::core::Dict::new_sync()),
    ));
    let resolved = eval_value(
        &test_context(),
        &apply_rooted_fixture(
            &root,
            global_access(vec![
                TestKey::Key(Key::atom_from_text("present")),
                TestKey::Key(Key::atom_from_text("missing")),
            ]),
        ),
    )
    .expect("missing member access should stay evaluable");

    assert_eq!(resolved, Value::Dict(crate::core::Dict::new_sync()));
}

#[test]
fn anno_builtin_continues_demand_after_assertions_pass() {
    let root =
        Value::Dict(crate::core::Dict::new_sync().insert(Key::atom_from_text("later"), n(42)));
    let annotation = singleton_expr(
        Value::Atom(crate::core::Atom::from_key(
            &crate::core::Key::binary_from_text("assert_undefined"),
        )),
        dict_union_expr(
            singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("name"),
                )),
                TestExpr::Value(Value::binary_from_text("missing")),
            ),
            singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("value"),
                )),
                global_access(vec![TestKey::Key(Key::atom_from_text("missing"))]),
            ),
        ),
    );

    let value = eval_value(
        &test_context(),
        &apply_rooted_fixture(
            &root,
            TestExpr::Apply(
                Arc::new(TestExpr::Apply(
                    Arc::new(TestExpr::Value(Value::Builtin(Builtin::Anno))),
                    Arc::new(annotation),
                )),
                Arc::new(global_access(vec![TestKey::Key(Key::atom_from_text(
                    "later",
                ))])),
            ),
        ),
    )
    .expect("anno should pass through successful assertions");

    assert_eq!(value, n(42));
}

#[test]
fn anno_builtin_reports_failed_assertions_during_demand() {
    let annotation = singleton_expr(
        Value::Atom(crate::core::Atom::from_key(
            &crate::core::Key::binary_from_text("assert_defined"),
        )),
        dict_union_expr(
            singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("name"),
                )),
                TestExpr::Value(Value::binary_from_text("foo")),
            ),
            singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("value"),
                )),
                global_access(vec![TestKey::Key(Key::atom_from_text("foo"))]),
            ),
        ),
    );

    let error = eval_value(
        &test_context(),
        &apply_rooted_fixture(
            &Value::Dict(crate::core::Dict::new_sync()),
            TestExpr::Apply(
                Arc::new(TestExpr::Apply(
                    Arc::new(TestExpr::Value(Value::Builtin(Builtin::Anno))),
                    Arc::new(annotation),
                )),
                Arc::new(TestExpr::Value(n(1))),
            ),
        ),
    )
    .expect_err("failed anno should raise during demand");
    assert_eq!(
        error.to_string(),
        "cannot override `foo` because it is not defined"
    );
}

#[test]
fn assert_unit_builtin_uses_its_diagnostic_context() {
    let target = n(42);
    let value = eval_closed_expr(&builtin3_expr(
        Builtin::AssertUnit,
        TestExpr::Value(Value::binary_from_text("test operation result")),
        TestExpr::Value(unit_value()),
        TestExpr::Value(target.clone()),
    ))
    .expect("unit assertion should return its target");
    assert_eq!(value, target);

    let error = eval_closed_expr(&builtin3_expr(
        Builtin::AssertUnit,
        TestExpr::Value(Value::binary_from_text("test operation result")),
        TestExpr::Value(Value::Dict(Dict::new_sync())),
        TestExpr::Value(n(42)),
    ))
    .expect_err("non-unit assertion value should fail");
    assert_eq!(
        error.to_string(),
        "test operation result: unit expected, received Undefined"
    );
}

#[test]
fn assert_unit_annotation_has_optional_diagnostic_context() {
    let annotation = |payload| {
        singleton_expr(
            Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
                "assert_unit",
            ))),
            payload,
        )
    };
    let value_payload = || {
        singleton_expr(
            Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text("value"))),
            TestExpr::Value(n(1)),
        )
    };

    let generic_error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        annotation(value_payload()),
        TestExpr::Value(n(42)),
    ))
    .expect_err("context-free unit annotation should fail generically");
    assert_eq!(generic_error.to_string(), "unit expected, received Number");

    let contextual_payload = dict_union_expr(
        value_payload(),
        singleton_expr(
            Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
                "context",
            ))),
            TestExpr::Value(Value::binary_from_text("annotated operation result")),
        ),
    );
    let contextual_error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        annotation(contextual_payload),
        TestExpr::Value(n(42)),
    ))
    .expect_err("contextual unit annotation should fail");
    assert_eq!(
        contextual_error.to_string(),
        "annotated operation result: unit expected, received Number"
    );
}

#[test]
fn error_annotations_carry_diagnostic_values_and_ordered_contexts() {
    let atom = |name| Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(name)));
    let context_annotation = |context| {
        TestExpr::Value(Value::Dict(
            Dict::new_sync().insert((*keys::CONTEXT).clone(), context),
        ))
    };
    let message = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::MSG).clone(),
                Value::Dict(
                    Dict::new_sync()
                        .insert(
                            (*keys::TEXT).clone(),
                            Value::binary_from_text("handler failed"),
                        )
                        .insert(
                            (*keys::CONTEXT).clone(),
                            Value::List(List::from_values(vec![Value::binary_from_text(
                                "emitted",
                            )])),
                        ),
                ),
            )
            .insert(Key::atom_from_text("operation"), atom("emit")),
    );
    let failure = builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(atom("error")),
        TestExpr::Value(message),
    );
    let inner = builtin2_expr(
        Builtin::Anno,
        context_annotation(Value::binary_from_text("inner")),
        failure,
    );
    let outer = builtin2_expr(
        Builtin::Anno,
        context_annotation(Value::binary_from_text("outer")),
        inner,
    );

    let error = eval_closed_expr(&outer).expect_err("error annotation must fail when demanded");
    assert_eq!(error.to_string(), "handler failed");
    let diagnostic =
        halt_diagnostic_value(&error).expect("permanent errors must project to diagnostics");
    let Value::Dict(diagnostic) = eval_value(&test_context(), &diagnostic).unwrap() else {
        panic!("failure diagnostic must be a dictionary");
    };
    let operation = eval_value(
        &test_context(),
        diagnostic
            .get(&Key::atom_from_text("operation"))
            .expect("diagnostic should retain ad hoc fields"),
    )
    .unwrap();
    assert_eq!(operation, atom("emit"));
    let message = eval_value(
        &test_context(),
        diagnostic
            .get(&*keys::MSG)
            .expect("diagnostic should define msg"),
    )
    .unwrap();
    let Value::Dict(message) = message else {
        panic!("diagnostic msg must be a dictionary");
    };
    let contexts = eval_value(
        &test_context(),
        message
            .get(&*keys::CONTEXT)
            .expect("context annotation should define msg.context"),
    )
    .unwrap();
    let Value::List(contexts) = contexts else {
        panic!("msg.context must be a list");
    };
    assert_eq!(
        list_to_value_items(&test_context(), &contexts).unwrap(),
        [
            Value::binary_from_text("outer"),
            Value::binary_from_text("inner"),
            Value::binary_from_text("emitted")
        ]
    );
}

#[test]
fn error_annotations_contextualize_failure_while_evaluating_their_message() {
    let error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("error"),
        ))),
        TestExpr::Value(Value::error(
            &crate::core::test_value_factory(),
            "message construction failed",
        )),
    ))
    .expect_err("failure while constructing an error message must propagate");
    assert_eq!(error.to_string(), "message construction failed");

    let diagnostic =
        halt_diagnostic_value(&error).expect("message-construction failure must remain permanent");
    let Value::Dict(diagnostic) = eval_value(&test_context(), &diagnostic).unwrap() else {
        panic!("message-construction failure must project to a diagnostic");
    };
    let message = eval_value(
        &test_context(),
        diagnostic
            .get(&*keys::MSG)
            .expect("diagnostic should define msg"),
    )
    .unwrap();
    let Value::Dict(message) = message else {
        panic!("diagnostic msg must be a dictionary");
    };
    let contexts = eval_value(
        &test_context(),
        message
            .get(&*keys::CONTEXT)
            .expect("message-construction failure should define msg.context"),
    )
    .unwrap();
    let Value::List(contexts) = contexts else {
        panic!("msg.context must be a list");
    };
    assert_eq!(
        list_to_value_items(&test_context(), &contexts).unwrap(),
        [evaluation_context_frame("error_message")]
    );
}

#[test]
fn annotation_selection_contextualizes_only_nested_evaluation_failures() {
    let error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::error(
            &crate::core::test_value_factory(),
            "annotation selection failed",
        )),
        TestExpr::Value(n(42)),
    ))
    .expect_err("failure while selecting an annotation must propagate");
    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("annotation")]
    );
}

#[test]
fn index_builtins_contextualize_demand_without_decorating_validation_errors() {
    let values = Value::List(List::from_values(vec![n(42)]));
    let nested = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(Value::error(
            &crate::core::test_value_factory(),
            "index computation failed",
        )),
        TestExpr::Value(values.clone()),
    ))
    .expect_err("failure while evaluating the index must propagate");
    assert_eq!(
        failure_context_items(&nested),
        [evaluation_context_frame("list_index")]
    );

    let validation = eval_closed_expr(&builtin2_expr(
        Builtin::ListAt,
        TestExpr::Value(Value::binary_from_text("not an index")),
        TestExpr::Value(values),
    ))
    .expect_err("a nonnumeric index must fail validation");
    assert_eq!(failure_context_items(&validation), []);
}

fn failure_context_items(error: &EvaluationHalt) -> Vec<Value> {
    let diagnostic = halt_diagnostic_value(error).expect("test error should be permanent");
    let Value::Dict(diagnostic) = eval_value(&test_context(), &diagnostic).unwrap() else {
        panic!("failure diagnostic must be a dictionary");
    };
    let message = eval_value(
        &test_context(),
        diagnostic
            .get(&*keys::MSG)
            .expect("diagnostic should define msg"),
    )
    .unwrap();
    let Value::Dict(message) = message else {
        panic!("diagnostic msg must be a dictionary");
    };
    let contexts = eval_value(
        &test_context(),
        message
            .get(&*keys::CONTEXT)
            .expect("diagnostic should define msg.context"),
    )
    .unwrap();
    let Value::List(contexts) = contexts else {
        panic!("msg.context must be a list");
    };
    list_to_value_items(&test_context(), &contexts).unwrap()
}

#[test]
fn context_annotations_are_transparent_and_do_not_demand_context_on_success() {
    let annotation = TestExpr::Value(Value::Dict(Dict::new_sync().insert(
        (*keys::CONTEXT).clone(),
        Value::error(
            &crate::core::test_value_factory(),
            "unused context must remain lazy",
        ),
    )));
    let value = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        annotation,
        TestExpr::Value(n(42)),
    ))
    .expect("successful context annotation should return its target");
    assert_eq!(value, n(42));
}

#[test]
fn metadata_annotation_initializes_the_canonical_sealed_carrier() {
    let context = test_context();
    let annotation = || {
        Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
            "meta_init",
        )))
    };
    let target_forces = Arc::new(AtomicUsize::new(0));
    let counted_target_forces = target_forces.clone();
    let unit = Value::deferred(
        &crate::core::test_value_factory(),
        "metadata annotation unit",
        move |_| {
            counted_target_forces.fetch_add(1, Ordering::SeqCst);
            Ok(unit_value())
        },
    );

    let first = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![annotation(), unit],
    )
    .expect("metadata annotation should accept demanded canonical unit");
    let second = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![annotation(), unit_value()],
    )
    .expect("metadata annotation should reuse its canonical carrier");

    assert_eq!(target_forces.load(Ordering::SeqCst), 1);
    assert_eq!(first, initial_metadata());
    assert_eq!(second, initial_metadata());
    assert_eq!(first, second);
    assert_eq!(
        first.associated_metadata(),
        Some(Value::Dict(Dict::new_sync()))
    );

    let seq_result = apply_values(
        &context,
        Value::Builtin(Builtin::Seq),
        vec![first.clone(), n(42)],
    )
    .expect("initial metadata should already satisfy shallow sequencing");
    assert_eq!(eval_value(&context, &seq_result).unwrap(), n(42));

    let spark_result = apply_values(&context, Value::Builtin(Builtin::Spark), vec![first, n(43)])
        .expect("initial metadata should be safe to spark");
    assert_eq!(eval_value(&context, &spark_result).unwrap(), n(43));
}

#[test]
fn metadata_annotation_rejects_non_unit_and_existing_carriers() {
    let context = test_context();
    let annotation = || {
        Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
            "meta_init",
        )))
    };

    for (target, expected_kind) in [
        (n(42), "Number"),
        (Value::Dict(Dict::new_sync()), "Undefined"),
        (initial_metadata(), "Sealed"),
    ] {
        let error = apply_values(
            &context,
            Value::Builtin(Builtin::Anno),
            vec![annotation(), target],
        )
        .expect_err("metadata initialization must require canonical unit");
        assert_eq!(
            error.to_string(),
            format!("unit expected, received {expected_kind}")
        );
    }
}

#[test]
fn old_metadata_annotation_spellings_are_unrecognized() {
    let context = test_context();
    let old_initial = Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text("meta")));
    assert_eq!(
        apply_values(
            &context,
            Value::Builtin(Builtin::Anno),
            vec![old_initial, n(42)],
        )
        .expect("an unrecognized annotation should preserve its target"),
        n(42),
        "the old initializer must not create a sealed carrier"
    );

    let carrier = Value::metadata_carrier(n(7));
    let old_update = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("meta_upd"),
        Value::error(
            &crate::core::test_value_factory(),
            "the old update function must remain unused",
        ),
    ));
    let target = Value::List(List::from_values(vec![carrier.clone()]));
    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![old_update, target.clone()],
    )
    .expect("an unrecognized annotation should preserve its target");
    assert_eq!(result, target);
    let Value::List(result) = result else {
        panic!("the preserved target must remain a list");
    };
    assert_eq!(
        list_to_value_items(&context, &result).unwrap(),
        vec![carrier],
        "the old updater must not derive another carrier"
    );
}

fn run_metadata_update(
    context: &EvalContext,
    function: Value,
    carriers: Vec<Value>,
) -> Result<Vec<Value>, EvaluationHalt> {
    run_metadata_transform(context, "meta_pure", function, carriers)
}

fn run_metadata_reflection_update(
    context: &EvalContext,
    function: Value,
    carriers: Vec<Value>,
) -> Result<Vec<Value>, EvaluationHalt> {
    run_metadata_transform(context, "meta_refl", function, carriers)
}

fn run_metadata_transform(
    context: &EvalContext,
    annotation_name: &str,
    function: Value,
    carriers: Vec<Value>,
) -> Result<Vec<Value>, EvaluationHalt> {
    let annotation =
        Value::Dict(Dict::new_sync().insert(Key::atom_from_text(annotation_name), function));
    let result = apply_values(
        context,
        Value::Builtin(Builtin::Anno),
        vec![annotation, Value::List(List::from_values(carriers))],
    )?;
    let Value::List(result) = result else {
        panic!("metadata update should return a list");
    };
    list_to_value_items(context, &result)
}

fn metadata_reorder_function(indices: &[usize]) -> Value {
    let projections = indices
        .iter()
        .map(|index| {
            Arc::new(builtin2_expr(
                Builtin::ListAt,
                TestExpr::Value(Value::Number(Number::from_usize(*index))),
                TestExpr::Local(0),
            ))
        })
        .collect::<Vec<_>>();
    closed_function_value(1, TestExpr::List(Arc::from(projections)))
}

fn evaluated_metadata(context: &EvalContext, carrier: &Value) -> Result<Value, EvaluationHalt> {
    let metadata = carrier
        .associated_metadata()
        .expect("metadata update output must remain sealed");
    eval_value(context, &metadata)
}

#[test]
fn metadata_update_reorders_copies_and_clears_hidden_values() {
    let context = test_context();
    let left = Value::metadata_carrier(n(1));
    let right = Value::metadata_carrier(n(2));

    let swapped = run_metadata_update(
        &context,
        metadata_reorder_function(&[1, 0]),
        vec![left.clone(), right.clone()],
    )
    .expect("metadata update should support permutation");
    assert_eq!(
        swapped
            .iter()
            .map(|carrier| evaluated_metadata(&context, carrier))
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        vec![n(2), n(1)]
    );

    let copied = run_metadata_update(
        &context,
        metadata_reorder_function(&[0, 0]),
        vec![left, right],
    )
    .expect("metadata update should support copying");
    assert_eq!(
        copied
            .iter()
            .map(|carrier| evaluated_metadata(&context, carrier))
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        vec![n(1), n(1)]
    );

    let cleared = run_metadata_update(
        &context,
        closed_function_value(
            1,
            TestExpr::List(Arc::from([
                Arc::new(builtin2_expr(
                    Builtin::Add,
                    builtin2_expr(Builtin::ListAt, TestExpr::Value(n(0)), TestExpr::Local(0)),
                    builtin2_expr(Builtin::ListAt, TestExpr::Value(n(1)), TestExpr::Local(0)),
                )),
                Arc::new(TestExpr::Value(Value::Dict(Dict::new_sync()))),
            ])),
        ),
        vec![Value::metadata_carrier(n(1)), Value::metadata_carrier(n(2))],
    )
    .expect("metadata update should permit merging and clearing");
    assert_eq!(
        cleared
            .iter()
            .map(|carrier| evaluated_metadata(&context, carrier))
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        vec![n(3), Value::Dict(Dict::new_sync())]
    );
}

#[test]
fn metadata_update_preserves_input_arity_without_validating_output_length() {
    let context = test_context();
    let empty = run_metadata_update(
        &context,
        Value::error(&crate::core::test_value_factory(), "unused update"),
        Vec::new(),
    )
    .expect("an empty carrier list should not demand its update function");
    assert!(empty.is_empty());

    let too_short = run_metadata_update(
        &context,
        closed_function_value(
            1,
            TestExpr::Value(Value::List(List::from_values(vec![n(7)]))),
        ),
        vec![initial_metadata(), initial_metadata()],
    )
    .expect("a short update list should remain latent inside output carriers");
    assert_eq!(too_short.len(), 2);
    let missing_error = evaluated_metadata(&context, &too_short[1])
        .expect_err("only the missing projection should fail");
    assert_eq!(
        missing_error.to_string(),
        "list at builtin index is out of bounds"
    );
    assert_eq!(
        evaluated_metadata(&context, &too_short[0]).unwrap(),
        n(7),
        "a failed later projection must not poison an earlier valid one"
    );

    let extra_forces = Arc::new(AtomicUsize::new(0));
    let counted_extra_forces = extra_forces.clone();
    let extra = Value::deferred(
        &crate::core::test_value_factory(),
        "unused metadata update result",
        move |_| {
            counted_extra_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(9))
        },
    );
    let too_long = run_metadata_update(
        &context,
        closed_function_value(
            1,
            TestExpr::Value(Value::List(List::from_values(vec![n(8), extra]))),
        ),
        vec![initial_metadata()],
    )
    .expect("extra update values should be ignored");
    assert_eq!(too_long.len(), 1);
    assert_eq!(evaluated_metadata(&context, &too_long[0]).unwrap(), n(8));
    assert_eq!(
        extra_forces.load(Ordering::SeqCst),
        0,
        "an unused extra update value must remain lazy"
    );
}

#[test]
fn metadata_update_validates_inputs_strictly_but_not_hidden_metadata() {
    let context = test_context();
    let carrier_forces = Arc::new(AtomicUsize::new(0));
    let counted_carrier_forces = carrier_forces.clone();
    let hidden_forces = Arc::new(AtomicUsize::new(0));
    let counted_hidden_forces = hidden_forces.clone();
    let hidden = Value::deferred(
        &crate::core::test_value_factory(),
        "hidden metadata",
        move |_| {
            counted_hidden_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(11))
        },
    );
    let carrier = Value::metadata_carrier(hidden);
    let lazy_carrier = Value::deferred(
        &crate::core::test_value_factory(),
        "lazy metadata carrier",
        move |_| {
            counted_carrier_forces.fetch_add(1, Ordering::SeqCst);
            Ok(carrier.clone())
        },
    );

    let result = run_metadata_update(
        &context,
        metadata_reorder_function(&[0]),
        vec![lazy_carrier],
    )
    .expect("lazy carrier shells should be demanded during input validation");
    assert_eq!(carrier_forces.load(Ordering::SeqCst), 1);
    assert_eq!(
        hidden_forces.load(Ordering::SeqCst),
        0,
        "input validation must not demand associated metadata"
    );
    assert_eq!(evaluated_metadata(&context, &result[0]).unwrap(), n(11));
    assert_eq!(hidden_forces.load(Ordering::SeqCst), 1);

    let error = run_metadata_update(
        &context,
        Value::error(
            &crate::core::test_value_factory(),
            "update function must remain unused",
        ),
        vec![n(1)],
    )
    .expect_err("ordinary input values must be rejected before update evaluation");
    assert_eq!(
        error.to_string(),
        "`meta_pure` annotation item 0 must be a sealed metadata carrier, received Number"
    );

    let annotation = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("meta_pure"),
        Value::error(
            &crate::core::test_value_factory(),
            "update function must remain unused",
        ),
    ));
    let error = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![annotation, n(1)],
    )
    .expect_err("metadata update target must be a list");
    assert_eq!(
        error.to_string(),
        "`meta_pure` annotation requires a list of sealed metadata carriers"
    );
}

#[test]
fn metadata_update_shares_update_failures_between_projections() {
    let context = test_context();
    let update_forces = Arc::new(AtomicUsize::new(0));
    let counted_update_forces = update_forces.clone();
    let function = Value::deferred(
        &crate::core::test_value_factory(),
        "failing metadata update",
        move |_| {
            counted_update_forces.fetch_add(1, Ordering::SeqCst);
            Err(EvaluationHalt::new("shared metadata update failed"))
        },
    );
    let result = run_metadata_update(
        &context,
        function,
        vec![initial_metadata(), initial_metadata()],
    )
    .expect("update failure should remain latent inside output carriers");
    assert_eq!(update_forces.load(Ordering::SeqCst), 0);

    for carrier in &result {
        let error =
            evaluated_metadata(&context, carrier).expect_err("every shared projection must fail");
        assert_eq!(error.to_string(), "shared metadata update failed");
    }
    assert_eq!(
        update_forces.load(Ordering::SeqCst),
        1,
        "all projections must share one update application"
    );
}

#[test]
fn metadata_update_delegates_output_interpretation_to_list_at() {
    let context = test_context();
    let binary = run_metadata_update(
        &context,
        closed_function_value(1, TestExpr::Value(Value::binary_from_text("x"))),
        vec![initial_metadata()],
    )
    .expect("binary update output should remain indexable");
    assert_eq!(
        evaluated_metadata(&context, &binary[0]).unwrap(),
        n(i64::from(b'x'))
    );

    let number = run_metadata_update(
        &context,
        closed_function_value(1, TestExpr::Value(n(42))),
        vec![initial_metadata()],
    )
    .expect("an unindexable update result should remain latent");
    let error =
        evaluated_metadata(&context, &number[0]).expect_err("the indexed projection must fail");
    assert_eq!(
        error.to_string(),
        "list at builtin requires a list or binary value"
    );
    assert_eq!(
        error.into_permanent_failure().contexts(),
        [evaluation_context_frame("wrap_metadata")]
    );
}

#[test]
fn metadata_reflection_update_is_inert_until_demand_and_shares_one_task() {
    let context = test_context();
    let builds = Arc::new(AtomicUsize::new(0));
    let result_policies = Arc::new(Mutex::new(Vec::new()));
    context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![
                n(2),
                n(1),
            ]))),
            builds: builds.clone(),
            result_policies: result_policies.clone(),
        }))
        .expect("fresh test session should accept its reflection launcher");

    let outputs = run_metadata_reflection_update(
        &context,
        Value::error(
            &crate::core::test_value_factory(),
            "the fixture launcher must not evaluate the effect",
        ),
        vec![Value::metadata_carrier(n(1)), Value::metadata_carrier(n(2))],
    )
    .expect("effectful metadata update should construct its output carriers");
    let copied_first = outputs[0].clone();
    assert_eq!(
        builds.load(Ordering::SeqCst),
        0,
        "constructing, copying, and transporting carriers must not launch their task"
    );

    assert_eq!(evaluated_metadata(&context, &outputs[1]).unwrap(), n(1));
    assert_eq!(evaluated_metadata(&context, &outputs[0]).unwrap(), n(2));
    assert_eq!(evaluated_metadata(&context, &copied_first).unwrap(), n(2));
    assert_eq!(
        builds.load(Ordering::SeqCst),
        1,
        "all projections and carrier copies must share one reflection task"
    );
    assert_eq!(
        *result_policies
            .lock()
            .expect("fixture result policies were poisoned"),
        [ReflectionTaskResultPolicy::ReturnValue]
    );
}

#[test]
fn metadata_reflection_update_blocks_and_resumes_on_its_shared_task() {
    let context = test_context();
    let outputs = run_metadata_reflection_update(
        &context,
        Value::error(&crate::core::test_value_factory(), "unlaunched effect"),
        vec![initial_metadata()],
    )
    .expect("effectful metadata update should remain latent");

    let blocked = evaluated_metadata(&context, &outputs[0])
        .expect_err("an unlaunched metadata task should block");
    let wait = blocked
        .blocked_on()
        .expect("the metadata projection should expose its task wait");
    context.complete_wait_with_value(&wait.0, Value::List(List::from_values(vec![n(42)])));
    assert_eq!(evaluated_metadata(&context, &outputs[0]).unwrap(), n(42));
}

#[test]
fn metadata_reflection_update_propagates_task_failure_and_cancellation() {
    let failure = Arc::new(
        EvaluationFailure::message("metadata reflection task failed")
            .with_context(evaluation_context_frame("metadata_producer")),
    );
    let failed_context = test_context();
    failed_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Failed(failure),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let failed = run_metadata_reflection_update(&failed_context, n(0), vec![initial_metadata()])
        .expect("task failure should remain latent in its output carrier");
    let error = evaluated_metadata(&failed_context, &failed[0])
        .expect_err("demanding failed effectful metadata must propagate its failure");
    assert_eq!(error.to_string(), "metadata reflection task failed");
    assert_eq!(
        failed_context
            .task_registry_counts()
            .unacknowledged_failures,
        0,
        "the demanding metadata projection owns reporting responsibility"
    );

    let cancelled_context = test_context();
    cancelled_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Cancelled,
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let cancelled =
        run_metadata_reflection_update(&cancelled_context, n(0), vec![initial_metadata()])
            .expect("task cancellation should remain latent in its output carrier");
    let error = evaluated_metadata(&cancelled_context, &cancelled[0])
        .expect_err("demanding cancelled effectful metadata must fail");
    assert_eq!(error.to_string(), "reflection result task was cancelled");
}

#[test]
fn metadata_reflection_update_preserves_projection_semantics_and_input_validation() {
    let short_context = test_context();
    short_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![n(7)]))),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let short = run_metadata_reflection_update(
        &short_context,
        n(0),
        vec![initial_metadata(), initial_metadata()],
    )
    .expect("a short result should remain latent");
    assert_eq!(evaluated_metadata(&short_context, &short[0]).unwrap(), n(7));
    assert_eq!(
        evaluated_metadata(&short_context, &short[1])
            .expect_err("the missing projection should fail")
            .to_string(),
        "list at builtin index is out of bounds"
    );

    let long_context = test_context();
    let unused_extra = Value::deferred(
        &crate::core::test_value_factory(),
        "unused effectful metadata result",
        |_| panic!("an extra metadata result must remain unused"),
    );
    long_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![
                n(8),
                unused_extra,
            ]))),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let long = run_metadata_reflection_update(&long_context, n(0), vec![initial_metadata()])
        .expect("an extra result should be ignored");
    assert_eq!(evaluated_metadata(&long_context, &long[0]).unwrap(), n(8));

    let non_list_context = test_context();
    non_list_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(n(42)),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let non_list =
        run_metadata_reflection_update(&non_list_context, n(0), vec![initial_metadata()])
            .expect("an unindexable result should remain latent");
    assert_eq!(
        evaluated_metadata(&non_list_context, &non_list[0])
            .expect_err("the projection should delegate its error to list.at")
            .to_string(),
        "list at builtin requires a list or binary value"
    );

    let partial_context = test_context();
    partial_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![
                Value::error(
                    &crate::core::test_value_factory(),
                    "one metadata projection failed",
                ),
                n(9),
            ]))),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let partial = run_metadata_reflection_update(
        &partial_context,
        n(0),
        vec![initial_metadata(), initial_metadata()],
    )
    .expect("individual failed results should remain latent");
    assert_eq!(
        evaluated_metadata(&partial_context, &partial[0])
            .expect_err("the first metadata result should fail")
            .to_string(),
        "one metadata projection failed"
    );
    assert_eq!(
        evaluated_metadata(&partial_context, &partial[1]).unwrap(),
        n(9),
        "one failed result must not poison a sibling projection"
    );

    let invalid_context = test_context();
    let error = run_metadata_reflection_update(&invalid_context, n(0), vec![n(1)])
        .expect_err("ordinary values must be rejected before task launch");
    assert_eq!(
        error.to_string(),
        "`meta_refl` annotation item 0 must be a sealed metadata carrier, received Number"
    );
    let error = run_metadata_reflection_update(&invalid_context, n(0), Vec::new())
        .expect("an empty carrier list should construct no projections");
    assert!(error.is_empty());
    assert_eq!(invalid_context.reflection_task_count(), 0);

    let annotation = Value::Dict(Dict::new_sync().insert(Key::atom_from_text("meta_refl"), n(0)));
    let error = apply_values(
        &invalid_context,
        Value::Builtin(Builtin::Anno),
        vec![annotation, n(1)],
    )
    .expect_err("effectful metadata target must be a list");
    assert_eq!(
        error.to_string(),
        "`meta_refl` annotation requires a list of sealed metadata carriers"
    );
}

#[test]
fn metadata_reflection_update_is_demanded_by_seq_and_worker_spark() {
    let seq_context = test_context();
    let seq_builds = Arc::new(AtomicUsize::new(0));
    seq_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![n(7)]))),
            builds: seq_builds.clone(),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let seq_outputs =
        run_metadata_reflection_update(&seq_context, n(0), vec![initial_metadata()]).unwrap();
    assert_eq!(
        apply_values(
            &seq_context,
            Value::Builtin(Builtin::Seq),
            vec![seq_outputs[0].clone(), n(42)],
        )
        .unwrap(),
        n(42)
    );
    assert_eq!(seq_builds.load(Ordering::SeqCst), 1);

    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let spark_context = EvalContext::new(session);
    let spark_builds = Arc::new(AtomicUsize::new(0));
    spark_context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(Value::List(List::from_values(vec![n(8)]))),
            builds: spark_builds.clone(),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let spark_outputs =
        run_metadata_reflection_update(&spark_context, n(0), vec![initial_metadata()]).unwrap();
    let result = apply_values(
        &spark_context,
        Value::Builtin(Builtin::Spark),
        vec![spark_outputs[0].clone(), n(43)],
    )
    .expect("spark should immediately return its target");
    assert_eq!(result, n(43));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while spark_builds.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(
        spark_builds.load(Ordering::SeqCst),
        1,
        "a worker spark should demand the hidden reflection task"
    );
}

#[test]
fn list_annotations_rebalance_and_flatten_lists() {
    let deque = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("deque"),
        ))),
        TestExpr::Value(Value::List(List::concat(
            List::from_bytes(Bytes::from_static(b"Hello")),
            List::from_values(vec![n(33)]),
        ))),
    ))
    .expect("deque annotation should evaluate");
    let Value::List(deque) = deque else {
        panic!("deque annotation should produce a list");
    };
    assert_eq!(deque.len(), 6);

    let binary = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("binary"),
        ))),
        TestExpr::Value(Value::List(List::concat(
            List::from_values(vec![n(72), n(105)]),
            List::from_bytes(Bytes::from_static(b"!")),
        ))),
    ))
    .expect("binary annotation should evaluate");
    assert_eq!(binary, Value::binary_from_text("Hi!"));

    let array = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("array"),
        ))),
        TestExpr::Value(Value::binary_from_text("Hi")),
    ))
    .expect("array annotation should evaluate");
    let Value::List(array) = array else {
        panic!("array annotation should produce a list");
    };
    assert_eq!(
        list_to_value_items(&test_context(), &array).unwrap(),
        vec![n(72), n(105)]
    );
}

#[test]
fn list_annotations_report_errors_for_wrong_targets() {
    let error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("binary"),
        ))),
        TestExpr::Value(Value::List(List::from_values(vec![n(300)]))),
    ))
    .expect_err("invalid binary annotation should fail during demand");

    assert_eq!(
        error.to_string(),
        "`binary` annotation cannot encode number `300` as a byte"
    );

    let error = eval_closed_expr(&builtin2_expr(
        Builtin::Anno,
        TestExpr::Value(Value::Atom(crate::core::Atom::from_key(
            &Key::binary_from_text("deque"),
        ))),
        TestExpr::Value(n(1)),
    ))
    .expect_err("invalid deque annotation should fail during demand");

    assert!(
        error
            .to_string()
            .contains("`deque` annotation requires a list target")
    );
}

#[test]
fn unknown_annotations_pass_through_targets() {
    let value = eval_closed_expr(&TestExpr::Apply(
        Arc::new(TestExpr::Apply(
            Arc::new(TestExpr::Value(Value::Builtin(Builtin::Anno))),
            Arc::new(singleton_expr(
                Value::Atom(crate::core::Atom::from_key(
                    &crate::core::Key::binary_from_text("mystery"),
                )),
                TestExpr::Value(n(0)),
            )),
        )),
        Arc::new(TestExpr::Value(n(42))),
    ))
    .expect("unknown annotations should pass through");

    assert_eq!(value, n(42));
}

fn reflection_annotation(context: &EvalContext, effect: Value, target: Value) -> Value {
    let annotation = Value::Dict(Dict::new_sync().insert(Key::atom_from_text("refl"), effect));
    apply_builtin(context, Builtin::Anno, vec![annotation], target)
        .expect("reflection annotation should construct a lazy gate")
}

#[test]
fn reflection_task_result_returns_arbitrary_lazy_value_once() {
    let context = test_context();
    let result_forces = Arc::new(AtomicUsize::new(0));
    let counted_result_forces = result_forces.clone();
    let result = Value::deferred(
        &crate::core::test_value_factory(),
        "returned reflection result",
        move |_| {
            counted_result_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(42))
        },
    );
    let builds = Arc::new(AtomicUsize::new(0));
    let result_policies = Arc::new(Mutex::new(Vec::new()));
    context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Complete(result),
            builds: builds.clone(),
            result_policies: result_policies.clone(),
        }))
        .expect("fresh test session should accept its reflection launcher");

    let computation = Value::reflection_task_result(&crate::core::test_value_factory(), n(0));
    let copy = computation.clone();
    assert_eq!(eval_value(&context, &computation).unwrap(), n(42));
    assert_eq!(eval_value(&context, &copy).unwrap(), n(42));
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    assert_eq!(result_forces.load(Ordering::SeqCst), 1);
    assert_eq!(
        *result_policies
            .lock()
            .expect("fixture result policies were poisoned"),
        [ReflectionTaskResultPolicy::ReturnValue]
    );
}

#[test]
fn reflection_task_result_blocks_across_sessions_and_returns_completion_value() {
    let owner = test_context();
    let observer = test_context();
    let computation = Value::reflection_task_result(&crate::core::test_value_factory(), n(0));
    let blocked = eval_value(&owner, &computation)
        .expect_err("an unlaunched reflection result task should block");
    let wait = blocked
        .blocked_on()
        .expect("the result computation should expose its stable wait");

    let cross_session = eval_value(&observer, &computation)
        .expect_err("a cross-session observer should follow the owner task");
    assert!(cross_session.blocked_on().is_some());

    owner.complete_wait_with_value(&wait.0, n(43));
    assert_eq!(eval_value(&observer, &computation).unwrap(), n(43));
    assert_eq!(eval_value(&owner, &computation).unwrap(), n(43));
}

#[test]
fn reflection_task_result_preserves_failure_and_transfers_reporting_responsibility() {
    let context = test_context();
    let producer_frame = evaluation_context_frame("reflection_result_producer");
    let failure = Arc::new(
        EvaluationFailure::message("reflection result failed").with_context(producer_frame.clone()),
    );
    context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Failed(failure),
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");

    let error = eval_value(
        &context,
        &Value::reflection_task_result(&crate::core::test_value_factory(), n(0)),
    )
    .expect_err("a failed result task must fail its lazy consumer");
    assert_eq!(error.to_string(), "reflection result failed");
    assert_eq!(
        failure_context_items(&error),
        [evaluation_context_frame("reflection_task"), producer_frame,]
    );
    assert_eq!(
        context.task_registry_counts().unacknowledged_failures,
        0,
        "propagating the failure must remove detached-task reporting responsibility"
    );
}

#[test]
fn reflection_task_result_propagates_cancellation() {
    let context = test_context();
    context
        .install_reflection_launcher(Arc::new(FixtureTaskLauncher {
            terminal: FixtureTaskTerminal::Cancelled,
            builds: Arc::new(AtomicUsize::new(0)),
            result_policies: Arc::new(Mutex::new(Vec::new())),
        }))
        .expect("fresh test session should accept its reflection launcher");

    let error = eval_value(
        &context,
        &Value::reflection_task_result(&crate::core::test_value_factory(), n(0)),
    )
    .expect_err("a cancelled result task must fail its lazy consumer");
    assert_eq!(error.to_string(), "reflection result task was cancelled");
}

#[test]
fn reflection_gate_waits_before_continuing_target_demand() {
    let context = test_context();
    let forced = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let forced_by_target = forced.clone();
    let target = Value::deferred(
        &crate::core::test_value_factory(),
        "reflection target",
        move |_| {
            forced_by_target.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(42))
        },
    );
    let gate = reflection_annotation(&context, n(0), target.clone());

    assert_eq!(context.reflection_task_count(), 0);
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 0);

    let first = eval_value(&context, &gate).expect_err("new reflection task should block");
    let wait = first
        .blocked_on()
        .expect("gate should report its task wait");
    let second = eval_value(&context, &gate).expect_err("queued reflection task should block");

    assert_eq!(second.blocked_on(), Some(wait.clone()));
    assert_eq!(context.reflection_task_count(), 1);
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 0);

    context.complete_wait(&wait.0);
    assert_eq!(
        eval_value(&context, &gate).expect("completed gate should continue target demand"),
        n(42)
    );
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(eval_value(&context, &gate).unwrap(), n(42));
    assert_eq!(forced.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn running_reflection_gate_blocks_an_observer_session_without_poisoning_its_cache() {
    let owner = test_context();
    let observer = test_context();
    let gate = reflection_annotation(&owner, n(0), n(42));
    let Value::Lazy(gate_lazy) = &gate else {
        panic!("reflection annotation should produce a lazy gate")
    };
    let blocked = eval_value(&owner, &gate).expect_err("new reflection task should block");

    let cross_session =
        eval_value(&observer, &gate).expect_err("cross-session gate task should block");
    assert!(cross_session.blocked_on().is_some());
    assert_eq!(
        gate_lazy.cached(),
        None,
        "a live cross-session dependency must not become a permanent lazy failure"
    );

    owner.complete_wait(&blocked.blocked_on().unwrap().0);
    assert_eq!(eval_value(&observer, &gate).unwrap(), n(42));
}

#[test]
fn reflection_gate_memoizes_task_failure() {
    let context = test_context();
    let gate = reflection_annotation(&context, n(0), n(42));
    let blocked = eval_value(&context, &gate).expect_err("new reflection task should block");
    let wait = blocked
        .blocked_on()
        .expect("gate should report its task wait");

    context.fail_wait(&wait.0, "reflection task failed deliberately");

    let first = eval_value(&context, &gate).unwrap_err();
    assert_eq!(first.to_string(), "reflection task failed deliberately");
    assert_eq!(
        failure_context_items(&first),
        [evaluation_context_frame("reflection_annotation")]
    );
    let second = eval_value(&context, &gate).unwrap_err();
    assert_eq!(second.to_string(), "reflection task failed deliberately");
    assert_eq!(
        failure_context_items(&second),
        [evaluation_context_frame("reflection_annotation")]
    );
}

fn assert_structured_reflection_gate_failure(stage: GateFailureStage) {
    let context = test_context();
    let detail = Key::atom_from_text("detail");
    let emission = Value::Dict(
        Dict::new_sync()
            .insert(
                (*keys::MSG).clone(),
                Value::Dict(Dict::new_sync().insert(
                    (*keys::TEXT).clone(),
                    Value::binary_from_text("structured gate failure"),
                )),
            )
            .insert(detail.clone(), n(7)),
    );
    let producer_frame = evaluation_context_frame("gate_producer");
    let failure = Arc::new(
        EvaluationFailure::emission(emission.clone()).with_context(producer_frame.clone()),
    );
    let builds = Arc::new(AtomicUsize::new(0));
    context
        .install_reflection_launcher(Arc::new(GateFailureLauncher {
            failure,
            stage,
            builds: builds.clone(),
        }))
        .expect("fresh test session should accept its reflection launcher");
    let gate = reflection_annotation(&context, n(0), n(42));
    let Value::Lazy(gate_lazy) = &gate else {
        panic!("reflection annotation should produce a lazy gate")
    };

    let first = eval_value(&context, &gate)
        .expect_err("the reflection gate should retain its structured task failure")
        .into_permanent_failure();
    assert_eq!(first.emission_value(), Some(&emission));
    assert_eq!(
        first.contexts(),
        [
            evaluation_context_frame("reflection_annotation"),
            producer_frame,
        ]
    );
    let cached = gate_lazy
        .cached()
        .expect("the failed gate should have a terminal lazy cache")
        .expect_err("the terminal gate cache should contain its failure");
    assert!(Arc::ptr_eq(&first, &cached));

    let second = eval_value(&context, &gate)
        .expect_err("the reflection gate should reuse its cached failure")
        .into_permanent_failure();
    assert!(Arc::ptr_eq(&cached, &second));
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    let Value::Dict(diagnostic) = failure_diagnostic_value(&second) else {
        panic!("structured gate failure should project to a diagnostic dictionary")
    };
    assert_eq!(diagnostic.get(&detail), Some(&n(7)));
    assert_eq!(
        context.task_registry_counts().unacknowledged_failures,
        0,
        "a propagated gate failure must not remain a detached task failure"
    );
}

#[test]
fn reflection_gate_preserves_structured_launcher_construction_failure() {
    assert_structured_reflection_gate_failure(GateFailureStage::LauncherConstruction);
}

#[test]
fn reflection_gate_preserves_structured_post_launch_failure() {
    assert_structured_reflection_gate_failure(GateFailureStage::TaskPoll);
}

#[test]
fn reflection_gate_blocks_and_resumes_the_exact_net_call() {
    let context = test_context();
    let identity = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        builder.wire(argument, result);
        application
    });
    let gate = reflection_annotation(&context, n(0), Value::Net(identity));
    let applied = closed_net(|builder| {
        let [application, argument, result] = builder.bind();
        let function = builder.data(gate);
        let value = builder.data(n(42));
        builder.wire(application, function);
        builder.wire(argument, value);
        result
    });
    let runtime = applied.runtime().clone();

    let computation = Value::Lazy(LazyValue::from_net_computation(
        &crate::core::test_value_factory(),
        applied,
    ));
    let blocked =
        eval_value(&context, &computation).expect_err("call should wait for its reflection gate");
    let wait = blocked
        .blocked_on()
        .expect("call should report a task wait");
    assert_eq!(runtime.with(|net| net.blocked_calls().count()), 1);

    context.complete_wait(&wait.0);
    let observer = test_context();
    let resumed = Value::Lazy(LazyValue::from_net_computation(
        &crate::core::test_value_factory(),
        NetValue::new(runtime),
    ));
    assert_eq!(eval_value(&observer, &resumed).unwrap(), n(42));
}

#[test]
fn builtins_are_curried_and_do_not_force_arguments_early() {
    let unforced = Value::deferred(
        &crate::core::test_value_factory(),
        "unforced builtin argument",
        |_| panic!("partial builtin application forced its first argument"),
    );
    let partial = apply_values(
        &test_context(),
        Value::Builtin(Builtin::Append),
        vec![unforced],
    )
    .expect("partial builtin application should accept its first argument");

    match partial {
        Value::PartialBuiltin(call) => {
            assert_eq!(call.builtin, Builtin::Append);
            assert_eq!(call.arguments.len(), 1);
            assert!(matches!(&call.arguments[0], Value::Lazy(_)));
        }
        other => panic!("expected partial builtin, got {other:?}"),
    }
}

#[test]
fn seq_forces_its_first_argument_before_continuing_target_demand() {
    let context = test_context();
    let error = apply_values(
        &context,
        Value::Builtin(Builtin::Seq),
        vec![
            Value::error(&crate::core::test_value_factory(), "seq forced this error"),
            n(42),
        ],
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "seq forced this error");

    let target_forces = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted_target_forces = target_forces.clone();
    let target = Value::deferred(
        &crate::core::test_value_factory(),
        "seq target",
        move |_| {
            counted_target_forces.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(n(42))
        },
    );
    let result = apply_values(&context, Value::Builtin(Builtin::Seq), vec![n(0), target]).unwrap();

    assert_eq!(target_forces.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(eval_value(&context, &result).unwrap(), n(42));
    assert_eq!(target_forces.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn zero_worker_spark_returns_target_without_forcing_work() {
    let context = test_context();
    let unforced = Value::deferred(
        &crate::core::test_value_factory(),
        "discarded spark",
        |_| panic!("zero-worker spark should be silently dropped"),
    );
    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Spark),
        vec![unforced, n(42)],
    )
    .unwrap();

    assert_eq!(eval_value(&context, &result).unwrap(), n(42));
}

#[test]
fn strategies_demand_hidden_metadata_without_exposing_the_carrier() {
    let context = test_context();
    let metadata_forces = Arc::new(AtomicUsize::new(0));
    let counted_metadata_forces = metadata_forces.clone();
    let metadata = Value::deferred(
        &crate::core::test_value_factory(),
        "sequenced metadata",
        move |_| {
            counted_metadata_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(7))
        },
    );
    let carrier = Value::metadata_carrier(metadata);
    let target_forces = Arc::new(AtomicUsize::new(0));
    let counted_target_forces = target_forces.clone();
    let target = Value::deferred(
        &crate::core::test_value_factory(),
        "metadata sequence target",
        move |_| {
            counted_target_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(42))
        },
    );

    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Seq),
        vec![carrier, target],
    )
    .expect("seq should successfully demand hidden metadata");
    assert_eq!(metadata_forces.load(Ordering::SeqCst), 1);
    assert_eq!(target_forces.load(Ordering::SeqCst), 0);
    assert_eq!(eval_value(&context, &result).unwrap(), n(42));
    assert_eq!(target_forces.load(Ordering::SeqCst), 1);
}

#[test]
fn zero_worker_spark_discards_hidden_metadata_demand() {
    let context = test_context();
    let metadata = Value::deferred(
        &crate::core::test_value_factory(),
        "discarded metadata spark",
        |_| panic!("zero-worker spark must not demand hidden metadata"),
    );
    let carrier = Value::metadata_carrier(metadata);
    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Spark),
        vec![carrier, n(42)],
    )
    .expect("spark should return its target with no workers");

    assert_eq!(eval_value(&context, &result).unwrap(), n(42));
}

fn wait_for_lazy_cache(lazy: &LazyValue, message: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while lazy.cached().is_none() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(lazy.cached().is_some(), "{message}");
}

fn wait_for_no_deferred_tasks(context: &EvalContext, message: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while context.task_registry_counts().deferred_active != 0
        && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert_eq!(
        context.task_registry_counts().deferred_active,
        0,
        "{message}"
    );
}

fn wait_for_blocked_sparks(
    coordinator: &crate::evaluation::EvaluationWorkCoordinator,
    expected: usize,
    message: &str,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while coordinator.spark_work_counts().2 != expected && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(coordinator.spark_work_counts().2, expected, "{message}");
}

#[test]
fn worker_spark_demands_metadata_behind_a_lazy_carrier_shell() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let context = EvalContext::new(session);
    let (shell_sender, shell_receiver) = std::sync::mpsc::channel();
    let (metadata_sender, metadata_receiver) = std::sync::mpsc::channel();
    let metadata = Value::deferred(
        &crate::core::test_value_factory(),
        "worker metadata",
        move |_| {
            metadata_sender
                .send(())
                .expect("metadata receiver should remain open");
            Ok(n(7))
        },
    );
    let carrier = Value::metadata_carrier(metadata);
    let lazy_carrier = Value::deferred(
        &crate::core::test_value_factory(),
        "lazy worker metadata carrier",
        move |_| {
            shell_sender
                .send(())
                .expect("carrier receiver should remain open");
            Ok(carrier.clone())
        },
    );

    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Spark),
        vec![lazy_carrier, n(42)],
    )
    .expect("spark should immediately return its target");
    assert_eq!(result, n(42));
    shell_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker should demand the carrier shell");
    metadata_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker should continue into the hidden metadata");
}

#[test]
fn metadata_strategy_failures_are_cached_and_seq_propagates_them() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let context = EvalContext::new(session);
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted_attempts = attempts.clone();
    let (attempt_sender, attempt_receiver) = std::sync::mpsc::channel();
    let metadata = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "failing metadata strategy",
        move |_| {
            counted_attempts.fetch_add(1, Ordering::SeqCst);
            attempt_sender
                .send(())
                .expect("attempt receiver should remain open");
            Err(EvaluationHalt::new("metadata strategy failed"))
        },
    );
    let carrier = Value::metadata_carrier(Value::Lazy(metadata.clone()));

    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Spark),
        vec![carrier.clone(), n(42)],
    )
    .expect("detached metadata failure must not replace the spark target");
    assert_eq!(result, n(42));
    attempt_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker should demand the failing metadata");
    wait_for_lazy_cache(
        &metadata,
        "the worker must cache the terminal metadata failure",
    );

    let error = apply_values(&context, Value::Builtin(Builtin::Seq), vec![carrier, n(43)])
        .expect_err("seq must propagate the cached hidden failure");
    assert_eq!(error.to_string(), "metadata strategy failed");
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn strategies_stop_at_nested_metadata_carriers() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let context = EvalContext::new(session);
    let hidden_forces = Arc::new(AtomicUsize::new(0));
    let counted_hidden_forces = hidden_forces.clone();
    let hidden = Value::deferred(
        &crate::core::test_value_factory(),
        "nested hidden metadata",
        move |_| {
            counted_hidden_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(7))
        },
    );
    let outer = Value::metadata_carrier(Value::metadata_carrier(hidden));

    assert_eq!(
        apply_values(
            &context,
            Value::Builtin(Builtin::Seq),
            vec![outer.clone(), n(42)],
        )
        .expect("seq should stop after demanding one hidden metadata value"),
        n(42)
    );
    assert_eq!(hidden_forces.load(Ordering::SeqCst), 0);

    context.spark(outer);
    let (finished_sender, finished_receiver) = std::sync::mpsc::channel();
    let sentinel = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "nested metadata spark sentinel",
        move |_| {
            finished_sender
                .send(())
                .expect("sentinel receiver should remain open");
            Ok(unit_value())
        },
    );
    context.spark(Value::Lazy(sentinel.clone()));
    finished_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker should finish the preceding metadata spark");
    wait_for_lazy_cache(&sentinel, "worker must finish the sentinel spark");
    assert_eq!(
        hidden_forces.load(Ordering::SeqCst),
        0,
        "spark must share seq's single metadata-boundary demand"
    );
}

#[test]
fn spark_admission_drops_whnf_and_follows_completed_promises() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let context = EvalContext::new(session);
    let net = closed_net(|builder| builder.data(n(1)));
    context.spark(Value::Net(net));
    context.spark(Value::Promised(PromisedValue::new(
        &crate::core::test_value_factory(),
        "unassigned spark input",
    )));
    let promised_forces = Arc::new(AtomicUsize::new(0));
    let counted_promised_forces = promised_forces.clone();
    let promised_work = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "promised spark work",
        move |_| {
            counted_promised_forces.fetch_add(1, Ordering::SeqCst);
            Ok(n(7))
        },
    );
    let promise = PromisedValue::new(&crate::core::test_value_factory(), "resolved spark input");
    promise
        .set(Value::Lazy(promised_work.clone()))
        .expect("test promise should accept its one assignment");
    context.spark(Value::Promised(promise));

    let (finished_sender, finished_receiver) = std::sync::mpsc::channel();
    let sentinel = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "spark admission sentinel",
        move |_| {
            finished_sender
                .send(())
                .expect("sentinel receiver should remain open");
            Ok(unit_value())
        },
    );
    context.spark(Value::Lazy(sentinel.clone()));
    finished_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("worker should process the earlier spark jobs first");
    wait_for_lazy_cache(&sentinel, "worker must finish the sentinel spark");
    wait_for_no_deferred_tasks(
        &context,
        "completed spark jobs must retire their deferred records",
    );

    let counts = context.task_registry_counts();
    assert_eq!(
        promised_forces.load(Ordering::SeqCst),
        1,
        "a worker should pursue useful lazy work through a completed promise"
    );
    assert!(promised_work.cached().is_some());
    assert_eq!(counts.deferred_active, 0);
    assert_eq!(counts.promises_active, 0);
}

#[test]
fn spark_resumes_after_a_resolver_owned_promise_completes() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    let session = crate::evaluation::EvaluationSession::shared(&coordinator);
    let context = EvalContext::new(session);
    let promise = PromisedValue::new(&crate::core::test_value_factory(), "later spark input");
    context.spark(Value::Promised(promise.clone()));
    wait_for_blocked_sparks(
        &coordinator,
        1,
        "an unassigned promise should retain its spark demand",
    );

    let (forced_sender, forced_receiver) = std::sync::mpsc::channel();
    let assigned = LazyValue::deferred(
        &crate::core::test_value_factory(),
        "resolved spark work",
        move |_| {
            forced_sender
                .send(())
                .expect("spark result receiver should remain open");
            Ok(n(7))
        },
    );
    promise
        .set(Value::Lazy(assigned.clone()))
        .expect("promise should accept its one assignment");
    context.notify_promise_changed();

    forced_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("promise completion should resume and finish its spark");
    wait_for_lazy_cache(&assigned, "resumed spark work must be cached");
    wait_for_blocked_sparks(
        &coordinator,
        0,
        "a completed spark must leave no parked executor job",
    );
    wait_for_no_deferred_tasks(
        &context,
        "promise and lazy followers must retire after spark completion",
    );
}

#[test]
fn dropping_a_session_discards_its_blocked_sparks() {
    let (coordinator, _executor) =
        crate::evaluation::test_execution_resources(1).expect("test worker should start");
    {
        let session = crate::evaluation::EvaluationSession::shared(&coordinator);
        let context = EvalContext::new(session);
        context.spark(Value::Promised(PromisedValue::new(
            &crate::core::test_value_factory(),
            "discarded blocked spark",
        )));
        wait_for_blocked_sparks(
            &coordinator,
            1,
            "unassigned promise should park before its session is dropped",
        );
    }
    wait_for_blocked_sparks(
        &coordinator,
        0,
        "parked spark values must not outlive their evaluation session",
    );
}

#[test]
fn metadata_seq_preserves_retryable_promise_blockage() {
    let context = test_context();
    let owner = context.with_new_task().unwrap();
    let observer = context.with_new_task().unwrap();
    let promise = PromisedValue::fixpoint(&owner, "blocked metadata").unwrap();
    let carrier = Value::metadata_carrier(Value::Promised(promise.clone()));

    let blocked = apply_values(
        &observer,
        Value::Builtin(Builtin::Seq),
        vec![carrier.clone(), n(42)],
    )
    .expect_err("seq should block on unresolved hidden metadata");
    assert!(blocked.blocked_on().is_some());

    promise.set(n(7)).unwrap();
    assert_eq!(
        apply_values(
            &observer,
            Value::Builtin(Builtin::Seq),
            vec![carrier, n(42)],
        )
        .expect("seq should resume after hidden metadata completes"),
        n(42)
    );
}

#[test]
fn completed_metadata_updates_release_sources_and_task_records() {
    let context = test_context();
    let prior_source_dropped = Arc::new(AtomicBool::new(false));
    let prior_signal = DropSignal(prior_source_dropped.clone());
    let prior = Value::deferred(
        &crate::core::test_value_factory(),
        "discardable prior metadata",
        move |_| {
            let _keep_signal_captured = &prior_signal;
            Ok(n(1))
        },
    );
    let outputs = run_metadata_update(
        &context,
        closed_function_value(
            1,
            TestExpr::Value(Value::List(List::from_values(vec![n(7)]))),
        ),
        vec![Value::metadata_carrier(prior)],
    )
    .expect("metadata update should remain lazy");
    assert!(
        !prior_source_dropped.load(Ordering::Acquire),
        "the unresolved update must retain its prior metadata input"
    );

    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Seq),
        vec![outputs[0].clone(), n(42)],
    )
    .expect("seq should complete the derived metadata");
    assert_eq!(result, n(42));
    assert!(
        prior_source_dropped.load(Ordering::Acquire),
        "a completed update which ignores its input should release the prior metadata graph"
    );
    let counts = context.task_registry_counts();
    assert_eq!(counts.deferred_active, 0);
    assert_eq!(counts.deferred_terminal, 0);
    assert_eq!(counts.deferred_by_wait, 0);
    assert_eq!(counts.deferred_by_task, 0);
}

#[test]
fn strategy_annotations_share_builtin_semantics() {
    let context = test_context();
    let seq_annotation = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("seq"),
        Value::error(&crate::core::test_value_factory(), "annotation forced"),
    ));
    let error = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![seq_annotation, n(42)],
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "annotation forced");

    let spark_annotation = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("spark"),
        Value::deferred(
            &crate::core::test_value_factory(),
            "discarded annotated spark",
            |_| panic!("zero-worker annotated spark should be silently dropped"),
        ),
    ));
    let result = apply_values(
        &context,
        Value::Builtin(Builtin::Anno),
        vec![spark_annotation, n(42)],
    )
    .unwrap();
    assert_eq!(eval_value(&context, &result).unwrap(), n(42));
}
