use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

#[cfg(feature = "aggressive-gc-verification")]
use crate::core::LazyValue;
use crate::core::{
    CoreValueFactory, EvaluationFailure, Value, thread_has_runtime_value_access_for_test,
};
use crate::evaluation::{EvalContext, EvaluationPollContext};
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::*;

fn isolated_values() -> CoreValueFactory {
    CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
}

fn root(values: &CoreValueFactory, value: Value) -> RuntimeValueRoot {
    values.construct_runtime_value_root(|_| value)
}

fn text(value: &'static str) -> Value {
    Value::binary_from_text(value)
}

#[test]
fn external_boundary_publishes_the_complete_checkpoint_before_access_closes() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let mut computation = WhnfComputation::from_root(root(&values, text("initial")));
    let registrations_before = values.managed_root_registrations_for_test();

    let mut budget = WhnfStepBudget::new(1);
    let outcome = poll.with_value_access(&context, |access| {
        let outcome = computation.poll_in(&access, &mut budget, |_access, work| {
            assert_eq!(work.focus, text("initial"));
            assert!(work.frames.is_empty());
            work.focus = text("replacement");
            work.frames.push(
                RegionalWhnfFrame {
                    kind: WhnfFrameKind::OrderedOperands,
                    cursor: 7,
                    retained: vec![text("left"), text("right")],
                }
                .into(),
            );
            RegionalWhnfStep::Boundary(RegionalBoundaryRequest::External(
                WhnfExternalBoundary::Reflection,
            ))
        });

        assert!(thread_has_runtime_value_access_for_test());
        assert_eq!(
            values.managed_root_registrations_for_test(),
            registrations_before + 3,
            "focus and two retained values must be rooted before access closes"
        );
        outcome
    });

    assert!(!thread_has_runtime_value_access_for_test());
    assert!(matches!(
        outcome,
        WhnfPoll::External(WhnfExternalBoundary::Reflection)
    ));

    let mut callback_ran = false;
    let mut callback = || {
        assert!(!thread_has_runtime_value_access_for_test());
        callback_ran = true;
    };
    callback();
    assert!(callback_ran);

    let mut resume_budget = WhnfStepBudget::new(1);
    let completed = poll.with_value_access(&context, |access| {
        computation.poll_in(&access, &mut resume_budget, |_access, work| {
            assert_eq!(work.focus, text("replacement"));
            assert_eq!(work.frames.len(), 1);
            let RegionalWhnfContinuation::Generic(frame) = &work.frames[0] else {
                panic!("expected a generic ordered-operands frame")
            };
            assert_eq!(frame.kind, WhnfFrameKind::OrderedOperands);
            assert_eq!(frame.cursor, 7);
            assert_eq!(frame.retained, [text("left"), text("right")]);
            RegionalWhnfStep::Ready(Value::Number(42.into()))
        })
    });
    let WhnfPoll::Ready(completed) = completed else {
        panic!("the resumed checkpoint must complete")
    };
    assert_eq!(completed.clone_core_for_test(), Value::Number(42.into()));
}

#[test]
fn permanent_failure_is_rooted_inside_the_regional_poll() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let mut computation = WhnfComputation::from_root(root(&values, Value::Number(0.into())));
    let registrations_before = values.managed_root_registrations_for_test();

    let mut budget = WhnfStepBudget::new(1);
    let failed = poll.with_value_access(&context, |access| {
        let failed = computation.poll_in(&access, &mut budget, |access, _work| {
            let failure = EvaluationFailure::emission(text("failure"))
                .with_context_in(access.values(), text("context"));
            RegionalWhnfStep::Failed(Arc::new(failure))
        });
        assert!(thread_has_runtime_value_access_for_test());
        assert_eq!(
            values.managed_root_registrations_for_test(),
            registrations_before + 2,
            "the failure emission and context must be rooted before access closes"
        );
        failed
    });

    assert!(!thread_has_runtime_value_access_for_test());
    let WhnfPoll::Failed(failure) = failed else {
        panic!("the permanent failure must leave as a rooted failure")
    };
    assert_eq!(failure.direct_value_roots().len(), 2);
    assert_eq!(failure.as_failure().contexts(), [text("context")]);
}

#[test]
fn unwind_preserves_a_nonempty_prior_checkpoint() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let mut computation = WhnfComputation::from_root(root(&values, text("initial")));

    let mut install_budget = WhnfStepBudget::new(1);
    let installed = poll.with_value_access(&context, |access| {
        computation.poll_in(&access, &mut install_budget, |_access, work| {
            work.focus = text("prior");
            work.frames.push(
                RegionalWhnfFrame {
                    kind: WhnfFrameKind::AccessPath,
                    cursor: 11,
                    retained: vec![text("retained")],
                }
                .into(),
            );
            RegionalWhnfStep::Boundary(RegionalBoundaryRequest::External(
                WhnfExternalBoundary::Host,
            ))
        })
    });
    assert!(matches!(
        installed,
        WhnfPoll::External(WhnfExternalBoundary::Host)
    ));

    let unwind = catch_unwind(AssertUnwindSafe(|| {
        let mut panic_budget = WhnfStepBudget::new(1);
        poll.with_value_access(&context, |access| {
            computation.poll_in(&access, &mut panic_budget, |_access, work| {
                assert_eq!(work.focus, text("prior"));
                assert_eq!(work.frames.len(), 1);
                work.focus = text("transient");
                work.frames.clear();
                panic!("forced regional unwind")
            })
        });
    }));
    assert!(unwind.is_err());
    assert!(!thread_has_runtime_value_access_for_test());

    let mut resume_budget = WhnfStepBudget::new(1);
    let resumed = poll.with_value_access(&context, |access| {
        computation.poll_in(&access, &mut resume_budget, |_access, work| {
            assert_eq!(work.focus, text("prior"));
            assert_eq!(work.frames.len(), 1);
            let RegionalWhnfContinuation::Generic(frame) = &work.frames[0] else {
                panic!("expected a generic access-path frame")
            };
            assert_eq!(frame.kind, WhnfFrameKind::AccessPath);
            assert_eq!(frame.cursor, 11);
            assert_eq!(frame.retained, [text("retained")]);
            RegionalWhnfStep::Ready(Value::Number(17.into()))
        })
    });
    assert!(matches!(resumed, WhnfPoll::Ready(_)));
}

