use std::sync::{Arc, Mutex, Weak};

use glam_gc::EdgeTransitionObservation;

use crate::core::{CoreValueFactory, Value};
use crate::core_net::CoreWaitToken;
use crate::evaluation::{
    CompletionSubscriptions, EvalContext, EvaluationPollContext, EvaluationTaskId,
    EvaluationWaitToken, EvaluationWorkCoordinator,
};
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::*;

fn context() -> crate::evaluation::OwnedEvalContext {
    EvalContext::isolated(CoreValueFactory::new(
        allocate_evaluation_runtime_id(),
        RuntimeIds::new(),
    ))
}

fn wait(context: &EvalContext) -> CoreWaitToken {
    let values = context.values();
    let wait = values
        .ids()
        .evaluation_wait()
        .expect("aggregate-poll wait identity should allocate");
    let producer = EvaluationTaskId::from_nonzero(
        values
            .ids()
            .evaluation_task()
            .expect("aggregate-poll producer identity should allocate"),
    );
    let coordinator = Arc::new(Mutex::new(Weak::<EvaluationWorkCoordinator>::new()));
    let completion = CompletionSubscriptions::for_wait(values.runtime_id(), wait, coordinator);
    CoreWaitToken(EvaluationWaitToken::new(
        wait,
        values,
        context.session_id(),
        producer,
        completion,
    ))
}

#[test]
fn one_poll_aggregates_every_focus_and_frame_edit_into_one_edge_transition() {
    let context = context();
    let values = context.values();
    let (old_root, old) = values.rooted_error_lazy_for_test("W6G.3e old edge");
    let (new_root, new) = values.rooted_error_lazy_for_test("W6G.3e new edge");
    let focus = values
        .construct_runtime_value_root(|access| access.duplicate_value(&Value::Lazy(old.clone())));
    let mut computation = WhnfComputation::from_root(focus);
    let expected = wait(&context);
    let probe = values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);
    let poll = EvaluationPollContext::for_context(&context);

    let mut transition = 0;
    let mut budget = WhnfStepBudget::new(4);
    let outcome = poll.with_value_access(&context, |access| {
        computation.poll_in(&access, &mut budget, |_access, work| {
            transition += 1;
            match transition {
                1 => {
                    work.frames.push(
                        WhnfFrame {
                            kind: WhnfFrameKind::CollectionWalk,
                            cursor: 0,
                            retained: vec![Value::Lazy(old.clone())],
                        }
                        .into(),
                    );
                    RegionalWhnfStep::Delegate(Value::Lazy(new.clone()))
                }
                2 => {
                    let WhnfContinuation::Generic(frame) = &mut work.frames[0] else {
                        panic!("aggregate poll must retain its installed generic frame")
                    };
                    frame.cursor = 1;
                    frame.retained.push(Value::Lazy(new.clone()));
                    RegionalWhnfStep::Delegate(Value::Lazy(old.clone()))
                }
                3 => RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Dependency(
                    WhnfDependency::Wait(expected.clone()),
                )),
                _ => panic!("the exact dependency must stop the aggregate quantum"),
            }
        })
    });

    let WhnfPoll::Pending(WhnfDependency::Wait(observed)) = outcome else {
        panic!("the aggregate quantum must retain its exact dependency")
    };
    assert_eq!(observed, expected);
    assert_eq!(transition, 3);
    assert_eq!((budget.spent(), budget.remaining()), (3, 1));

    let records = probe.records();
    assert_eq!(
        records.len(),
        1,
        "one poll must publish one aggregate transition, not one per internal edit"
    );
    assert_eq!(records[0].leaving_edges(), 1);
    assert_eq!(records[0].adding_edges(), 3);
    drop((old_root, new_root));
}

#[test]
fn managed_checkpoint_resumes_on_another_worker_after_collection() {
    let context = context();
    let values = context.values().clone();
    let initial = values.construct_runtime_value_root(|_| Value::Number(1.into()));
    let mut computation = WhnfComputation::from_root(initial);
    let poll = EvaluationPollContext::for_context(&context);

    let mut first_budget = WhnfStepBudget::new(1);
    let first = poll.with_value_access(&context, |access| {
        computation.poll_in(&access, &mut first_budget, |_access, work| {
            assert_eq!(work.focus, Value::Number(1.into()));
            work.frames.push(
                WhnfFrame {
                    kind: WhnfFrameKind::DiagnosticContext,
                    cursor: 17,
                    retained: vec![Value::Number(2.into())],
                }
                .into(),
            );
            RegionalWhnfStep::Delegate(Value::Number(3.into()))
        })
    });
    assert!(matches!(first, WhnfPoll::Yielded));
    values
        .collect_managed_for_test()
        .expect("the suspended checkpoint must survive collection before migration");

    let worker_context = context.clone();
    std::thread::spawn(move || {
        let poll = EvaluationPollContext::for_context(&worker_context);
        let mut resume_budget = WhnfStepBudget::new(1);
        let resumed = poll.with_value_access(&worker_context, |access| {
            computation.poll_in(&access, &mut resume_budget, |_access, work| {
                assert_eq!(work.focus, Value::Number(3.into()));
                let WhnfContinuation::Generic(frame) = &work.frames[0] else {
                    panic!("cross-worker resumption must retain its exact frame")
                };
                assert_eq!(
                    (frame.kind, frame.cursor),
                    (WhnfFrameKind::DiagnosticContext, 17)
                );
                assert_eq!(frame.retained, [Value::Number(2.into())]);
                RegionalWhnfStep::Ready(Value::Number(5.into()))
            })
        });
        assert_eq!((resume_budget.spent(), resume_budget.remaining()), (1, 0));
        let WhnfPoll::Ready(result) = resumed else {
            panic!("cross-worker resumption must complete")
        };
        assert_eq!(result.clone_core_for_test(), Value::Number(5.into()));
    })
    .join()
    .expect("cross-worker WHNF resumption must not panic");
}

#[test]
fn aggregate_poll_exit_matrix_remains_explicit() {
    let w1c = include_str!("w1c.rs");
    for fixture in [
        "external_boundary_publishes_the_complete_checkpoint_before_access_closes",
        "permanent_failure_is_rooted_inside_the_regional_poll",
        "unwind_poison_is_reported_without_reentering_the_reducer",
        "dropping_a_suspended_computation_retires_its_complete_checkpoint",
        "collection_between_polls_preserves_only_the_installed_checkpoint",
    ] {
        assert!(
            w1c.contains(&format!("fn {fixture}")),
            "W6G.3e exit coverage lost `{fixture}`"
        );
    }

    let current = include_str!("w6g3e.rs");
    for fixture in [
        "one_poll_aggregates_every_focus_and_frame_edit_into_one_edge_transition",
        "managed_checkpoint_resumes_on_another_worker_after_collection",
    ] {
        assert!(
            current.contains(&format!("fn {fixture}")),
            "W6G.3e aggregate coverage lost `{fixture}`"
        );
    }

    let managed = include_str!("../managed_state.rs");
    assert!(managed.contains("state: Mutex<WhnfState>"));
    assert!(
        !managed.contains("Mutex<Option<WhnfState>>"),
        "no poll exit may publish an unlocked empty managed cell"
    );
}
