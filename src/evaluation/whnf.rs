//! Scheduler-boundary adapters for resumable WHNF evaluation.
//!
//! Semantic progress remains in `eval::whnf`. This module is the only W1A
//! layer which knows both its dependency vocabulary and coordinator work.

#![allow(
    dead_code,
    reason = "W1A installs dependency translation before a production WHNF owner is cut over"
)]

use super::WorkDependency;
use crate::eval::whnf::WhnfDependency;

pub(super) fn work_dependency(dependency: WhnfDependency) -> WorkDependency {
    match dependency {
        WhnfDependency::Wait(wait) => WorkDependency::Wait(wait.0),
        WhnfDependency::Promise(promise) => WorkDependency::Promise(promise),
    }
}

#[cfg(test)]
mod tests {
    use crate::core::{ManagedPromiseRoot, PromisedValue};
    use crate::core_net::CoreWaitToken;
    use crate::evaluation::EvalContext;

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
}
