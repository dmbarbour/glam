//! Non-reducing adapters for compatibility interaction-net payloads.

use super::{CompatibilityValueEdges, visit_values};
use crate::core::{
    EvaluationHalt, EvaluationHaltPayload, FunctionCode, FunctionValue, LazySource, LazyValue,
    NetValue, Value,
};
use crate::core_net::{CoreOperator, CoreRuntimeNet, CoreRuntimeNetAccess, CoreRuntimeNetPayload};
use crate::interaction_net::RuntimeNetPayloadVisitStats;

/// Reports every direct runtime-net identity held by one compatibility
/// payload.
///
/// Implementations must not reduce, inspect, or materialize the reported net.
/// The callback is synchronous and may not retain the borrow. I8 replaces
/// these external identities with exact managed outer-cell edges.
#[allow(
    dead_code,
    reason = "I4E installs compatibility adapters consumed as net-bearing families migrate in I6-I8"
)]
pub(crate) trait CompatibilityNetEdges {
    fn visit_compatibility_net_edges(&self, visit: &mut dyn FnMut(&CoreRuntimeNet));
}

/// Logical work performed while translating one core net's generic payload
/// walk into direct semantic value and net edges.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "I4E installs counters consumed by focused fixtures and the later I8 audit"
)]
pub(crate) struct CoreRuntimeNetEdgeVisitStats {
    pub(crate) runtime: RuntimeNetPayloadVisitStats,
    pub(crate) value_edges: usize,
    pub(crate) net_edges: usize,
}

impl CompatibilityNetEdges for NetValue {
    fn visit_compatibility_net_edges(&self, visit: &mut dyn FnMut(&CoreRuntimeNet)) {
        visit(self.runtime());
    }
}

impl CompatibilityNetEdges for FunctionCode {
    fn visit_compatibility_net_edges(&self, visit: &mut dyn FnMut(&CoreRuntimeNet)) {
        visit(self.runtime());
    }
}

impl CompatibilityNetEdges for FunctionValue {
    fn visit_compatibility_net_edges(&self, visit: &mut dyn FnMut(&CoreRuntimeNet)) {
        self.stage().visit_compatibility_net_edges(visit);
    }
}

impl CompatibilityNetEdges for Value {
    fn visit_compatibility_net_edges(&self, visit: &mut dyn FnMut(&CoreRuntimeNet)) {
        match self {
            Self::Function(function) => function.visit_compatibility_net_edges(visit),
            Self::Net(net) => net.visit_compatibility_net_edges(visit),
            Self::Atom(_)
            | Self::Number(_)
            | Self::Binary(_)
            | Self::List(_)
            | Self::Dict(_)
            | Self::Builtin(_)
            | Self::PartialBuiltin(_)
            | Self::Lazy(_)
            | Self::Promised(_)
            | Self::Metadata(_)
            | Self::Opaque(_) => {}
        }
    }
}

impl CompatibilityNetEdges for LazySource {
    fn visit_compatibility_net_edges(&self, visit: &mut dyn FnMut(&CoreRuntimeNet)) {
        match self {
            Self::NetComputation(net) => net.visit_compatibility_net_edges(visit),
            Self::FunctionCall { function, .. } => function.visit_compatibility_net_edges(visit),
            Self::Error
            | Self::ComputedFixpoint(_)
            | Self::SemanticComputation(_)
            | Self::HostCall(_)
            | Self::ReflectionTask(_)
            | Self::Access { .. }
            | Self::Application(_)
            | Self::Builtin(_)
            | Self::NetConstruction(_) => {}
            #[cfg(test)]
            Self::SemanticThunk(_) => {}
        }
    }
}

impl CompatibilityNetEdges for LazyValue {
    fn visit_compatibility_net_edges(&self, visit: &mut dyn FnMut(&CoreRuntimeNet)) {
        // A terminal result is already the lazy cell's direct `Value` edge;
        // any net below that value belongs to the value shell. Only an
        // unresolved producer can contain a direct net identity here. I5's
        // managed visitor must obtain one stable source/result snapshot for
        // both value and net categories; it must not call the two
        // compatibility adapters independently across a publication race.
        if let Some(source) = self.source_snapshot() {
            source.visit_compatibility_net_edges(visit);
        }
    }
}

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

impl CompatibilityNetEdges for CoreOperator {
    fn visit_compatibility_net_edges(&self, visit: &mut dyn FnMut(&CoreRuntimeNet)) {
        match self {
            Self::FunctionCaptures { code, .. } | Self::ComputationCaptures { code, .. } => {
                code.visit_compatibility_net_edges(visit);
            }
            Self::ApplyArity { .. }
            | Self::Dict { .. }
            | Self::Builtin(_)
            | Self::Applicable(_)
            | Self::List { .. }
            | Self::Access { .. }
            | Self::Request { .. } => {}
        }
    }
}

