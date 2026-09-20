use std::collections::BTreeSet;
use std::sync::Arc;

use crate::core::{CoreValueFactory, DeferredValueId, LazyValue, PromisedValue, Value};
use crate::evaluation::{EvalContext, EvaluationPollContext};
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::*;

fn isolated_values() -> CoreValueFactory {
    CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
}

fn sentinel(values: &CoreValueFactory, label: String) -> Value {
    Value::Lazy(LazyValue::semantic_thunk(values, label, |_| {
        Ok(Value::Number(0.into()))
    }))
}

fn lazy_id(access: &EvaluationValueAccess<'_>, value: &Value) -> crate::core::LazyId {
    let Value::Lazy(lazy) = value else {
        panic!("NC1A sentinel must remain lazy")
    };
    access.lazy(lazy).id()
}

fn retained_values(work: &WhnfState) -> Vec<&Value> {
    let mut values = vec![&work.focus];
    for frame in &work.frames {
        match frame {
            WhnfContinuation::Generic(frame) => values.extend(&frame.retained),
            WhnfContinuation::Application { arguments, .. } => values.extend(arguments),
            WhnfContinuation::DictionaryApplication {
                effect_payload,
                remaining_effect_values,
                apply_member,
                ..
            } => {
                values.push(effect_payload);
                values.extend(remaining_effect_values);
                values.extend(apply_member);
            }
            WhnfContinuation::SemanticUndefined { ancestors, .. } => {
                for ancestor in ancestors {
                    values.extend(&ancestor.members);
                }
            }
            WhnfContinuation::StaticAccess { .. } => {}
        }
    }
    values
}

#[test]
fn net_state_round_trip_preserves_empty_frames_and_scalar_identity() {
    let values = isolated_values();
    let context = EvalContext::isolated(values);
    let poll = EvaluationPollContext::for_context(&context);

    poll.with_value_access(&context, |access| {
        let regional = RegionalWhnfWork::from_parts(
            &access,
            Value::Number(17.into()),
            Vec::new(),
            BTreeSet::new(),
            None,
            None,
        );
        let state = NetWhnfState::from_regional(&access, regional);
        let projected = state.into_regional(&access);

        assert_eq!(projected.focus, Value::Number(17.into()));
        assert!(projected.frames.is_empty());
        assert!(projected.followed.is_empty());
        assert_eq!(projected.source_owner, None);
        assert!(projected.cycle_promise.is_none());
    });
}

fn container_identities(work: &WhnfState) -> Vec<(&'static str, usize, usize, usize)> {
    let mut identities = vec![(
        "frames",
        work.frames.as_ptr() as usize,
        work.frames.len(),
        work.frames.capacity(),
    )];
    for frame in &work.frames {
        match frame {
            WhnfContinuation::Generic(frame) => identities.push((
                "generic retained",
                frame.retained.as_ptr() as usize,
                frame.retained.len(),
                frame.retained.capacity(),
            )),
            WhnfContinuation::Application { arguments, .. } => identities.push((
                "application arguments",
                arguments.as_ptr() as usize,
                arguments.len(),
                arguments.capacity(),
            )),
            WhnfContinuation::DictionaryApplication {
                remaining_effect_values,
                ..
            } => identities.push((
                "remaining effect values",
                remaining_effect_values.as_ptr() as usize,
                remaining_effect_values.len(),
                remaining_effect_values.capacity(),
            )),
            WhnfContinuation::SemanticUndefined { ancestors, .. } => {
                identities.push((
                    "undefined ancestors",
                    ancestors.as_ptr() as usize,
                    ancestors.len(),
                    ancestors.capacity(),
                ));
                for ancestor in ancestors {
                    identities.push((
                        "undefined members",
                        ancestor.members.as_ptr() as usize,
                        ancestor.members.len(),
                        ancestor.members.capacity(),
                    ));
                }
            }
            WhnfContinuation::StaticAccess { keys, .. } => identities.push((
                "static keys",
                keys.as_ptr() as usize,
                keys.len(),
                keys.len(),
            )),
        }
    }
    identities
}

