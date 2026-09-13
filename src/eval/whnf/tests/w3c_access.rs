use crate::core::{Dict, Key, PromisedValue, Value};
use crate::evaluation::{EvalContext, EvaluationPollContext, OwnedEvalContext};
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::*;

fn context() -> OwnedEvalContext {
    EvalContext::isolated(CoreValueFactory::new(
        allocate_evaluation_runtime_id(),
        RuntimeIds::new(),
    ))
}

fn static_access(
    context: &EvalContext,
    base: Value,
    keys: impl IntoIterator<Item = Key>,
) -> WhnfComputation {
    let poll = EvaluationPollContext::for_context(context);
    poll.with_value_access(context, |access| {
        WhnfComputation::from_static_access_checkpoint_in(
            &access,
            base,
            keys.into_iter().collect::<Vec<_>>().into(),
            None,
        )
    })
}

fn poll(context: &EvalContext, computation: &mut WhnfComputation) -> WhnfPoll {
    let poll = EvaluationPollContext::for_context(context);
    poll.with_value_access(context, |access| {
        computation.poll_semantic_in(&access, &mut WhnfStepBudget::new(1))
    })
}

#[test]
fn static_access_resumes_at_the_exact_intermediate_dictionary() {
    let context = context();
    let outer = Key::atom_from_text("outer");
    let leaf = Key::atom_from_text("leaf");
    let promise = PromisedValue::new(context.values(), "access intermediate");
    let base =
        Value::Dict(Dict::new_sync().insert(outer.clone(), Value::Promised(promise.clone())));
    let mut computation = static_access(&context, base, [outer, leaf.clone()]);

    assert!(matches!(
        poll(&context, &mut computation),
        WhnfPoll::Yielded
    ));
    let WhnfPoll::Deferred(WhnfDeferredRequest::Promise(root)) = poll(&context, &mut computation)
    else {
        panic!("the path must suspend on its exact intermediate dictionary")
    };
    assert_eq!(root.id(), promise.id(context.values()));

    crate::core::set_test_promise(
        context.values(),
        &promise,
        Value::Dict(Dict::new_sync().insert(leaf, Value::Number(42.into()))),
    )
    .expect("the intermediate promise should accept its assignment");
    assert!(matches!(
        poll(&context, &mut computation),
        WhnfPoll::Yielded
    ));
    assert!(matches!(
        poll(&context, &mut computation),
        WhnfPoll::Yielded
    ));
    let WhnfPoll::Ready(result) = poll(&context, &mut computation) else {
        panic!("the retained path must finish after the intermediate assignment")
    };
    assert_eq!(result.clone_core_for_test(), Value::Number(42.into()));
}

#[test]
fn static_access_preserves_missing_and_nondictionary_results() {
    let context = context();
    let missing = Key::atom_from_text("missing");
    let mut absent = static_access(&context, Value::Dict(Dict::new_sync()), [missing.clone()]);
    assert!(matches!(poll(&context, &mut absent), WhnfPoll::Yielded));
    let WhnfPoll::Ready(value) = poll(&context, &mut absent) else {
        panic!("a missing static key must produce undefined")
    };
    assert!(matches!(value.clone_core_for_test(), Value::Dict(dict) if dict.is_empty()));

    let mut invalid = static_access(&context, Value::Number(1.into()), [missing]);
    let WhnfPoll::Failed(failure) = poll(&context, &mut invalid) else {
        panic!("a non-dictionary access base must remain a structured failure")
    };
    assert_eq!(
        failure.as_failure().to_string(),
        "value access base is not a dictionary"
    );
}