#[allow(
    dead_code,
    reason = "I4E installs exact halt visitation before I8 uses the core net adapter in production tracing"
)]
pub(crate) fn visit_halt_value_edges(halt: &EvaluationHalt, visit: &mut dyn FnMut(&Value)) {
    match halt.payload() {
        EvaluationHaltPayload::Failure(failure) => {
            failure.visit_compatibility_value_edges(visit);
        }
        EvaluationHaltPayload::Blocked => {}
        EvaluationHaltPayload::UnassignedPromise(promise) => {
            // The promise cell is the direct semantic identity. The
            // compatibility shell represents it through the existing arm.
            visit(&Value::Promised(promise.clone()));
        }
    }
}

/// Enumerates every direct semantic edge in one core runtime net without
/// reducing the net or following a remote cursor.
///
/// The caller supplies matching runtime value access. Callbacks run while the
/// net's read-only synchronization guard is held and therefore must not
/// re-enter this net.
#[allow(
    dead_code,
    reason = "I4E installs the core net adapter before I8 uses it in production tracing"
)]
pub(crate) fn visit_core_runtime_net_edges(
    access: &CoreRuntimeNetAccess<'_, '_>,
    visit_value: &mut dyn FnMut(&Value),
    visit_net: &mut dyn FnMut(&CoreRuntimeNet),
) -> CoreRuntimeNetEdgeVisitStats {
    let mut stats = CoreRuntimeNetEdgeVisitStats::default();
    stats.runtime = access.visit_logical_payloads(&mut |payload| match payload {
        CoreRuntimeNetPayload::Value(value) => {
            stats.value_edges += 1;
            visit_value(value);
        }
        CoreRuntimeNetPayload::Operator(operator) => {
            operator.visit_compatibility_value_edges(&mut |value| {
                stats.value_edges += 1;
                visit_value(value);
            });
            operator.visit_compatibility_net_edges(&mut |net| {
                stats.net_edges += 1;
                visit_net(net);
            });
        }
        CoreRuntimeNetPayload::Source(source) => {
            stats.net_edges += 1;
            visit_net(&source);
        }
        CoreRuntimeNetPayload::StuckReason(reason) => {
            visit_halt_value_edges(reason, &mut |value| {
                stats.value_edges += 1;
                visit_value(value);
            });
        }
    });
    stats
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use glam_gc::{Gc, Trace, Visitor};

    use super::*;
    use crate::core::{
        Builtin, BuiltinCall, CoreValueFactory, EvaluationHalt, FunctionValue, Key,
        ManagedDropRecord, ManagedFamily,
    };
    use crate::core_net::{CoreDataKey, CoreSpecialization};
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

    fn net_edges(value: &impl CompatibilityNetEdges) -> Vec<CoreRuntimeNet> {
        let mut edges = Vec::new();
        value.visit_compatibility_net_edges(&mut |net| edges.push(net.clone()));
        edges
    }

    #[test]
    fn core_operator_adapter_enumerates_every_value_and_net_payload() {
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
                0,
            ),
            (
                CoreOperator::FunctionCaptures {
                    code: code.clone(),
                    supplied: supplied.clone(),
                },
                2,
                1,
            ),
            (
                CoreOperator::ComputationCaptures {
                    code: code.clone(),
                    supplied: supplied.clone(),
                },
                2,
                1,
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
                0,
            ),
            (
                CoreOperator::Builtin(BuiltinCall {
                    builtin: Builtin::Append,
                    arguments: one_supplied,
                }),
                1,
                0,
            ),
            (CoreOperator::Applicable(first.clone()), 1, 0),
            (
                CoreOperator::List {
                    arity: 3,
                    supplied: supplied.clone(),
                },
                2,
                0,
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
                0,
            ),
            (
                CoreOperator::Request {
                    tag: Key::binary_from_text("request"),
                    arity: 3,
                    supplied,
                    wrap_effect: true,
                },
                2,
                0,
            ),
        ];

        for (operator, expected_values, expected_nets) in operators {
            let mut value_edges = Vec::new();
            operator.visit_compatibility_value_edges(&mut |value| {
                value_edges.push(value.clone());
            });
            assert_eq!(value_edges.len(), expected_values);

            let nets = net_edges(&operator);
            assert_eq!(nets.len(), expected_nets);
            assert!(
                nets.iter().all(|net| net.ptr_eq(&function_runtime)),
                "only function-code operators carry the function runtime"
            );
        }

        let stage = NetValue::new(function_runtime.clone());
        let function = FunctionValue::new(stage.clone(), 1);
        assert!(net_edges(&stage)[0].ptr_eq(&function_runtime));
        assert!(net_edges(code.as_ref())[0].ptr_eq(&function_runtime));
        assert!(net_edges(&function)[0].ptr_eq(&function_runtime));
        assert!(net_edges(&Value::Function(function.clone()))[0].ptr_eq(&function_runtime));
        assert!(net_edges(&Value::Net(stage.clone()))[0].ptr_eq(&function_runtime));

        let net_lazy = LazyValue::from_net_computation(&values, stage);
        assert!(net_edges(&net_lazy)[0].ptr_eq(&function_runtime));
        let call_lazy = LazyValue::from_function_call(&values, function, Arc::from([first]));
        assert!(net_edges(&call_lazy)[0].ptr_eq(&function_runtime));
    }

    #[test]
    fn net_value_adapter_traces_without_reduction_or_materialization() {
        let values = values();
        let forced = Arc::new(AtomicBool::new(false));
        let forced_by_thunk = forced.clone();
        let deferred = Value::semantic_thunk(&values, "net adapter sentinel", move |_| {
            forced_by_thunk.store(true, Ordering::Release);
            panic!("net payload tracing must not force semantic data")
        });
        let supplied = number(41);
        let failure = number(99);

        let function_runtime = closed_data_net(&values, number(0));
        let code = Arc::new(FunctionCode::new(function_runtime.clone(), 1, 1));
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
            .test_with(|net| net.active_pairs().next())
            .expect("the operator/data fixture should begin active");
        let reduction = runtime
            .test_with_optional_mut(|net| net.reduce_pair(pair))
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

        let before = runtime.test_with_revisions(|net| {
            assert!(net.stuck_reason(pair).is_some());
        });
        let mut value_edges = Vec::new();
        let mut runtime_edges = Vec::new();
        let stats = values.with_runtime_value_access(|value_access| {
            visit_core_runtime_net_edges(
                &runtime.access(&value_access),
                &mut |value| value_edges.push(value.clone()),
                &mut |net| runtime_edges.push(net.clone()),
            )
        });
        let after = runtime.test_with_revisions(|net| {
            assert!(net.stuck_reason(pair).is_some());
        });

        assert_eq!(
            before.1, after.1,
            "payload visitation must not mutate the net"
        );
        assert_eq!(stats.runtime.data_nodes, 1);
        assert_eq!(stats.runtime.operator_nodes, 1);
        assert_eq!(stats.runtime.stuck_reasons, 1);
        assert_eq!(stats.value_edges, 3);
        assert_eq!(stats.net_edges, 1);
        assert_eq!(value_edges.len(), 3);
        assert!(value_edges.contains(&deferred));
        assert!(value_edges.contains(&supplied));
        assert!(value_edges.contains(&failure));
        assert_eq!(runtime_edges.len(), 1);
        assert!(runtime_edges[0].ptr_eq(&function_runtime));
        assert!(!forced.load(Ordering::Acquire));

        let (copy, _) = CoreRuntimeNet::test_copy_layer(function_runtime.clone());
        let before_copy = copy.test_with_revisions(|_| ());
        let mut copied_sources = Vec::new();
        let copy_stats = values.with_runtime_value_access(|value_access| {
            visit_core_runtime_net_edges(
                &copy.access(&value_access),
                &mut |_| panic!("an untouched copy layer has no local semantic value"),
                &mut |net| copied_sources.push(net.clone()),
            )
        });
        let after_copy = copy.test_with_revisions(|_| ());
        assert_eq!(before_copy.1, after_copy.1);
        assert_eq!(copy_stats.runtime.source_nets, 1);
        assert_eq!(copy_stats.net_edges, 1);
        assert!(copied_sources[0].ptr_eq(&function_runtime));
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
                RuntimeNetPayload::Data(edge) => visitor.visit(*edge),
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
            "I4E closed runtime-net adapter fixture",
            "src/core/managed/payload_edges/runtime_net.rs",
            "direct Drop updates only an external atomic counter",
            "the runtime graph, mutex, and Gc data edges drop passively",
        );
    }

    #[test]
    fn net_value_adapter_cycle_marks_exactly() {
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
                let owner = scope.get_traced_edge(node);
                scope
                    .mutator
                    .with_edge_replacement(node, None, Some(node), || {
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