#[test]
fn regional_net_role_handoffs_preserve_every_container_allocation() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);

    poll.with_value_access(&context, |access| {
        let frames = Vec::with_capacity(8);
        let mut regional = RegionalWhnfWork::from_parts(
            &access,
            Value::Number(0.into()),
            frames,
            BTreeSet::new(),
            None,
            None,
        );
        regional.frames.extend([
            WhnfContinuation::Generic(WhnfFrame {
                kind: WhnfFrameKind::OrderedOperands,
                cursor: 1,
                retained: Vec::with_capacity(3),
            }),
            WhnfContinuation::Application {
                arguments: Vec::with_capacity(4),
                next: 0,
            },
            WhnfContinuation::DictionaryApplication {
                effect_payload: Value::Number(1.into()),
                remaining_effect_values: Vec::with_capacity(5),
                next_effect_value: 0,
                apply_member: None,
            },
            WhnfContinuation::SemanticUndefined {
                purpose: UndefinedPurpose::EffectExtra,
                ancestors: vec![WhnfUndefinedDictionary {
                    members: Vec::with_capacity(6),
                    next: 0,
                }],
                phase: UndefinedPhase::Inspect,
            },
            WhnfContinuation::StaticAccess {
                keys: Arc::from([crate::core::Key::atom_from_text("key")]),
                next: 0,
            },
        ]);
        let expected = container_identities(&regional);
        let roots_before = values.managed_root_registrations_for_test();

        let net = NetWhnfState::from_regional(&access, regional);
        assert_eq!(container_identities(&net.0), expected);
        let regional = net.into_regional(&access);
        assert_eq!(container_identities(&regional), expected);
        let net = NetWhnfState::from_regional(&access, regional);
        assert_eq!(container_identities(&net.0), expected);
        let regional = net.into_regional(&access);
        assert_eq!(container_identities(&regional), expected);
        assert_eq!(values.managed_root_registrations_for_test(), roots_before);
    });

    let source = include_str!("../../whnf.rs");
    let publish = source_section(
        source,
        "pub(crate) fn from_regional(",
        "/// Claims the complete state",
    );
    let claim = source_section(
        source,
        "pub(crate) fn into_regional(",
        "/// Drives one bounded callback-free quantum",
    );
    for handoff in [publish, claim] {
        for forbidden in [".iter()", ".map(", ".collect(", "duplicate_value", "clone("] {
            assert!(
                !handoff.contains(forbidden),
                "a regional/net role handoff must not contain `{forbidden}`"
            );
        }
    }
}

