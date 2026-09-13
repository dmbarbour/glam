use crate::core::{Builtin, Dict, Key, LazySource, PromisedValue, Value};
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
        WhnfComputation::from_application_checkpoint_in(&access, function, arguments)
    })
}

fn poll(context: &EvalContext, computation: &mut WhnfComputation, steps: usize) -> WhnfPoll {
    let poll = EvaluationPollContext::for_context(context);
    let mut budget = WhnfStepBudget::new(steps);
    poll.with_value_access(context, |access| {
        computation.poll_semantic_in(&access, &mut budget)
    })
}

fn ready_after_yields(
    context: &EvalContext,
    computation: &mut WhnfComputation,
    limit: usize,
) -> RuntimeValueRoot {
    for _ in 0..limit {
        match poll(context, computation, 1) {
            WhnfPoll::Yielded => {}
            WhnfPoll::Ready(value) => return value,
            _ => panic!("the assigned fixture must advance without another boundary"),
        }
    }
    panic!("the assigned fixture did not finish within its structural bound")
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

#[test]
fn effect_payload_undefined_check_resumes_from_the_exact_promise() {
    let context = context();
    let promise = PromisedValue::new(context.values(), "effect payload");
    let effect = Value::Dict(
        Dict::new_sync().insert(Key::atom_from_text("eff"), Value::Promised(promise.clone())),
    );
    let mut computation = application(&context, effect, &[Value::Number(23.into())]);

    let WhnfPoll::Deferred(WhnfDeferredRequest::Promise(root)) =
        poll(&context, &mut computation, 2)
    else {
        panic!("tag recognition must suspend on the exact unresolved payload")
    };
    let expected = promise.id(context.values());
    assert_eq!(root.id(), expected);

    crate::core::set_test_promise(context.values(), &promise, Value::Number(29.into()))
        .expect("the effect payload should accept its assignment");
    let result = ready_after_yields(&context, &mut computation, 4).clone_core_for_test();
    let Value::Dict(effect) = result else {
        panic!("a semantically defined singleton tag must produce an effect")
    };
    let Value::PartialBuiltin(call) = effect
        .get(&Key::atom_from_text("eff"))
        .expect("the result must retain its effect function")
    else {
        panic!("effect application must append through EffectApply")
    };
    assert_eq!(call.builtin, Builtin::EffectApply);
    assert_eq!(call.arguments[1], Value::Number(23.into()));
}

#[test]
fn nested_undefined_extra_resumes_without_restarting_tag_recognition() {
    let context = context();
    let promise = PromisedValue::new(context.values(), "nested undefined extra");
    let nested = Value::Dict(Dict::new_sync().insert(
        Key::atom_from_text("nested"),
        Value::Promised(promise.clone()),
    ));
    let effect = Value::Dict(
        Dict::new_sync()
            .insert(Key::atom_from_text("eff"), Value::Number(31.into()))
            .insert(Key::atom_from_text("extra"), nested),
    );
    let mut computation = application(&context, effect, &[Value::Number(37.into())]);

    assert!(matches!(
        poll(&context, &mut computation, 8),
        WhnfPoll::Deferred(WhnfDeferredRequest::Promise(_))
    ));
    crate::core::set_test_promise(context.values(), &promise, Value::Dict(Dict::new_sync()))
        .expect("the nested extra should accept its undefined assignment");

    let result = ready_after_yields(&context, &mut computation, 8).clone_core_for_test();
    assert!(
        matches!(result, Value::Dict(ref dict) if dict.get(&Key::atom_from_text("eff")).is_some()),
        "a recursively undefined extra must not disqualify the effect tag"
    );
}
