//! Deterministic budget, yield, and fairness witnesses for Phase W7C.
//!
//! Every ordering assertion is driven by explicit single-poll calls. Repeated
//! execution under an uncontrolled scheduler is deliberately not evidence.

use std::sync::{Arc, Mutex};

use super::coordinator::{
    ClientDemandResult, ClientDemandSnapshot, CoordinatorSelection, ReflectionWorkSnapshot,
    ReflectionWorkState, SparkWorkPoll,
};
use super::{
    EvalContext, EvaluationMachinePoll, EvaluationPumpOutcome, EvaluationStepBudget,
    EvaluationTaskMachine, EvaluationWaitPoll, EvaluatorStepContext, OwnedEvalContext,
};
use crate::core::{
    CoreValueFactory, EvaluationHalt, LazyValue, PromisedValue, Value,
    max_runtime_value_access_depth_for_test, reset_runtime_value_access_depth_for_test,
};
use crate::number::Number;
use crate::runtime::{RuntimeIds, RuntimeValueRoot, allocate_evaluation_runtime_id};

const CHECKPOINT_DEPTH: usize = 96;
const FIFO_CHECKPOINT_DEPTH: usize = 192;

fn context() -> OwnedEvalContext {
    EvalContext::isolated(CoreValueFactory::new(
        allocate_evaluation_runtime_id(),
        RuntimeIds::new(),
    ))
}

fn return_first_capture(
    _context: &EvaluatorStepContext<'_>,
    captures: &[Value],
) -> Result<Value, EvaluationHalt> {
    Ok(captures[0].clone())
}

fn assigned_promise_chain_value(context: &EvalContext, depth: usize) -> Value {
    let mut current = Value::Number(Number::from_usize(depth));
    for _ in 0..depth {
        let promise = PromisedValue::new(context.values(), "W7C assigned promise alias");
        crate::core::set_test_promise(context.values(), &promise, current)
            .expect("a fresh promise alias should accept its assignment");
        current = Value::Promised(promise);
    }
    current
}

fn assigned_promise_chain_root(context: &EvalContext, depth: usize) -> RuntimeValueRoot {
    RuntimeValueRoot::new(
        context.values(),
        assigned_promise_chain_value(context, depth),
    )
}

fn checkpointed_promise_chain_root(context: &EvalContext, depth: usize) -> RuntimeValueRoot {
    let current = assigned_promise_chain_value(context, depth);
    context.values().construct_runtime_value_root(|access| {
        Value::Lazy(LazyValue::semantic_computation_in(
            access,
            "W7C checkpointed promise chain",
            Arc::from([access.duplicate_value(&current)]),
            return_first_capture,
        ))
    })
}

