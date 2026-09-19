//! Scheduler-boundary adapters for resumable WHNF evaluation.
//!
//! Semantic progress remains in `eval::whnf`. This module is the only layer
//! which knows both its dependency vocabulary and coordinator work.

use super::{EvalContext, EvaluationPollContext, WorkDependency};
#[cfg(test)]
use crate::core::thread_has_runtime_value_access_for_test;
use crate::core::{EvaluationFailure, ManagedLazyRoot};
use crate::eval::whnf::{
    WhnfComputation, WhnfDeferredRequest, WhnfDependency, WhnfExternalBoundary, WhnfPoll,
};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

pub(crate) enum WhnfOwnerPoll {
    Ready(RuntimeValueRoot),
    Pending(WorkDependency),
    External(WhnfExternalBoundary),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(super) fn work_dependency(dependency: WhnfDependency) -> WorkDependency {
    match dependency {
        WhnfDependency::Wait(wait) => WorkDependency::Wait(wait.0),
        WhnfDependency::Promise(promise) => WorkDependency::Promise(promise),
    }
}

/// Polls semantic WHNF work, then interprets its deferred-shell request only
/// after the managed region has closed.
pub(crate) fn poll_computation(
    computation: &mut WhnfComputation,
    poll_context: &EvaluationPollContext,
    context: &EvalContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> WhnfOwnerPoll {
    let poll = poll_context.with_value_access(context, |access| {
        computation.poll_semantic_in(&access, step_budget)
    });
    #[cfg(test)]
    assert!(
        !thread_has_runtime_value_access_for_test(),
        "WHNF orchestration must begin only after managed access closes"
    );
    interpret_poll(poll, context)
}

/// Polls the canonical WHNF checkpoint retained by one managed lazy.
///
/// The lazy root is the liveness authority. Its checkpoint edge is duplicated
/// and consumed only inside this matching access region, then orchestration is
/// interpreted after the region closes just like an ordinary computation.
pub(crate) fn poll_lazy_checkpoint(
    lazy: &ManagedLazyRoot,
    poll_context: &EvaluationPollContext,
    context: &EvalContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> Option<WhnfOwnerPoll> {
    let poll = poll_context.with_value_access(context, |access| {
        let checkpoint = access.lazy_root(lazy).checkpoint_snapshot()?;
        checkpoint.poll_semantic_in(&access, step_budget)
    });
    #[cfg(test)]
    assert!(
        !thread_has_runtime_value_access_for_test(),
        "lazy-checkpoint orchestration must begin only after managed access closes"
    );
    poll.map(|poll| interpret_poll(poll, context))
}

fn interpret_poll(poll: WhnfPoll, context: &EvalContext) -> WhnfOwnerPoll {
    match poll {
        WhnfPoll::Ready(value) => WhnfOwnerPoll::Ready(value),
        WhnfPoll::Pending(dependency) => WhnfOwnerPoll::Pending(work_dependency(dependency)),
        WhnfPoll::Deferred(WhnfDeferredRequest::Lazy(lazy)) => {
            match crate::eval::lazy_root_wait(context, &lazy) {
                Ok(wait) => WhnfOwnerPoll::Pending(WorkDependency::Wait(wait)),
                Err(error) => WhnfOwnerPoll::Failed(RuntimeFailureRoot::new(
                    context.values(),
                    std::sync::Arc::new(EvaluationFailure::message(error.as_ref())),
                )),
            }
        }
        WhnfPoll::Deferred(WhnfDeferredRequest::Promise(promise)) => {
            if let Some(producer) = promise.producer()
                && context.observes_as_task(producer.owner())
            {
                return WhnfOwnerPoll::Failed(RuntimeFailureRoot::new(
                    context.values(),
                    std::sync::Arc::new(EvaluationFailure::message(format!(
                        "reflection promise {} recursively observed itself in task {}",
                        promise.id().get(),
                        producer.owner().get()
                    ))),
                ));
            }
            WhnfOwnerPoll::Pending(WorkDependency::Promise(promise))
        }
        WhnfPoll::Deferred(WhnfDeferredRequest::PromiseFollow(promise)) => {
            match crate::eval::promise_root_wait(context, &promise) {
                Ok(wait) => WhnfOwnerPoll::Pending(WorkDependency::Wait(wait)),
                Err(error) => WhnfOwnerPoll::Failed(RuntimeFailureRoot::new(
                    context.values(),
                    std::sync::Arc::new(EvaluationFailure::message(error.as_ref())),
                )),
            }
        }
        WhnfPoll::External(boundary) => WhnfOwnerPoll::External(boundary),
        WhnfPoll::Yielded => WhnfOwnerPoll::Yielded,
        WhnfPoll::Failed(failure) => WhnfOwnerPoll::Failed(failure),
    }
}

#[cfg(test)]
mod tests {
    use crate::core::{LazyValue, ManagedPromiseRoot, PromisedValue, Value};
    use crate::core_net::CoreWaitToken;
    use crate::evaluation::{EvalContext, EvaluationPollContext};

    use super::*;

    #[test]
    fn whnf_dependencies_translate_only_at_the_evaluation_boundary() {
        let context = EvalContext::isolated(crate::core::CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        ));
        let (_, task, _) = context
            .task_owned_promise("WHNF dependency translation")
            .expect("test promise should register");
        let wait = work_dependency(WhnfDependency::Wait(CoreWaitToken(task.wait().clone())));
        assert!(matches!(wait, WorkDependency::Wait(_)));

        let promise = PromisedValue::new(context.values(), "WHNF unassigned promise");
        let promise = promise.root(context.values());
        let dependency = work_dependency(WhnfDependency::Promise(promise.clone()));
        let WorkDependency::Promise(translated) = dependency else {
            panic!("WHNF promise must remain a promise dependency")
        };
        assert!(ManagedPromiseRoot::same_promise(&translated, &promise));
    }

    #[test]
    fn uncached_lazy_admission_occurs_after_regional_access_closes() {
        let context = EvalContext::isolated(crate::core::CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        ));
        let focus = context.values().construct_runtime_value_root(|access| {
            Value::Lazy(LazyValue::semantic_thunk_in(
                access,
                "post-region lazy admission",
                |_| Ok(Value::Number(61.into())),
            ))
        });
        let mut computation = WhnfComputation::from_root(focus);
        let poll_context = EvaluationPollContext::for_context(&context);

        assert_eq!(context.deferred_task_count(), 0);
        let result = poll_computation(
            &mut computation,
            &poll_context,
            &context,
            &mut crate::evaluation::EvaluationStepBudget::new(1),
        );
        let WhnfOwnerPoll::Pending(WorkDependency::Wait(wait)) = result else {
            panic!("uncached lazy must admit its canonical producer after inspection")
        };
        assert!(!thread_has_runtime_value_access_for_test());
        assert_eq!(context.deferred_task_count(), 1);
        assert!(matches!(
            context.poll_wait(&wait),
            crate::evaluation::EvaluationWaitPoll::Pending(_)
        ));
    }

    #[test]
    fn unassigned_resolver_promise_becomes_a_direct_dependency() {
        let context = EvalContext::isolated(crate::core::CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        ));
        let promise = PromisedValue::new(context.values(), "direct promise dependency");
        let expected = promise.id(context.values());
        let focus = context.values().construct_runtime_value_root(|access| {
            access.duplicate_value(&Value::Promised(promise))
        });
        let mut computation = WhnfComputation::from_root(focus);
        let poll_context = EvaluationPollContext::for_context(&context);

        let result = poll_computation(
            &mut computation,
            &poll_context,
            &context,
            &mut crate::evaluation::EvaluationStepBudget::new(1),
        );
        let WhnfOwnerPoll::Pending(WorkDependency::Promise(promise)) = result else {
            panic!("resolver promise must remain the exact completion dependency")
        };
        assert_eq!(promise.id(), expected);
        assert_eq!(context.deferred_task_count(), 0);
    }

    #[test]
    fn task_owned_promise_self_observation_fails_outside_regional_access() {
        let context = EvalContext::isolated(crate::core::CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        ));
        let (promise, task, owner) = context
            .task_owned_promise("self-observed promise")
            .expect("test task should own its promise");
        let promise_id = promise.id(owner.values());
        let focus = owner.values().construct_runtime_value_root(|access| {
            access.duplicate_value(&Value::Promised(promise))
        });
        let mut computation = WhnfComputation::from_root(focus);
        let poll_context = EvaluationPollContext::for_context(&owner);

        let result = poll_computation(
            &mut computation,
            &poll_context,
            &owner,
            &mut crate::evaluation::EvaluationStepBudget::new(1),
        );
        let WhnfOwnerPoll::Failed(failure) = result else {
            panic!("a producer task cannot wait on its own promise")
        };
        assert!(failure.to_string().contains(&format!(
            "reflection promise {} recursively observed itself in task {}",
            promise_id.get(),
            task.id().get()
        )));
    }
}