#[test]
fn net_state_trace_retains_every_value_position_through_collection() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let roots_before = values.managed_root_registrations_for_test();
    let baseline_marked = values.collect_managed_for_test().unwrap().marked_slots();

    let (state_root, expected_lazy_ids, source_owner, promise_id) =
        poll.with_value_access(&context, |access| {
            let mut next = 0;
            let mut make = || {
                let value = sentinel(&values, format!("NC1A edge {next}"));
                next += 1;
                value
            };

            let focus = make();
            let generic = vec![make(), make()];
            let application = vec![make(), make()];
            let effect_payload = make();
            let remaining_effect_values = vec![make(), make()];
            let apply_member = make();
            let first_ancestor = vec![make(), make()];
            let second_ancestor = vec![make(), make()];
            let expected_lazy_ids = [
                std::slice::from_ref(&focus),
                generic.as_slice(),
                application.as_slice(),
                std::slice::from_ref(&effect_payload),
                remaining_effect_values.as_slice(),
                std::slice::from_ref(&apply_member),
                first_ancestor.as_slice(),
                second_ancestor.as_slice(),
            ]
            .into_iter()
            .flatten()
            .map(|value| lazy_id(&access, value))
            .collect::<Vec<_>>();
            assert_eq!(expected_lazy_ids.len(), next);

            let source_owner = expected_lazy_ids[1];
            let promise = PromisedValue::new(&values, "NC1A promise breadcrumb");
            let promise_id = access.promise(&promise).id();
            let followed = [
                DeferredValueId::Lazy(expected_lazy_ids[0]),
                DeferredValueId::Promise(promise_id),
            ]
            .into_iter()
            .collect();
            let regional = RegionalWhnfWork::from_parts(
                &access,
                focus,
                vec![
                    WhnfContinuation::Generic(WhnfFrame {
                        kind: WhnfFrameKind::OrderedOperands,
                        cursor: 2,
                        retained: generic,
                    }),
                    WhnfContinuation::Application {
                        arguments: application,
                        next: 1,
                    },
                    WhnfContinuation::DictionaryApplication {
                        effect_payload,
                        remaining_effect_values,
                        next_effect_value: 1,
                        apply_member: Some(apply_member),
                    },
                    WhnfContinuation::SemanticUndefined {
                        purpose: UndefinedPurpose::EffectExtra,
                        ancestors: vec![
                            WhnfUndefinedDictionary {
                                members: first_ancestor,
                                next: 1,
                            },
                            WhnfUndefinedDictionary {
                                members: second_ancestor,
                                next: 2,
                            },
                        ],
                        phase: UndefinedPhase::ReturnTrue,
                    },
                    WhnfContinuation::StaticAccess {
                        keys: Arc::from([
                            crate::core::Key::atom_from_text("first"),
                            crate::core::Key::atom_from_text("second"),
                        ]),
                        next: 1,
                    },
                ],
                followed,
                Some(source_owner),
                Some(promise),
            );
            let state = NetWhnfState::from_regional(&access, regional);
            let allocator = access
                .values()
                .allocator::<NetWhnfState>()
                .expect("NC1A state must fit one managed run");
            let state_root = access.values().root(allocator.alloc(state));
            (state_root, expected_lazy_ids, source_owner, promise_id)
        });

    assert_eq!(
        values.managed_root_registrations_for_test(),
        roots_before + 1,
        "the net state owns traced edges, not one registered root per value"
    );
    let report = values.collect_managed_for_test().unwrap();
    assert_eq!(
        report.marked_slots(),
        baseline_marked + expected_lazy_ids.len() + 2,
        "the rooted state, every lazy sentinel, and the promise breadcrumb must survive"
    );

    poll.with_value_access(&context, |access| {
        let state = access.values().get(&state_root);
        let actual_lazy_ids = retained_values(&state.0)
            .into_iter()
            .map(|value| lazy_id(&access, value))
            .collect::<Vec<_>>();
        assert_eq!(actual_lazy_ids, expected_lazy_ids);
        assert_eq!(state.0.frames.len(), 5);
        assert_eq!(state.0.source_owner, Some(source_owner));
        assert_eq!(
            state
                .0
                .cycle_promise
                .as_ref()
                .map(|promise| access.promise(promise).id()),
            Some(promise_id)
        );
        assert_eq!(
            state.0.followed,
            [
                DeferredValueId::Lazy(expected_lazy_ids[0]),
                DeferredValueId::Promise(promise_id),
            ]
            .into_iter()
            .collect()
        );
    });
}

fn shell_work(access: &EvaluationValueAccess<'_>) -> NetWhnfState {
    NetWhnfState::from_regional(
        access,
        RegionalWhnfWork::from_parts(
            access,
            Value::Number(0.into()),
            Vec::new(),
            BTreeSet::new(),
            None,
            None,
        ),
    )
}

fn reduce_shell(
    _access: &EvaluationValueAccess<'_>,
    work: &mut RegionalWhnfState<'_>,
) -> RegionalWhnfStep {
    if work.focus == Value::Number(0.into()) {
        RegionalWhnfStep::Delegate(Value::Number(1.into()))
    } else if work.focus == Value::Number(1.into()) {
        RegionalWhnfStep::Delegate(Value::Number(2.into()))
    } else {
        RegionalWhnfStep::Ready(Value::Number(3.into()))
    }
}

