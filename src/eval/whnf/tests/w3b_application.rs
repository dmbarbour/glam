use crate::core::{Builtin, LazySource, Value};
use crate::eval::test_support::{TestExpr, closed_function_value_in};
use crate::evaluation::{EvalContext, EvaluationPollContext, OwnedEvalContext};
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::*;

fn context() -> OwnedEvalContext {
    EvalContext::isolated(CoreValueFactory::new(
        allocate_evaluation_runtime_id(),
        RuntimeIds::new(),
    ))
}

fn application(context: &EvalContext, function: Value, arguments: &[Value]) -> WhnfComputation {
    let poll = EvaluationPollContext::for_context(context);
    poll.with_value_access(context, |access| {
        WhnfComputation::from_application_in(&access, function, arguments)
    })
}

fn poll(context: &EvalContext, computation: &mut WhnfComputation, steps: usize) -> WhnfPoll {
    let poll = EvaluationPollContext::for_context(context);
    let mut budget = WhnfStepBudget::new(steps);
    poll.with_value_access(context, |access| {
        computation.poll_semantic_in(&access, &mut budget)
    })
}

#[test]
fn builtin_application_batches_only_to_saturation() {
    let context = context();
    let mut computation = application(
        &context,
        Value::Builtin(Builtin::Add),
        &[Value::Number(2.into()), Value::Number(3.into())],
    );

    assert!(matches!(
        poll(&context, &mut computation, 1),
        WhnfPoll::Yielded
    ));
    let WhnfPoll::Deferred(WhnfDeferredRequest::Lazy(lazy)) = poll(&context, &mut computation, 1)
    else {
        panic!("a saturated builtin must delegate to its memoized lazy call")
    };
    let source = context.values().with_runtime_value_access(|access| {
        lazy.access(&access)
            .expect("the lazy must belong to this runtime")
            .source_snapshot()
    });
    let Some(LazySource::Builtin(call)) = source else {
        panic!("the application frame must preserve the ordinary builtin source")
    };
    assert_eq!(call.builtin, Builtin::Add);
    assert_eq!(
        call.arguments.as_ref(),
        [Value::Number(2.into()), Value::Number(3.into())]
    );
}

#[test]
fn partial_builtin_resumes_without_replaying_supplied_arguments() {
    let context = context();
    let partial = context.values().with_runtime_value_access(|access| {
        Value::builtin_call_in(&access, Builtin::Add, vec![Value::Number(7.into())])
    });
    let mut computation = application(&context, partial, &[Value::Number(11.into())]);

    assert!(matches!(
        poll(&context, &mut computation, 1),
        WhnfPoll::Yielded
    ));
    let WhnfPoll::Deferred(WhnfDeferredRequest::Lazy(lazy)) = poll(&context, &mut computation, 1)
    else {
        panic!("completing a partial builtin must expose one saturated call")
    };
    let source = context.values().with_runtime_value_access(|access| {
        lazy.access(&access)
            .expect("the lazy must belong to this runtime")
            .source_snapshot()
    });
    let Some(LazySource::Builtin(call)) = source else {
        panic!("the resumed partial builtin must retain a builtin source")
    };
    assert_eq!(
        call.arguments.as_ref(),
        [Value::Number(7.into()), Value::Number(11.into())]
    );
}

#[test]
fn function_application_batches_arguments_without_intermediate_roots() {
    let context = context();
    let function = closed_function_value_in(context.values(), 3, TestExpr::Local(2));
    let roots_before = context.values().managed_root_registrations_for_test();
    let mut computation = application(
        &context,
        function,
        &[Value::Number(13.into()), Value::Number(17.into())],
    );
    let roots_after_checkpoint = context.values().managed_root_registrations_for_test();
    assert_eq!(
        roots_after_checkpoint,
        roots_before + 1,
        "immediate arguments need no managed roots; only the function stage does"
    );

    assert!(matches!(
        poll(&context, &mut computation, 1),
        WhnfPoll::Yielded
    ));
    assert_eq!(
        context.values().managed_root_registrations_for_test(),
        roots_after_checkpoint + 1,
        "regional batching must publish only the resulting partial stage"
    );
    let WhnfPoll::Ready(result) = poll(&context, &mut computation, 1) else {
        panic!("an under-saturated function must become an immediate function value")
    };
    let Value::Function(function) = result.clone_core_for_test() else {
        panic!("partial function application must preserve a function stage")
    };
    assert_eq!(function.remaining_arity(), 1);
}
