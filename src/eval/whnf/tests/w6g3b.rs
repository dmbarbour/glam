use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, Weak};

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
        .expect("borrowed-driver wait identity should allocate");
    let producer = EvaluationTaskId::from_nonzero(
        values
            .ids()
            .evaluation_task()
            .expect("borrowed-driver producer identity should allocate"),
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

fn multi_frame_work(access: &EvaluationValueAccess<'_>) -> RegionalWhnfWork {
    RegionalWhnfWork::from_parts(
        access,
        Value::Number(0.into()),
        vec![
            WhnfContinuation::Generic(WhnfFrame {
                kind: WhnfFrameKind::CollectionWalk,
                cursor: 0,
                retained: vec![Value::Number(10.into()), Value::Number(11.into())],
            }),
            WhnfContinuation::Application {
                arguments: vec![Value::Number(20.into()), Value::Number(21.into())],
                next: 0,
            },
            WhnfContinuation::SemanticUndefined {
                purpose: UndefinedPurpose::EffectExtra,
                ancestors: vec![WhnfUndefinedDictionary {
                    members: vec![Value::Number(30.into()), Value::Number(31.into())],
                    next: 0,
                }],
                phase: UndefinedPhase::Inspect,
            },
        ],
        BTreeSet::new(),
        None,
        None,
    )
}

#[test]
fn borrowed_multi_frame_yield_preserves_containers_without_projection_or_roots() {
    WhnfComputation::reset_baseline_metrics_for_test();
    let context = context();
    let values = context.values();
    let registrations_before = values.managed_root_registrations_for_test();
    let poll = EvaluationPollContext::for_context(&context);

    poll.with_value_access(&context, |access| {
        let mut work = multi_frame_work(&access);
        let identities = work.container_identities_for_test();
        let mut budget = WhnfStepBudget::new(1);
        let status = drive_regional_in_place(&access, &mut work, &mut budget, |_access, state| {
            let WhnfContinuation::Generic(frame) = &mut state.frames[0] else {
                unreachable!()
            };
            frame.cursor = 1;
            let WhnfContinuation::Application { next, .. } = &mut state.frames[1] else {
                unreachable!()
            };
            *next = 1;
            RegionalWhnfStep::Delegate(Value::Number(1.into()))
        });

        assert!(matches!(status, RegionalWhnfStatus::Yielded));
        assert_eq!((budget.spent(), budget.remaining()), (1, 0));
        assert_eq!(work.focus, Value::Number(1.into()));
        assert_eq!(work.container_identities_for_test(), identities);
    });

    assert_eq!(
        values.managed_root_registrations_for_test(),
        registrations_before
    );
    assert_eq!(
        WhnfComputation::baseline_metrics_for_test(),
        DurableWhnfBaselineMetrics::default()
    );
}

#[test]
fn owned_and_borrowed_drivers_preserve_boundary_identity_and_budget() {
    let context = context();
    let expected = wait(&context);
    let poll = EvaluationPollContext::for_context(&context);

    poll.with_value_access(&context, |access| {
        let boundary = |_access: &EvaluationValueAccess<'_>, _state: &mut RegionalWhnfState<'_>| {
            RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Dependency(WhnfDependency::Wait(
                expected.clone(),
            )))
        };

        let mut borrowed = multi_frame_work(&access);
        let mut borrowed_budget = WhnfStepBudget::new(1);
        let RegionalWhnfStatus::Boundary(RegionalBoundaryRequest::Dependency(
            WhnfDependency::Wait(borrowed_wait),
        )) = drive_regional_in_place(&access, &mut borrowed, &mut borrowed_budget, boundary)
        else {
            panic!("borrowed driver must preserve its exact boundary")
        };

        let mut owned_budget = WhnfStepBudget::new(1);
        let RegionalWhnfDrive::Boundary {
            work: owned,
            request: RegionalBoundaryRequest::Dependency(WhnfDependency::Wait(owned_wait)),
        } = drive_regional(
            &access,
            multi_frame_work(&access),
            &mut owned_budget,
            boundary,
        )
        else {
            panic!("owned adapter must preserve its exact boundary")
        };

        assert_eq!(borrowed_wait, expected);
        assert_eq!(owned_wait, expected);
        assert_eq!(borrowed.container_identities_for_test().len(), 5);
        assert_eq!(owned.container_identities_for_test().len(), 5);
        assert_eq!(borrowed_budget.spent(), owned_budget.spent());
        assert_eq!(borrowed_budget.remaining(), owned_budget.remaining());
    });
}