#[test]
fn net_state_and_durable_work_share_exact_budget_split_semantics() {
    let values = isolated_values();
    let context = EvalContext::isolated(values);
    let poll = EvaluationPollContext::for_context(&context);

    poll.with_value_access(&context, |access| {
        let mut uninterrupted_budget = WhnfStepBudget::new(4);
        let NetWhnfDrive::Ready(uninterrupted) =
            shell_work(&access).drive_in(&access, &mut uninterrupted_budget, reduce_shell)
        else {
            panic!("uninterrupted shell chain must complete")
        };
        assert_eq!(uninterrupted, Value::Number(3.into()));
        assert_eq!(uninterrupted_budget.spent(), 3);
        assert_eq!(uninterrupted_budget.remaining(), 1);

        for first_quantum in 0..=3 {
            let mut state = shell_work(&access);
            let mut first_budget = WhnfStepBudget::new(first_quantum);
            let first = state.drive_in(&access, &mut first_budget, reduce_shell);
            assert_eq!(first_budget.spent(), first_quantum.min(3));
            if first_quantum == 3 {
                let NetWhnfDrive::Ready(value) = first else {
                    panic!("three transitions must complete the shell chain")
                };
                assert_eq!(value, uninterrupted);
                continue;
            }
            let NetWhnfDrive::Yielded(next) = first else {
                panic!("a split before completion must preserve a successor")
            };
            state = next;
            let mut rest_budget = WhnfStepBudget::new(3 - first_quantum);
            let NetWhnfDrive::Ready(value) =
                state.drive_in(&access, &mut rest_budget, reduce_shell)
            else {
                panic!("the remaining exact allowance must complete the shell chain")
            };
            assert_eq!(value, uninterrupted);
            assert_eq!(rest_budget.spent(), 3 - first_quantum);
            assert_eq!(rest_budget.remaining(), 0);
        }
    });
}

#[test]
fn net_driver_retains_frame_state_on_yield_boundary_and_failure() {
    let values = isolated_values();
    let context = EvalContext::isolated(values);
    let poll = EvaluationPollContext::for_context(&context);

    poll.with_value_access(&context, |access| {
        let make_state = || {
            NetWhnfState::from_regional(
                &access,
                RegionalWhnfWork::from_parts(
                    &access,
                    Value::Number(0.into()),
                    vec![WhnfContinuation::Application {
                        arguments: vec![Value::Number(10.into()), Value::Number(11.into())],
                        next: 0,
                    }],
                    BTreeSet::new(),
                    None,
                    None,
                ),
            )
        };
        let advance_frame = |_access: &EvaluationValueAccess<'_>,
                             work: &mut RegionalWhnfState<'_>| {
            let WhnfContinuation::Application { next, .. } = &mut work.frames[0] else {
                unreachable!()
            };
            *next = 1;
            RegionalWhnfStep::Delegate(Value::Number(1.into()))
        };

        let mut zero = WhnfStepBudget::new(0);
        let NetWhnfDrive::Yielded(unchanged) =
            make_state().drive_in(&access, &mut zero, advance_frame)
        else {
            panic!("zero allowance must yield before frame mutation")
        };
        let unchanged = unchanged.into_regional(&access);
        let WhnfContinuation::Application { arguments, next } = &unchanged.frames[0] else {
            unreachable!()
        };
        assert_eq!(*next, 0);
        assert_eq!(
            arguments,
            &[Value::Number(10.into()), Value::Number(11.into())]
        );

        let mut one = WhnfStepBudget::new(1);
        let NetWhnfDrive::Yielded(advanced) =
            make_state().drive_in(&access, &mut one, advance_frame)
        else {
            panic!("one completed transition must yield its complete successor")
        };
        let advanced = advanced.into_regional(&access);
        assert_eq!(advanced.focus, Value::Number(1.into()));
        let WhnfContinuation::Application { arguments, next } = &advanced.frames[0] else {
            unreachable!()
        };
        assert_eq!(*next, 1);
        assert_eq!(
            arguments,
            &[Value::Number(10.into()), Value::Number(11.into())]
        );

        let boundary_reducer = |_access: &EvaluationValueAccess<'_>,
                                work: &mut RegionalWhnfState<'_>| {
            let WhnfContinuation::Application { next, .. } = &mut work.frames[0] else {
                unreachable!()
            };
            *next = 1;
            work.focus = Value::Number(1.into());
            RegionalWhnfStep::Boundary(RegionalBoundaryRequest::External(
                WhnfExternalBoundary::Reflection,
            ))
        };
        let mut boundary_budget = WhnfStepBudget::new(1);
        let NetWhnfDrive::Boundary { state, request } =
            make_state().drive_in(&access, &mut boundary_budget, boundary_reducer)
        else {
            panic!("boundary transition must publish its complete successor")
        };
        assert!(matches!(
            request,
            RegionalBoundaryRequest::External(WhnfExternalBoundary::Reflection)
        ));
        let state = state.into_regional(&access);
        assert_eq!(state.focus, Value::Number(1.into()));
        let WhnfContinuation::Application { arguments, next } = &state.frames[0] else {
            unreachable!()
        };
        assert_eq!(*next, 1);
        assert_eq!(
            arguments,
            &[Value::Number(10.into()), Value::Number(11.into())]
        );

        let expected = Arc::new(crate::core::EvaluationFailure::message("NC1B failure"));
        let mut failed_budget = WhnfStepBudget::new(1);
        let NetWhnfDrive::Failed(actual) =
            make_state().drive_in(&access, &mut failed_budget, |_access, _work| {
                RegionalWhnfStep::Failed(Arc::clone(&expected))
            })
        else {
            panic!("failure transition must remain terminal")
        };
        assert!(Arc::ptr_eq(&actual, &expected));
    });
}