#[test]
fn exact_one_unit_polls_retain_one_checkpoint_without_root_or_allocation_churn() {
    let context = context();
    let root = checkpointed_promise_chain_root(&context, CHECKPOINT_DEPTH);
    let Value::Lazy(lazy) = root.clone_core_for_test() else {
        unreachable!("the application chain must publish one outer lazy")
    };
    let lazy_root = lazy.root(context.values());
    let wait = crate::eval::lazy_root_wait(&context, &lazy_root)
        .expect("the exact lazy route should be admitted");
    let coordinator = context
        .coordinator()
        .expect("coordinator should remain live");
    let work = coordinator
        .work_for_wait(&wait)
        .expect("the lazy route should own one canonical work record");

    reset_runtime_value_access_depth_for_test();
    let roots_before = context.values().managed_root_registrations_for_test();
    let slots_before = context.values().allocated_managed_slots_for_test();
    assert_eq!(
        context.pump_wait(&wait, 0),
        EvaluationPumpOutcome::BudgetExhausted
    );
    assert_eq!(
        context.values().managed_root_registrations_for_test(),
        roots_before,
        "a zero reservation must not publish a root"
    );
    assert_eq!(
        context.values().allocated_managed_slots_for_test(),
        slots_before,
        "a zero reservation must not allocate a checkpoint"
    );
    assert!(coordinator.work_dependency_by_id(work).is_none());

    assert_eq!(
        context.pump_wait(&wait, 1),
        EvaluationPumpOutcome::BudgetExhausted
    );
    context.values().with_runtime_value_access(|access| {
        assert!(
            lazy_root
                .access(&access)
                .expect("the lazy root belongs to this runtime")
                .checkpoint_snapshot()
                .is_some(),
            "the first semantic step should publish its checkpoint"
        );
    });
    let checkpoint_roots = context.values().managed_root_registrations_for_test();
    let checkpoint_slots = context.values().allocated_managed_slots_for_test();

    for _ in 0..8 {
        assert_eq!(
            context.pump_wait(&wait, 1),
            EvaluationPumpOutcome::BudgetExhausted
        );
        assert!(
            coordinator.work_dependency_by_id(work).is_none(),
            "an ordinary budget yield must not install a dependency"
        );
        assert_eq!(
            context.values().managed_root_registrations_for_test(),
            checkpoint_roots,
            "re-polling one retained checkpoint must not register roots"
        );
        assert_eq!(
            context.values().allocated_managed_slots_for_test(),
            checkpoint_slots,
            "re-polling one retained checkpoint must not allocate another checkpoint"
        );
    }
    assert_eq!(
        max_runtime_value_access_depth_for_test(),
        1,
        "checkpoint repolls must use bounded, non-nested managed access"
    );

    for _ in 0..CHECKPOINT_DEPTH * 4 {
        match context.pump_wait(&wait, 1) {
            EvaluationPumpOutcome::BudgetExhausted => {}
            EvaluationPumpOutcome::TargetReady => break,
            other => panic!("strict one-unit checkpoint should keep progressing: {other:?}"),
        }
    }
    let EvaluationWaitPoll::Complete(value) = context.poll_wait(&wait) else {
        panic!("the retained checkpoint should eventually complete")
    };
    assert_eq!(
        value.clone_core_for_test(),
        Value::Number(Number::from_usize(CHECKPOINT_DEPTH))
    );
}

struct RecordPollOrder {
    label: u8,
    polls: Arc<Mutex<Vec<u8>>>,
    yield_once: bool,
}

impl EvaluationTaskMachine for RecordPollOrder {
    fn poll(
        &mut self,
        context: &super::EvaluationPollContext,
        _step_budget: &mut EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        self.polls
            .lock()
            .expect("W7C poll trace was poisoned")
            .push(self.label);
        if std::mem::take(&mut self.yield_once) {
            EvaluationMachinePoll::Yielded
        } else {
            EvaluationMachinePoll::Complete(context.root_value(crate::core::keys::unit_value()))
        }
    }
}

#[test]
fn reflection_budget_yields_requeue_fifo_without_subscriptions() {
    let context = context();
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut tasks = Vec::new();
    for label in [1, 2] {
        let polls = order.clone();
        tasks.push(
            context
                .schedule_task(move |_| {
                    Ok(Box::new(RecordPollOrder {
                        label,
                        polls,
                        yield_once: true,
                    }))
                })
                .expect("the W7C reflection fixture should schedule"),
        );
    }

    for (index, task) in tasks.iter().enumerate() {
        assert_eq!(
            context
                .coordinator()
                .expect("coordinator should remain live")
                .poll_runtime_work_bounded(1),
            Some(1)
        );
        assert_eq!(
            task.wait().exact_subscription_count(),
            0,
            "task {} yielded on budget rather than a dependency",
            index + 1
        );
    }
    let snapshots = context
        .coordinator()
        .expect("coordinator should remain live")
        .reflection_snapshots(context.session_id());
    assert!(snapshots.iter().all(|snapshot| matches!(
        snapshot,
        ReflectionWorkSnapshot {
            state: ReflectionWorkState::Queued,
            ..
        }
    )));

    for _ in 0..2 {
        assert_eq!(
            context
                .coordinator()
                .expect("coordinator should remain live")
                .poll_runtime_work_bounded(1),
            Some(1)
        );
    }
    assert_eq!(
        order
            .lock()
            .expect("W7C poll trace was poisoned")
            .as_slice(),
        [1, 2, 1, 2]
    );
    assert!(tasks.iter().all(|task| matches!(
        context.poll_reflection_task(task),
        EvaluationWaitPoll::Complete(_)
    )));
}

