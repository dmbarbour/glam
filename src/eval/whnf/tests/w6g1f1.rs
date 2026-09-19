use crate::core::{CoreValueFactory, DeferredValueId, Value};
use crate::evaluation::{EvalContext, EvaluationPollContext};
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::managed_state::ManagedLazyCheckpointEdge;
use super::*;

fn context() -> crate::evaluation::OwnedEvalContext {
    EvalContext::isolated(CoreValueFactory::new(
        allocate_evaluation_runtime_id(),
        RuntimeIds::new(),
    ))
}

fn install_self_checkpoint(
    context: &crate::evaluation::OwnedEvalContext,
    root: &crate::core::ManagedLazyRoot,
    lazy: &crate::core::LazyValue,
) {
    let poll = EvaluationPollContext::for_context(context);
    poll.with_value_access(context, |access| {
        let work = RegionalWhnfWork::from_parts(
            &access,
            Value::Lazy(lazy.clone()),
            Vec::new(),
            Default::default(),
            Some(root.id()),
            None,
        );
        let checkpoint = ManagedLazyCheckpointEdge::allocate_regional_in(&access, work)
            .expect("managed lazy checkpoint should fit one collector slot");
        assert!(
            access
                .lazy_root(root)
                .install_checkpoint(checkpoint)
                .is_ok(),
            "one unresolved source must accept its first checkpoint"
        );
    });
}

#[test]
fn unreachable_lazy_checkpoint_self_cycle_is_collected() {
    let context = context();
    let values = context.values();
    let baseline = values
        .collect_managed_for_test()
        .expect("checkpoint cycle fixture should start collectible");
    let (root, lazy) = values.rooted_error_lazy_for_test("W6G.1f.1 self cycle");
    install_self_checkpoint(&context, &root, &lazy);

    drop((lazy, root));
    let collected = values
        .collect_managed_for_test()
        .expect("unreachable lazy/checkpoint cycle should collect");
    assert_eq!(collected.root_entries(), baseline.root_entries());
    assert_eq!(collected.marked_slots(), baseline.marked_slots());
}

#[test]
fn reachable_lazy_retains_and_resumes_its_checkpoint() {
    let context = context();
    let values = context.values();
    let baseline = values
        .collect_managed_for_test()
        .expect("checkpoint retention fixture should start collectible");
    let (root, lazy) = values.rooted_error_lazy_for_test("W6G.1f.1 retained checkpoint");
    let lazy_id = root.id();
    install_self_checkpoint(&context, &root, &lazy);

    let retained = values
        .collect_managed_for_test()
        .expect("rooted lazy must retain its checkpoint");
    assert_eq!(retained.root_entries(), baseline.root_entries() + 1);
    assert!(retained.marked_slots() >= baseline.marked_slots() + 2);

    let poll = EvaluationPollContext::for_context(&context);
    poll.with_value_access(&context, |access| {
        let checkpoint = access
            .lazy_root(&root)
            .checkpoint_snapshot()
            .expect("reachable lazy must retain its checkpoint edge");
        let checkpoint = checkpoint.access(&access);
        checkpoint
            .with_state_transition(|state| {
                assert_eq!(state.source_owner, Some(lazy_id));
                let Value::Lazy(focus) = &state.focus else {
                    panic!("checkpoint must retain its original lazy focus")
                };
                assert_eq!(access.lazy(focus).id(), lazy_id);
                state.followed.insert(DeferredValueId::Lazy(lazy_id));
            })
            .expect("retained checkpoint should resume without reconstruction");
    });

    values
        .collect_managed_for_test()
        .expect("updated rooted checkpoint must remain traceable");
    poll.with_value_access(&context, |access| {
        let checkpoint = access
            .lazy_root(&root)
            .checkpoint_snapshot()
            .expect("updated checkpoint must remain installed");
        assert_eq!(
            checkpoint
                .access(&access)
                .inspect(|state| state.followed.contains(&DeferredValueId::Lazy(lazy_id))),
            Ok(true),
            "checkpoint mutation must survive collection and later access"
        );
    });
}
