use std::collections::BTreeSet;

use crate::core::{CoreValueFactory, Value};
use crate::evaluation::EvalContext;
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::*;

fn context() -> crate::evaluation::OwnedEvalContext {
    EvalContext::isolated(CoreValueFactory::new(
        allocate_evaluation_runtime_id(),
        RuntimeIds::new(),
    ))
}

fn multi_frame_work(access: &EvaluationValueAccess<'_>) -> RegionalWhnfWork {
    RegionalWhnfWork::from_parts(
        access,
        Value::Number(0.into()),
        vec![
            WhnfContinuation::Application {
                arguments: vec![
                    Value::Number(9.into()),
                    Value::Number(10.into()),
                    Value::Number(11.into()),
                ],
                next: 0,
            },
            WhnfContinuation::Application {
                arguments: vec![Value::Number(20.into()), Value::Number(21.into())],
                next: 0,
            },
            WhnfContinuation::StaticAccess {
                keys: std::sync::Arc::from([
                    crate::core::Key::atom_from_text("first"),
                    crate::core::Key::atom_from_text("second"),
                ]),
                next: 0,
            },
        ],
        BTreeSet::new(),
        None,
        None,
    )
}

#[test]
fn borrowed_multi_frame_yield_preserves_containers_without_projection_or_roots() {
    let context = context();
    let values = context.values();
    let registrations_before = values.managed_root_registrations_for_test();

    crate::evaluation::EvalContext::with_test_value_access(&context, |access| {
        let mut work = multi_frame_work(&access);
        let identities = work.container_identities_for_test();
        let mut budget = WhnfStepBudget::new(1);
        let status = drive_regional_in_place(&access, &mut work, &mut budget, |_access, state| {
            let WhnfContinuation::Application { next, .. } = &mut state.frames[0] else {
                unreachable!()
            };
            *next = 1;
            let WhnfContinuation::Application { next, .. } = &mut state.frames[1] else {
                unreachable!()
            };
            *next = 1;
            RegionalWhnfStep::Delegate(Value::Number(1.into()))
        });

        assert!(matches!(status, RegionalWhnfStatus::BudgetExhausted));
        assert_eq!((budget.spent(), budget.remaining()), (1, 0));
        access
            .values()
            .assert_same_representation_for_test(&work.focus, &Value::Number(1.into()));
        assert_eq!(work.container_identities_for_test(), identities);
    });

    assert_eq!(
        values.managed_root_registrations_for_test(),
        registrations_before
    );
}

#[test]
fn owned_and_borrowed_drivers_preserve_boundary_identity_and_budget() {
    let context = context();
    let (expected, _) = context.values().rooted_error_lazy_for_test();

    crate::evaluation::EvalContext::with_test_value_access(&context, |access| {
        let boundary = |_access: &EvaluationValueAccess<'_>, _state: &mut RegionalWhnfState<'_>| {
            RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Deferred(
                WhnfDeferredRequest::Lazy(expected.clone()),
            ))
        };

        let mut borrowed = multi_frame_work(&access);
        let mut borrowed_budget = WhnfStepBudget::new(1);
        let RegionalWhnfStatus::Boundary(RegionalBoundaryRequest::Deferred(
            WhnfDeferredRequest::Lazy(borrowed_request),
        )) = drive_regional_in_place(&access, &mut borrowed, &mut borrowed_budget, boundary)
        else {
            panic!("borrowed driver must preserve its exact boundary")
        };

        let mut owned_budget = WhnfStepBudget::new(1);
        let RegionalWhnfDrive::Boundary {
            work: owned,
            request: RegionalBoundaryRequest::Deferred(WhnfDeferredRequest::Lazy(owned_request)),
        } = drive_regional(
            &access,
            multi_frame_work(&access),
            &mut owned_budget,
            boundary,
        )
        else {
            panic!("owned adapter must preserve its exact boundary")
        };

        assert_eq!(borrowed_request.id(), expected.id());
        assert_eq!(owned_request.id(), expected.id());
        assert_eq!(borrowed.container_identities_for_test().len(), 4);
        assert_eq!(owned.container_identities_for_test().len(), 4);
        assert_eq!(borrowed_budget.spent(), owned_budget.spent());
        assert_eq!(borrowed_budget.remaining(), owned_budget.remaining());
    });
}