#[test]
fn dropping_a_suspended_computation_retires_its_complete_checkpoint() {
    let values = isolated_values();
    let baseline = values
        .collect_managed_for_test()
        .expect("the cancellation fixture should begin collectible");
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let mut computation = WhnfComputation::from_root(root(&values, text("initial")));

    let mut budget = WhnfStepBudget::new(1);
    let yielded = poll.with_value_access(&context, |access| {
        computation.poll_in(&access, &mut budget, |_access, _work| {
            RegionalWhnfStep::Continue(RegionalWhnfWork {
                focus: text("replacement"),
                frames: vec![
                    RegionalWhnfFrame {
                        kind: WhnfFrameKind::CollectionWalk,
                        cursor: 3,
                        retained: vec![text("first"), text("second")],
                    }
                    .into(),
                ],
                followed: BTreeSet::new(),
            })
        })
    });
    assert!(matches!(yielded, WhnfPoll::Yielded));

    let suspended = values
        .collect_managed_for_test()
        .expect("a complete suspended checkpoint should remain live");
    assert_eq!(suspended.root_entries(), baseline.root_entries() + 3);
    assert_eq!(
        suspended.finalized_slots(),
        1,
        "the superseded initial checkpoint should already be collectible"
    );

    drop(computation);
    let cancelled = values
        .collect_managed_for_test()
        .expect("dropping the computation should retire its checkpoint");
    assert_eq!(cancelled.root_entries(), baseline.root_entries());
    assert_eq!(cancelled.finalized_slots(), 3);
}

#[cfg(feature = "aggressive-gc-verification")]
#[test]
fn collection_between_polls_preserves_only_the_installed_checkpoint() {
    let values = isolated_values();
    let baseline = values
        .collect_managed_for_test()
        .expect("the collection fixture should begin collectible");
    values.enable_collection_before_outer_entry_for_verification();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);

    let mut prior_id = None;
    let initial = values.construct_runtime_value_root(|access| {
        let lazy = LazyValue::error_in(access, "prior checkpoint");
        prior_id = Some(lazy.access(access).id());
        Value::Lazy(lazy)
    });
    let prior_id = prior_id.expect("the prior lazy identity should be recorded");
    let mut computation = WhnfComputation::from_root(initial);

    let mut replacement_id = None;
    let mut budget = WhnfStepBudget::new(1);
    let yielded = poll.with_value_access(&context, |access| {
        computation.poll_in(&access, &mut budget, |access, work| {
            let Value::Lazy(prior) = &work.focus else {
                panic!("the initial checkpoint should retain its lazy")
            };
            assert_eq!(access.lazy(prior).id(), prior_id);
            let replacement = LazyValue::error_in(access.values(), "replacement checkpoint");
            replacement_id = Some(access.lazy(&replacement).id());
            RegionalWhnfStep::Continue(RegionalWhnfWork {
                focus: Value::Lazy(replacement),
                frames: Vec::new(),
                followed: BTreeSet::new(),
            })
        })
    });
    assert!(matches!(yielded, WhnfPoll::Yielded));
    let replacement_id = replacement_id.expect("the replacement identity should be recorded");

    let between = values
        .collect_managed_for_test()
        .expect("collection between polls should preserve the installed checkpoint");
    assert_eq!(between.root_entries(), baseline.root_entries() + 1);
    assert!(
        between.finalized_slots() >= 2,
        "the superseded value root and lazy should be collectible"
    );

    let mut resume_budget = WhnfStepBudget::new(1);
    let resumed = poll.with_value_access(&context, |access| {
        computation.poll_in(&access, &mut resume_budget, |access, work| {
            let Value::Lazy(replacement) = &work.focus else {
                panic!("the replacement checkpoint should retain its lazy")
            };
            assert_eq!(access.lazy(replacement).id(), replacement_id);
            RegionalWhnfStep::Ready(Value::Number(23.into()))
        })
    });
    assert!(matches!(resumed, WhnfPoll::Ready(_)));

    drop(resumed);
    drop(computation);
    let retired = values
        .collect_managed_for_test()
        .expect("retiring the computation should release its final checkpoint");
    assert_eq!(retired.root_entries(), baseline.root_entries());
    assert!(retired.finalized_slots() >= 2);
}