#[test]
fn client_budget_yields_requeue_fifo_without_subscriptions() {
    let context = context();
    let coordinator = context
        .coordinator()
        .expect("coordinator should remain live");
    let clients = [11, 22].map(|_| {
        context
            .demand_whnf(assigned_promise_chain_root(&context, FIFO_CHECKPOINT_DEPTH))
            .expect("the W7C client fixture should admit")
    });

    for expected in [0, 1, 0, 1] {
        let claimed = coordinator
            .claim_ready_client_demand_for_test()
            .expect("one client should remain queued");
        assert!(matches!(
            coordinator.client_demand_snapshot(clients[expected].work()),
            Some(ClientDemandSnapshot::Running)
        ));
        assert!(matches!(
            coordinator.client_demand_snapshot(clients[1 - expected].work()),
            Some(ClientDemandSnapshot::Queued)
        ));
        coordinator.poll_claimed_client_demand(claimed);
        assert!(matches!(
            coordinator.client_demand_snapshot(clients[expected].work()),
            Some(ClientDemandSnapshot::Queued)
        ));
        assert!(
            coordinator
                .work_dependency_by_id(clients[expected].work())
                .is_none(),
            "an ordinary client budget yield must not publish a dependency"
        );
    }

    for client in clients {
        assert!(client.poll().is_none());
        client.abandon();
    }
}

#[test]
fn spark_budget_yield_requeues_the_same_record_without_a_dependency() {
    let context = context();
    let coordinator = context
        .coordinator()
        .expect("coordinator should remain live");
    coordinator.executor_started(1);
    context.spark_root(assigned_promise_chain_root(&context, CHECKPOINT_DEPTH));

    let CoordinatorSelection::Spark(first) = coordinator.select_worker() else {
        panic!("the admitted spark should be selected")
    };
    let work = first.id_for_test();
    assert!(!first.has_prior_dependency_for_test());
    coordinator.poll_claimed_spark(first);
    assert_eq!(coordinator.spark_work_counts(), (1, 0, 0));
    assert!(coordinator.work_dependency_by_id(work).is_none());

    let CoordinatorSelection::Spark(second) = coordinator.select_worker() else {
        panic!("the yielded spark should be selected again")
    };
    assert_eq!(second.id_for_test(), work);
    assert!(!second.has_prior_dependency_for_test());
    coordinator.release_spark(second, SparkWorkPoll::Complete);
    assert_eq!(coordinator.spark_work_counts(), (0, 0, 0));
}

#[test]
fn background_selection_excludes_foreground_clients_even_when_both_are_ready() {
    let context = context();
    let coordinator = context
        .coordinator()
        .expect("coordinator should remain live");
    let client = context
        .demand_whnf(RuntimeValueRoot::new(
            context.values(),
            Value::Number(31.into()),
        ))
        .expect("the foreground client should admit");
    let task = context
        .schedule_task(|_| {
            Ok(Box::new(RecordPollOrder {
                label: 1,
                polls: Arc::new(Mutex::new(Vec::new())),
                yield_once: false,
            }))
        })
        .expect("the background task should schedule");

    assert_eq!(coordinator.poll_runtime_work_bounded(1), Some(1));
    assert!(matches!(
        context.poll_reflection_task(&task),
        EvaluationWaitPoll::Complete(_)
    ));
    assert!(client.poll().is_none());
    assert!(matches!(
        coordinator.client_demand_snapshot(client.work()),
        Some(ClientDemandSnapshot::Queued)
    ));

    let claimed = coordinator
        .claim_client_demand(client.work())
        .expect("only the foreground owner should claim its client");
    coordinator.poll_claimed_client_demand(claimed);
    assert!(matches!(
        client.poll(),
        Some(ClientDemandResult::Complete(_))
    ));
}
