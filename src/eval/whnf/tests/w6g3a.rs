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

fn text(value: impl AsRef<str>) -> Value {
    Value::binary_from_text(value.as_ref())
}

fn synthetic_wait(context: &EvalContext) -> CoreWaitToken {
    let values = context.values();
    let wait = values
        .ids()
        .evaluation_wait()
        .expect("baseline wait identity should allocate");
    let producer = EvaluationTaskId::from_nonzero(
        values
            .ids()
            .evaluation_task()
            .expect("baseline producer identity should allocate"),
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

fn root(context: &EvalContext, value: Value) -> crate::runtime::RuntimeValueRoot {
    context.values().construct_runtime_value_root(|_| value)
}

fn poll_with(
    context: &EvalContext,
    computation: &mut WhnfComputation,
    budget: &mut WhnfStepBudget,
    access_entries: &mut usize,
    reduce: impl FnMut(&EvaluationValueAccess<'_>, &mut RegionalWhnfState<'_>) -> RegionalWhnfStep,
) -> WhnfPoll {
    *access_entries += 1;
    let poll = EvaluationPollContext::for_context(context);
    poll.with_value_access(context, |access| {
        computation.poll_in(&access, budget, reduce)
    })
}

fn assert_wait(outcome: WhnfPoll, expected: &CoreWaitToken) {
    let WhnfPoll::Pending(WhnfDependency::Wait(observed)) = outcome else {
        panic!("baseline poll must retain its exact wait dependency")
    };
    assert_eq!(&observed, expected);
}

fn managed_seed_promotion(frame_count: usize) {
    WhnfComputation::reset_baseline_metrics_for_test();
    let context = context();
    let values = context.values();
    let empty = values
        .collect_managed_for_test()
        .expect("baseline heap should collect before construction");
    let wait = synthetic_wait(&context);
    let mut computation = WhnfComputation::from_root(root(&context, text("initial")));
    let registrations_before = values.managed_root_registrations_for_test();
    let mut access_entries = 0;

    let mut install_budget = WhnfStepBudget::new(1);
    let installed = poll_with(
        &context,
        &mut computation,
        &mut install_budget,
        &mut access_entries,
        |_access, work| {
            work.focus = text("focus");
            work.frames = (0..frame_count)
                .map(|index| {
                    WhnfFrame {
                        kind: WhnfFrameKind::CollectionWalk,
                        cursor: index,
                        retained: vec![
                            text(format!("left-{index}")),
                            text(format!("right-{index}")),
                        ],
                    }
                    .into()
                })
                .collect();
            RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Dependency(WhnfDependency::Wait(
                wait.clone(),
            )))
        },
    );
    assert_wait(installed, &wait);
    assert_eq!((install_budget.spent(), install_budget.remaining()), (1, 0));
    assert_eq!(
        values.managed_root_registrations_for_test() - registrations_before,
        1,
        "first poll must replace the seed with one managed state root"
    );

    let registrations_after_install = values.managed_root_registrations_for_test();
    let mut yield_budget = WhnfStepBudget::new(0);
    assert!(matches!(
        poll_with(
            &context,
            &mut computation,
            &mut yield_budget,
            &mut access_entries,
            |_access, _work| panic!("an empty budget must not enter the reducer"),
        ),
        WhnfPoll::Yielded
    ));
    assert_eq!((yield_budget.spent(), yield_budget.remaining()), (0, 0));
    assert_eq!(
        values.managed_root_registrations_for_test() - registrations_after_install,
        0,
        "a zero-work yield must retain the same managed state root"
    );

    let registrations_after_yield = values.managed_root_registrations_for_test();
    let mut dependency_budget = WhnfStepBudget::new(1);
    let pending = poll_with(
        &context,
        &mut computation,
        &mut dependency_budget,
        &mut access_entries,
        |_access, _work| {
            RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Dependency(WhnfDependency::Wait(
                wait.clone(),
            )))
        },
    );
    assert_wait(pending, &wait);
    assert_eq!(
        (dependency_budget.spent(), dependency_budget.remaining()),
        (1, 0)
    );
    assert_eq!(
        values.managed_root_registrations_for_test() - registrations_after_yield,
        0,
        "an unchanged dependency boundary must retain the same managed state root"
    );

    assert_eq!(
        WhnfComputation::baseline_metrics_for_test(),
        DurableWhnfBaselineMetrics::default(),
        "managed seed polls must perform no legacy projection or reconstruction"
    );

    let registrations_before_ready = values.managed_root_registrations_for_test();
    let mut ready_budget = WhnfStepBudget::new(1);
    let WhnfPoll::Ready(ready) = poll_with(
        &context,
        &mut computation,
        &mut ready_budget,
        &mut access_entries,
        |_access, _work| RegionalWhnfStep::Ready(text("ready")),
    ) else {
        panic!("the baseline computation must retain its result semantics")
    };
    assert_eq!(ready.clone_core_for_test(), text("ready"));
    assert_eq!((ready_budget.spent(), ready_budget.remaining()), (1, 0));
    assert_eq!(
        values.managed_root_registrations_for_test() - registrations_before_ready,
        1,
        "only the terminal result is rooted when no checkpoint is published"
    );
    assert_eq!(
        WhnfComputation::baseline_metrics_for_test(),
        DurableWhnfBaselineMetrics::default()
    );
    assert_eq!(
        access_entries, 4,
        "the direct baseline opens one managed-access region per demand poll"
    );

    drop(ready);
    let retained = values
        .collect_managed_for_test()
        .expect("the installed checkpoint should remain collectible");
    assert_eq!(
        retained.root_entries(),
        empty.root_entries() + 1,
        "frame count must not change the single steady-state demand root"
    );
    drop(computation);
    let retired = values
        .collect_managed_for_test()
        .expect("dropping the computation should retire the baseline roots");
    assert_eq!(retired.root_entries(), empty.root_entries());
}

#[test]
fn small_and_large_seed_promotions_use_one_managed_root() {
    managed_seed_promotion(1);
    managed_seed_promotion(32);
}

#[test]
fn source_entry_remains_outside_demand_conversion_accounting() {
    WhnfComputation::reset_baseline_metrics_for_test();
    let context = context();
    let values = context.values();
    let (lazy, _) = values.rooted_error_lazy_for_test("W6G.3a source entry");
    let mut computation = WhnfComputation::from_lazy_source(lazy, values.runtime_id());

    assert!(computation.source_root().is_some());
    assert_eq!(
        WhnfComputation::baseline_metrics_for_test(),
        DurableWhnfBaselineMetrics::default()
    );

    let source_result = root(&context, text("source result"));
    computation.install_source_result(source_result);
    assert!(computation.source_root().is_none());
    assert_eq!(
        WhnfComputation::baseline_metrics_for_test(),
        DurableWhnfBaselineMetrics::default(),
        "source production is not an ordinary durable demand projection"
    );

    let registrations_before_poll = values.managed_root_registrations_for_test();
    let mut budget = WhnfStepBudget::new(0);
    let mut access_entries = 0;
    assert!(matches!(
        poll_with(
            &context,
            &mut computation,
            &mut budget,
            &mut access_entries,
            |_access, _work| panic!("an empty budget must not enter the reducer"),
        ),
        WhnfPoll::Yielded
    ));
    assert_eq!(
        values.managed_root_registrations_for_test() - registrations_before_poll,
        1,
        "first demand must publish exactly one managed state root"
    );
}
