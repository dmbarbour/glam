//! Non-reducing semantic-edge adapters for interaction-net payloads.

use super::{CompatibilityValueEdges, visit_values};
use crate::core::{EvaluationHalt, EvaluationHaltPayload, LazySource, Value};
use crate::core_net::CoreOperator;
use glam_gc::Visitor;

impl CompatibilityValueEdges for CoreOperator {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        match self {
            Self::ApplyArity { supplied, .. }
            | Self::FunctionCaptures { supplied, .. }
            | Self::ComputationCaptures { supplied, .. }
            | Self::Dict { supplied, .. }
            | Self::List { supplied, .. }
            | Self::Access { supplied, .. }
            | Self::Request { supplied, .. } => visit_values(supplied, visit),
            Self::Builtin(call) => call.visit_compatibility_value_edges(visit),
            Self::Applicable(function) => visit(function),
        }
    }
}

pub(crate) fn visit_halt_value_edges(halt: &EvaluationHalt, visit: &mut dyn FnMut(&Value)) {
    match halt.payload() {
        EvaluationHaltPayload::Failure(failure) => {
            failure.visit_compatibility_value_edges(visit);
        }
        EvaluationHaltPayload::Blocked => {}
        // The registered root is already an independent collector root. It
        // must not also appear as an interior compatibility edge.
        EvaluationHaltPayload::UnassignedPromise => {}
    }
}

/// Traces the exact managed net edge held by a lazy source, if any.
///
/// This is not a compatibility projection: the managed lazy cell invokes it
/// directly from its collector trace. The exhaustive match must remain free of
/// reduction, materialization, and semantic callbacks.
pub(crate) fn trace_lazy_source_managed_net_edges(source: &LazySource, visitor: &mut Visitor<'_>) {
    match source {
        LazySource::NetComputation(net) => net.runtime().trace_managed_edge(visitor),
        LazySource::FunctionCall { function, .. } => {
            function.stage().runtime().trace_managed_edge(visitor);
        }
        LazySource::Error
        | LazySource::ComputedFixpoint(_)
        | LazySource::SemanticComputation(_)
        | LazySource::HostCall(_)
        | LazySource::ReflectionTask(_)
        | LazySource::Access { .. }
        | LazySource::Application(_)
        | LazySource::Builtin(_)
        | LazySource::NetConstruction(_) => {}
        #[cfg(test)]
        LazySource::SemanticThunk(_) => {}
    }
}