fn source_section<'source>(source: &'source str, start: &str, end: &str) -> &'source str {
    let (_, section) = source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing inventory start `{start}`"));
    section
        .split_once(end)
        .unwrap_or_else(|| panic!("missing inventory end `{end}`"))
        .0
}

#[test]
fn callable_checkpoint_reachability_inventory_starts_frame_free() {
    let net = include_str!("../../net.rs");
    assert!(
        !net.contains("fn lower_core_callable_in("),
        "NC6 must not retain a synchronous deferred-callable forcing seam"
    );

    let production_driver = source_section(
        net,
        "fn drive_original_callable_whnf(",
        "#[cfg(test)]\npub(super) fn classify_core_callable(",
    );
    assert_eq!(
        production_driver
            .matches("RegionalWhnfWork::from_focus")
            .count(),
        1,
        "production callable demand must have one canonical frame-free constructor"
    );
    for producer_only_state in [
        "from_application_checkpoint_in",
        "from_static_access_checkpoint_in",
        "with_source_owner",
        "cycle_promise",
        "frames.push",
    ] {
        assert!(
            !production_driver.contains(producer_only_state),
            "production callable admission must not synthesize producer state: {producer_only_state}"
        );
    }

    let value = include_str!("../../value.rs");
    let producer_dispatch = source_section(
        value,
        "match source {",
        "if matches!(\n                    self.work,",
    );
    for producer_owned_family in [
        "LazySource::Application(application)",
        "LazySource::ReflectionTask(computation)",
        "LazySource::Access { path, arguments }",
    ] {
        assert!(
            producer_dispatch.contains(producer_owned_family),
            "lazy-source work must remain owned by its canonical producer: {producer_owned_family}"
        );
    }

    let whnf = include_str!("../../whnf.rs");
    let initial = source_section(whnf, "pub(crate) fn from_focus(", "    fn from_parts(");
    assert!(initial.contains("Vec::new(), BTreeSet::new(), None, None"));

    let source_owner_transition = source_section(
        whnf,
        "pub(crate) fn with_source_owner(",
        "    pub(crate) fn from_application_checkpoint_in(",
    );
    assert!(
        source_owner_transition.contains("self.0.source_owner = Some(source_owner)"),
        "source-owner modification must remain confined to regional WHNF work"
    );
    assert_eq!(
        source_owner_transition
            .matches("self.0.source_owner = Some(source_owner)")
            .count(),
        1,
        "the regional modifier must install source ownership exactly once"
    );
    assert_eq!(
        whnf.matches("self.0.source_owner = Some(source_owner)")
            .count(),
        1,
        "new source-owner transitions require an NC5D inventory decision"
    );

    let semantic = source_section(
        whnf,
        "fn reduce_semantic_shell(",
        "enum DirectApplicationStep",
    );
    assert_eq!(
        semantic.matches(".cycle_promise = Some(").count(),
        1,
        "only following an assigned promise may create the promise breadcrumb"
    );
    assert_eq!(
        whnf.matches("work.0.cycle_promise = Some(").count(),
        1,
        "new promise-breadcrumb transitions require an NC5D inventory decision"
    );

    for frame_transition in [
        "fn resume_static_access(",
        "fn advance_application(",
        "fn begin_dictionary_application(",
        "fn begin_semantic_undefined(",
        "fn resume_semantic_undefined(",
        "fn finish_semantic_undefined(",
    ] {
        assert!(
            whnf.contains(frame_transition),
            "the complete frame-transition inventory must retain {frame_transition}"
        );
    }
}
