use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use glam_gc::{EdgeTransitionObservation, Trace};

use crate::core::{CoreValueFactory, Value};
use crate::evaluation::{EvalContext, EvaluationPollContext};
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::managed_state::{ManagedWhnfAccessError, ManagedWhnfRoot};
use super::*;

fn context() -> crate::evaluation::OwnedEvalContext {
    EvalContext::isolated(CoreValueFactory::new(
        allocate_evaluation_runtime_id(),
        RuntimeIds::new(),
    ))
}

fn work(access: &EvaluationValueAccess<'_>, focus: Value) -> RegionalWhnfWork {
    RegionalWhnfWork::from_parts(
        access,
        focus,
        vec![WhnfContinuation::Generic(WhnfFrame {
            kind: WhnfFrameKind::CollectionWalk,
            cursor: 0,
            retained: vec![Value::Number(1.into())],
        })],
        BTreeSet::new(),
        None,
        None,
    )
}

#[test]
fn managed_root_projects_only_under_matching_runtime_access() {
    let owner = context();
    let owner_poll = EvaluationPollContext::for_context(&owner);
    let root = owner_poll.with_value_access(&owner, |access| {
        ManagedWhnfRoot::from_regional_in(&access, work(&access, Value::Number(7.into())))
            .expect("managed WHNF state should fit one collector slot")
    });
    assert_eq!(root.runtime_id(), owner.values().runtime_id());

    owner_poll.with_value_access(&owner, |access| {
        let state = root
            .access(&access)
            .expect("owner access must project root");
        assert_eq!(
            state.inspect(|state| state.focus == Value::Number(7.into())),
            Ok(true)
        );
    });

    let unrelated = context();
    EvaluationPollContext::for_context(&unrelated).with_value_access(&unrelated, |access| {
        assert!(matches!(
            root.access(&access),
            Err(ManagedWhnfAccessError::RuntimeMismatch)
        ));
    });
}

#[test]
fn managed_transition_reports_complete_before_and_after_edge_sets() {
    let context = context();
    let values = context.values();
    let (old_root, old) = values.rooted_error_lazy_for_test("W6G.3c old edge");
    let (new_root, new) = values.rooted_error_lazy_for_test("W6G.3c new edge");
    let probe = values.install_edge_transition_probe_for_test(EdgeTransitionObservation::Both);
    let poll = EvaluationPollContext::for_context(&context);

    let state = poll.with_value_access(&context, |access| {
        ManagedWhnfRoot::from_regional_in(&access, work(&access, Value::Lazy(old)))
            .expect("managed WHNF state should fit one collector slot")
    });
    poll.with_value_access(&context, |access| {
        state
            .access(&access)
            .expect("state and access must share one runtime")
            .with_state_transition(|state| {
                state.focus = Value::Lazy(new);
            })
            .expect("fresh managed WHNF state must not be poisoned");
    });

    let records = probe.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].leaving_edges(), 1);
    assert_eq!(records[0].adding_edges(), 1);
    drop((old_root, new_root));
}

#[test]
fn forced_collection_traces_every_edge_owned_by_the_managed_state() {
    let context = context();
    let values = context.values();
    let baseline = values
        .collect_managed_for_test()
        .expect("managed WHNF trace fixture should begin collectible");
    let (sentinel_root, sentinel) = values.rooted_error_lazy_for_test("W6G.3c trace sentinel");
    let poll = EvaluationPollContext::for_context(&context);
    let sentinel_id = poll.with_value_access(&context, |access| access.lazy(&sentinel).id());
    let state = poll.with_value_access(&context, |access| {
        ManagedWhnfRoot::from_regional_in(&access, work(&access, Value::Lazy(sentinel)))
            .expect("managed WHNF state should fit one collector slot")
    });
    drop(sentinel_root);

    let retained = values
        .collect_managed_for_test()
        .expect("rooted managed WHNF state should trace its sentinel");
    assert_eq!(retained.root_entries(), baseline.root_entries() + 1);
    assert!(retained.marked_slots() >= baseline.marked_slots() + 2);
    poll.with_value_access(&context, |access| {
        let state = state.access(&access).expect("state must remain rooted");
        let observed = state
            .inspect(|state| {
                let Value::Lazy(sentinel) = &state.focus else {
                    panic!("managed focus must retain the lazy sentinel")
                };
                access.lazy(sentinel).id()
            })
            .expect("collection must not poison managed state");
        assert_eq!(observed, sentinel_id);
    });

    drop(state);
    let retired = values
        .collect_managed_for_test()
        .expect("dropping the managed root should retire its graph");
    assert_eq!(retired.root_entries(), baseline.root_entries());
}

#[test]
fn poisoned_state_remains_traceable_but_repolling_observes_poison() {
    let context = context();
    let values = context.values();
    let baseline = values
        .collect_managed_for_test()
        .expect("poison fixture should begin collectible");
    let (sentinel_root, sentinel) = values.rooted_error_lazy_for_test("W6G.3c poison sentinel");
    let poll = EvaluationPollContext::for_context(&context);
    let state = poll.with_value_access(&context, |access| {
        ManagedWhnfRoot::from_regional_in(&access, work(&access, Value::Lazy(sentinel)))
            .expect("managed WHNF state should fit one collector slot")
    });
    drop(sentinel_root);

    let panic = catch_unwind(AssertUnwindSafe(|| {
        poll.with_value_access(&context, |access| {
            let state = state
                .access(&access)
                .expect("state must project before unwind");
            let _ = state.with_state_transition::<()>(|_| {
                panic!("injected W6G.3c evaluator unwind");
            });
        });
    }));
    assert!(panic.is_err());

    let retained = values
        .collect_managed_for_test()
        .expect("poisoned managed WHNF state must remain traceable");
    assert_eq!(retained.root_entries(), baseline.root_entries() + 1);
    assert!(retained.marked_slots() >= baseline.marked_slots() + 2);
    poll.with_value_access(&context, |access| {
        let state = state
            .access(&access)
            .expect("poison does not change provenance");
        assert!(matches!(
            state.inspect(|_| ()),
            Err(ManagedWhnfAccessError::Poisoned)
        ));
    });
}

#[test]
fn managed_cell_uses_the_canonical_trace_and_slot_policy() {
    assert_eq!(
        <managed_state::ManagedWhnfCell as Trace>::REQUESTED_SLOT_SIZE,
        Some(crate::core::managed_slot_extent::<
            managed_state::ManagedWhnfCell,
        >())
    );
}