/// Traces the exact managed function-code net edge held by an operator.
///
/// Raw semantic values remain handled by `CompatibilityValueEdges` until the
/// value-representation project replaces their structural interiors.
pub(crate) fn trace_core_operator_managed_net_edges(
    operator: &CoreOperator,
    visitor: &mut Visitor<'_>,
) {
    match operator {
        CoreOperator::FunctionCaptures { code, .. }
        | CoreOperator::ComputationCaptures { code, .. } => {
            code.runtime().trace_managed_edge(visitor);
        }
        CoreOperator::ApplyArity { .. }
        | CoreOperator::Dict { .. }
        | CoreOperator::Builtin(_)
        | CoreOperator::Applicable(_)
        | CoreOperator::List { .. }
        | CoreOperator::Access { .. }
        | CoreOperator::Request { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use glam_gc::{Gc, Trace, Visitor};

    use super::*;
    use crate::core::{
        Builtin, BuiltinCall, CoreValueFactory, EvaluationHalt, FunctionCode, Key,
        ManagedDropRecord, ManagedFamily, NetValue,
    };
    use crate::core_net::{CoreDataKey, CoreRuntimeNet, CoreSpecialization};
    use crate::interaction_net::{
        NetBuilder, NetSpecialization, OperatorCall, ReductionKind, RuntimeNet, RuntimeNetPayload,
    };
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn values() -> CoreValueFactory {
        CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
    }

    fn number(value: i64) -> Value {
        Value::Number(value.into())
    }

    fn closed_data_net(values: &CoreValueFactory, value: Value) -> CoreRuntimeNet {
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let exposed = builder.data(value);
        values.instantiate_core_net(&builder.finish(exposed))
    }

    #[test]
    fn core_operator_value_adapter_enumerates_every_value_payload() {
        let values = values();
        let first = number(1);
        let second = number(2);
        let function_runtime = closed_data_net(&values, number(0));
        let code = Arc::new(FunctionCode::new(function_runtime.clone(), 1, 3));
        let supplied: Arc<[Value]> = Arc::from([first.clone(), second.clone()]);
        let one_supplied: Arc<[Value]> = Arc::from([first.clone()]);
        let operators = [
            (
                CoreOperator::ApplyArity {
                    arity: 3,
                    supplied: supplied.clone(),
                },
                2,
            ),
            (
                CoreOperator::FunctionCaptures {
                    code: code.clone(),
                    supplied: supplied.clone(),
                },
                2,
            ),
            (
                CoreOperator::ComputationCaptures {
                    code: code.clone(),
                    supplied: supplied.clone(),
                },
                2,
            ),
            (
                CoreOperator::Dict {
                    keys: Arc::from([
                        Key::binary_from_text("first"),
                        Key::binary_from_text("second"),
                        Key::binary_from_text("third"),
                    ]),
                    supplied: supplied.clone(),
                },
                2,
            ),
            (
                CoreOperator::Builtin(BuiltinCall {
                    builtin: Builtin::Append,
                    arguments: one_supplied,
                }),
                1,
            ),
            (CoreOperator::Applicable(first.clone()), 1),
            (
                CoreOperator::List {
                    arity: 3,
                    supplied: supplied.clone(),
                },
                2,
            ),
            (
                CoreOperator::Access {
                    path: Arc::from([
                        CoreDataKey::Key(Key::binary_from_text("leaf")),
                        CoreDataKey::Index,
                        CoreDataKey::PathIndex,
                    ]),
                    supplied: supplied.clone(),
                },
                2,
            ),
            (
                CoreOperator::Request {
                    tag: Key::binary_from_text("request"),
                    arity: 3,
                    supplied,
                    wrap_effect: true,
                },
                2,
            ),
        ];

        for (operator, expected_values) in operators {
            let mut value_edges = Vec::new();
            operator.visit_compatibility_value_edges(&mut |value| {
                value_edges.push(value.clone());
            });
            assert_eq!(value_edges.len(), expected_values);
        }
    }

    #[test]
    fn managed_core_net_trace_does_not_reduce_materialize_or_force() {
        let values = values();
        let public_values = crate::api::Values::from_core_factory(values.clone());
        let baseline = values
            .collect_managed_for_test()
            .expect("the trace fixture should begin collectible");
        let forced = Arc::new(AtomicBool::new(false));
        let forced_by_thunk = forced.clone();
        let deferred = Value::semantic_thunk(&values, "managed net trace sentinel", move |_| {
            forced_by_thunk.store(true, Ordering::Release);
            panic!("net payload tracing must not force semantic data")
        });
        let supplied = number(41);
        let failure = number(99);

        let function_runtime = closed_data_net(&values, number(0));
        let code = Arc::new(FunctionCode::new(function_runtime, 1, 1));
        let retained_code = Arc::downgrade(&code);
        let operator = CoreOperator::FunctionCaptures {
            code,
            supplied: Arc::from([supplied.clone()]),
        };
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let [operator_port, result] = builder.operator(operator);
        let data = builder.data(deferred.clone());
        builder.wire(operator_port, data);
        let runtime = values.instantiate_core_net(&builder.finish(result));

        // Retain a specialization failure so the same read-only walk also
        // exercises semantic payloads parked in active-pair state.
        let pair = runtime
            .test_with(&values, |net| net.active_pairs().next())
            .expect("the operator/data fixture should begin active");
        let reduction = runtime
            .test_with_optional_mut(&values, |net| net.reduce_pair(pair))
            .expect("the ready operator pair should be claimed");
        let ReductionKind::OperatorCall { operator, data } = reduction.kind else {
            panic!("the fixture should claim an operator call")
        };
        values.with_runtime_value_access(|value_access| {
            runtime.access(&value_access).fail_claimed_operator_call(
                OperatorCall {
                    pair,
                    operator,
                    data,
                },
                EvaluationHalt::from_value(failure.clone()),
            );
        });

        let owner = public_values.wrap(Value::Net(NetValue::new(runtime.clone())));
        let before = runtime.test_with_revisions(&values, |net| {
            assert!(net.stuck_reason(pair).is_some());
        });
        let live = values
            .collect_managed_for_test()
            .expect("the rooted managed net should trace every payload edge");
        let after = runtime.test_with_revisions(&values, |net| {
            assert!(net.stuck_reason(pair).is_some());
        });

        assert_eq!(
            before.1, after.1,
            "payload visitation must not mutate the net"
        );
        assert_eq!(live.finalized_slots(), 0);
        assert!(live.marked_slots() >= baseline.marked_slots() + 3);
        assert!(retained_code.upgrade().is_some());
        assert!(!forced.load(Ordering::Acquire));

        drop(owner);
        values
            .collect_managed_for_test()
            .expect("retiring the owner should leave the trace fixture collectible");
        assert!(retained_code.upgrade().is_none());
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ManagedNetFixtureSpecialization;

    impl NetSpecialization for ManagedNetFixtureSpecialization {
        type Data = Gc<ManagedNetFixtureNode>;
        type Operator = ();
        type RuntimeSource = crate::interaction_net::SharedRuntimeNet<Self>;
        type WaitToken = ();
        type StuckReason = ();
    }

    struct ManagedNetFixtureNode {
        runtime: Mutex<Option<RuntimeNet<ManagedNetFixtureSpecialization>>>,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for ManagedNetFixtureNode {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    // SAFETY: `visit_logical_payloads` exhaustively reports every managed data
    // edge in the fixture net without reducing it. This specialization has no
    // managed operator, source-net, or stuck-reason payload.
    unsafe impl Trace for ManagedNetFixtureNode {
        fn trace(&self, visitor: &mut Visitor<'_>) {
            let runtime = self
                .runtime
                .lock()
                .expect("managed net fixture mutex should not be poisoned");
            let runtime = runtime
                .as_ref()
                .expect("a published managed net fixture must retain its runtime");
            runtime.visit_logical_payloads(&mut |payload| match payload {
                RuntimeNetPayload::Data(edge) => visitor.visit(edge),
                RuntimeNetPayload::Operator(()) => {}
                RuntimeNetPayload::Source(_) => {
                    unreachable!("the closed fixture creates no logical copies")
                }
                RuntimeNetPayload::StuckReason(()) => {}
            });
        }
    }

    // SAFETY: direct Drop updates only an external atomic counter. The mutex,
    // runtime topology, and inert `Gc` values all destroy passively without a
    // Glam service or observation of a dying edge.
    unsafe impl ManagedFamily for ManagedNetFixtureNode {
        const DROP_RECORD: ManagedDropRecord = ManagedDropRecord::passive(
            "closed generic runtime-net payload fixture",
            "src/core/managed/payload_edges/runtime_net.rs",
            "direct Drop updates only an external atomic counter",
            "the runtime graph, mutex, and Gc data edges drop passively",
        );
    }

    #[test]
    fn generic_runtime_net_payload_cycle_marks_exactly() {
        let values = values();
        let baseline = values
            .collect_managed_for_test()
            .expect("canonical roots should collect before the net fixture");
        let drops = Arc::new(AtomicUsize::new(0));
        let root = values.with_managed_values(|scope| {
            let allocator = scope
                .allocator::<ManagedNetFixtureNode>()
                .expect("the managed net fixture layout should be supported");
            let node = allocator.alloc(ManagedNetFixtureNode {
                runtime: Mutex::new(None),
                drops: drops.clone(),
            });
            let mut builder = NetBuilder::<ManagedNetFixtureSpecialization>::new();
            let exposed = builder.data(node);
            let runtime = builder.finish(exposed).instantiate();

            // SAFETY: `node` is the live owner and target in this matching
            // heap. Its runtime changes from absent to exactly one self edge.
            unsafe {
                let owner = scope.get_traced_edge(&node);
                scope
                    .mutator
                    .with_edge_replacement(&node, None, Some(&node), || {
                        assert!(
                            owner
                                .runtime
                                .lock()
                                .expect("managed net fixture mutex should not be poisoned")
                                .replace(runtime)
                                .is_none()
                        );
                    });
            }
            scope.root(node)
        });

        let live = values
            .collect_managed_for_test()
            .expect("the rooted runtime-net cycle should collect");
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 1);
        assert_eq!(drops.load(Ordering::Relaxed), 0);

        drop(root);
        let dead = values
            .collect_managed_for_test()
            .expect("the unrooted runtime-net cycle should be reclaimed");
        assert_eq!(dead.finalized_slots(), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}
