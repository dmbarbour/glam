use crate::core::{CoreValueFactory, Value};
use crate::evaluation::{EvalContext, EvaluationPollContext};
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::*;

fn isolated_values() -> CoreValueFactory {
    CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
}

#[test]
fn immediate_completion_returns_whnf_in_one_step() {
    let context = EvalContext::isolated(isolated_values());
    let poll = EvaluationPollContext::for_context(&context);
    poll.with_value_access(&context, |access| {
        let expected = Value::binary_from_text("immediate");
        let work = RegionalWhnfWork::from_parts(
            &access,
            access.values().duplicate_value(&expected),
            Vec::new(),
            BTreeSet::new(),
            None,
            None,
        );
        let mut budget = WhnfStepBudget::new(1);
        let outcome = drive_regional(&access, work, &mut budget, |access, work| {
            RegionalWhnfStep::Ready(access.values().duplicate_value(&work.focus))
        });
        let RegionalWhnfDrive::Ready(actual) = outcome else {
            panic!("immediate work must complete")
        };
        access
            .values()
            .assert_same_representation_for_test(&actual, &expected);
        assert_eq!(budget.remaining(), 0);
    });
}

#[test]
fn tail_delegation_is_iterative_and_does_not_push_or_root() {
    const DELEGATIONS: usize = 100_000;

    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let roots_before = values.managed_root_registrations_for_test();
    poll.with_value_access(&context, |access| {
        let expected = Value::binary_from_text("tail");
        let frames = Vec::with_capacity(8);
        let frame_capacity = frames.capacity();
        let work = RegionalWhnfWork::from_parts(
            &access,
            access.values().duplicate_value(&expected),
            frames,
            BTreeSet::new(),
            None,
            None,
        );
        let mut transitions = 0;
        let mut budget = WhnfStepBudget::new(DELEGATIONS + 1);
        let outcome = drive_regional(&access, work, &mut budget, |access, work| {
            assert!(work.frames.is_empty());
            assert_eq!(work.frames.capacity(), frame_capacity);
            if transitions < DELEGATIONS {
                transitions += 1;
                RegionalWhnfStep::Delegate(access.values().duplicate_value(&work.focus))
            } else {
                RegionalWhnfStep::Ready(access.values().duplicate_value(&work.focus))
            }
        });
        let RegionalWhnfDrive::Ready(actual) = outcome else {
            panic!("bounded tail delegation must complete")
        };
        access
            .values()
            .assert_same_representation_for_test(&actual, &expected);
        assert_eq!(transitions, DELEGATIONS);
        assert_eq!(budget.remaining(), 0);
    });
    assert_eq!(
        values.managed_root_registrations_for_test(),
        roots_before,
        "tail delegation must not register a durable root"
    );
}

#[test]
fn budget_yield_retains_work_without_inventing_a_dependency() {
    let context = EvalContext::isolated(isolated_values());
    let poll = EvaluationPollContext::for_context(&context);
    poll.with_value_access(&context, |access| {
        let work = RegionalWhnfWork::from_parts(
            &access,
            Value::binary_from_text("yield"),
            Vec::new(),
            BTreeSet::new(),
            None,
            None,
        );
        let mut transitions = 0;
        let mut budget = WhnfStepBudget::new(3);
        let outcome = drive_regional(&access, work, &mut budget, |access, work| {
            transitions += 1;
            RegionalWhnfStep::Delegate(access.values().duplicate_value(&work.focus))
        });
        let RegionalWhnfDrive::Yielded(work) = outcome else {
            panic!("budget exhaustion must yield without a dependency")
        };
        assert_eq!(transitions, 3);
        assert_eq!(budget.remaining(), 0);

        let mut resume_budget = WhnfStepBudget::new(1);
        let resumed = drive_regional(&access, work, &mut resume_budget, |access, work| {
            RegionalWhnfStep::Ready(access.values().duplicate_value(&work.focus))
        });
        assert!(matches!(resumed, RegionalWhnfDrive::Ready(_)));
    });
}
