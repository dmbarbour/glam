//! Exact semantic-value edges in compatibility structural payloads.
//!
//! I5 replaced the recursive lazy, promise, and core-net identities. The
//! remaining adapters keep immutable Rust-owned shells compile-exhaustive and
//! compose into those exact managed leaves. I6-I8 may retire an adapter only
//! when an audited managed replacement reports the same edges.

#[cfg(test)]
use super::super::{EvaluationFailure, SemanticComputation};
use super::super::{
    BuiltinCall, EvaluatedValue, FixpointComputation, LazyApplication, LazySource,
    ListEffectComputation, MetadataCarrier, ReflectionComputation, Value,
};

/// Reports every direct semantic `Value` edge held by one compatibility
/// payload, in stable source order.
///
/// Implementations must not evaluate, format, compare, or recursively visit a
/// reported value. The callback is synchronous and may not retain the borrow.
/// External scheduler/host lifecycle state is not a semantic value edge and
/// remains governed by its own root inventory.
#[allow(
    dead_code,
    reason = "I4C compatibility adapters remain the exact walk for audited structural shells"
)]
pub(crate) trait CompatibilityValueEdges {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value));

    /// Reports direct managed edges which cannot be borrowed as a raw
    /// compatibility `Value` without duplicating a persistent identity.
    fn trace_direct_compatibility_managed_edges(&self, _visitor: &mut glam_gc::Visitor<'_>) {}
}

mod managed;
mod persistent;
mod runtime_net;

pub(super) use managed::{
    visit_compatibility_managed_edges, visit_compatibility_payload_managed_edges,
};
pub(super) use runtime_net::{
    trace_core_operator_managed_net_edges, trace_halt_managed_edges,
    trace_lazy_source_managed_net_edges,
};

fn visit_values(values: &[Value], visit: &mut dyn FnMut(&Value)) {
    for value in values {
        visit(value);
    }
}

impl CompatibilityValueEdges for Value {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        match self {
            Self::List(list) => list.visit_compatibility_value_edges(visit),
            Self::Dict(dict) => dict.visit_compatibility_value_edges(visit),
            Self::PartialBuiltin(call) => call.visit_compatibility_value_edges(visit),
            Self::Metadata(metadata) => metadata.visit_compatibility_value_edges(visit),
            Self::Atom(_)
            | Self::Number(_)
            | Self::Binary(_)
            | Self::Builtin(_)
            | Self::Function(_)
            | Self::Net(_)
            | Self::Lazy(_)
            | Self::Promised(_)
            | Self::Opaque(_) => {}
        }
    }

    fn trace_direct_compatibility_managed_edges(&self, visitor: &mut glam_gc::Visitor<'_>) {
        match self {
            Self::List(list) => list.trace_direct_compatibility_managed_edges(visitor),
            Self::Atom(_)
            | Self::Number(_)
            | Self::Binary(_)
            | Self::Dict(_)
            | Self::Builtin(_)
            | Self::PartialBuiltin(_)
            | Self::Function(_)
            | Self::Net(_)
            | Self::Lazy(_)
            | Self::Promised(_)
            | Self::Metadata(_)
            | Self::Opaque(_) => {}
        }
    }
}

impl CompatibilityValueEdges for EvaluatedValue {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        visit(&self.0);
    }
}

impl CompatibilityValueEdges for BuiltinCall {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        visit_values(&self.arguments, visit);
    }
}

impl CompatibilityValueEdges for LazyApplication {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        visit(&self.function);
        visit_values(&self.arguments, visit);
    }
}

impl CompatibilityValueEdges for FixpointComputation {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        match self {
            Self::Function(value) | Self::ObjectInstance(value) => visit(value),
        }
    }
}

#[cfg(test)]
impl CompatibilityValueEdges for SemanticComputation {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        visit_values(&self.captures, visit);
    }
}

impl CompatibilityValueEdges for ListEffectComputation {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        match self {
            Self::Run { effect }
            | Self::Cut { operation: effect }
            | Self::FixFunction {
                function: effect, ..
            } => visit(effect),
            Self::Sequence {
                results,
                continuation,
            }
            | Self::FlatMapResults {
                results,
                continuation,
            } => {
                results.visit_compatibility_value_edges(visit);
                visit(continuation);
            }
            Self::FirstResult { results } => results.visit_compatibility_value_edges(visit),
        }
    }

    fn trace_direct_compatibility_managed_edges(&self, visitor: &mut glam_gc::Visitor<'_>) {
        match self {
            Self::Sequence { results, .. }
            | Self::FlatMapResults { results, .. }
            | Self::FirstResult { results } => {
                results.trace_direct_compatibility_managed_edges(visitor);
            }
            Self::Run { .. } | Self::Cut { .. } | Self::FixFunction { .. } => {}
        }
    }
}

impl CompatibilityValueEdges for ReflectionComputation {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        // The managed lazy owns these immutable semantic edges directly. The
        // transient task handoff must never substitute registered roots for
        // this trace.
        visit(&self.effect);
        if let Some(target) = &self.target {
            visit(target);
        }
    }

    fn trace_direct_compatibility_managed_edges(&self, visitor: &mut glam_gc::Visitor<'_>) {
        self.completion_promise.trace_managed_edge(visitor);
    }
}

impl CompatibilityValueEdges for LazySource {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        match self {
            Self::Error => {}
            Self::HostCall(_) => {}
            Self::NetComputation(_) => {}
            Self::ComputedFixpoint(computation) => {
                computation.visit_compatibility_value_edges(visit);
            }
            #[cfg(test)]
            Self::SemanticComputation(computation) => {
                computation.visit_compatibility_value_edges(visit);
            }
            Self::ListEffectComputation(computation) => {
                computation.visit_compatibility_value_edges(visit);
            }
            #[cfg(test)]
            Self::SemanticThunk(_) => {
                // This capture-bearing compatibility constructor does not
                // exist in production. I4B deliberately retains it only for
                // pre-managed unit fixtures, where it is never presented as
                // exact traceable state.
            }
            Self::ReflectionTask(computation) => {
                computation.visit_compatibility_value_edges(visit);
            }
            Self::Access { path: _, arguments } => visit_values(arguments, visit),
            Self::Application(application) => {
                application.visit_compatibility_value_edges(visit);
            }
            Self::Builtin(call) => call.visit_compatibility_value_edges(visit),
            Self::FunctionCall {
                function: _,
                arguments,
            } => {
                visit_values(arguments, visit);
            }
        }
    }

    fn trace_direct_compatibility_managed_edges(&self, visitor: &mut glam_gc::Visitor<'_>) {
        match self {
            Self::HostCall(producer) => producer.trace_managed_edges(visitor),
            Self::ReflectionTask(computation) => {
                computation.trace_direct_compatibility_managed_edges(visitor);
            }
            Self::ListEffectComputation(computation) => {
                computation.trace_direct_compatibility_managed_edges(visitor);
            }
            Self::Error
            | Self::NetComputation(_)
            | Self::ComputedFixpoint(_)
            | Self::Access { .. }
            | Self::Application(_)
            | Self::Builtin(_)
            | Self::FunctionCall { .. } => {}
            #[cfg(test)]
            Self::SemanticComputation(_) | Self::SemanticThunk(_) => {}
        }
    }
}

impl CompatibilityValueEdges for MetadataCarrier {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        visit(self.metadata.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::core::{
        Builtin, CoreValueFactory, FunctionValue, HostCallRecord, LazyCycle, LazyCycleMember,
        LazyId, LazyValue, NetValue, PromisedValue,
    };
    use crate::core_net::{CoreDataKey, CoreSpecialization};
    use crate::interaction_net::NetBuilder;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn values() -> CoreValueFactory {
        CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
    }

    fn number(value: i64) -> Value {
        Value::Number(value.into())
    }

    fn edges(values: &CoreValueFactory, value: &impl CompatibilityValueEdges) -> Vec<Value> {
        let mut edges = Vec::new();
        value.visit_compatibility_value_edges(&mut |value| {
            edges.push(value.duplicate_for_test(values));
        });
        edges
    }

    fn assert_edges(
        values: &CoreValueFactory,
        value: &impl CompatibilityValueEdges,
        expected: &[Value],
    ) {
        let actual = edges(values, value);
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            values.assert_same_representation_for_test(actual, expected);
        }
    }

    fn fixture_function(values: &CoreValueFactory) -> FunctionValue {
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let exposed = builder.data(number(0));
        let template = builder.finish(exposed);
        FunctionValue::new(NetValue::new(values.instantiate_core_net(&template)), 1)
    }

    fn return_first_capture(
        context: &crate::evaluation::EvaluatorStepContext<'_>,
        captures: &[Value],
    ) -> Result<Value, crate::core::EvaluationHalt> {
        let [first, ..] = captures else {
            unreachable!("the fixture always supplies a capture")
        };
        Ok(context.with_value_access(|access| access.values().duplicate_value(first)))
    }

    #[test]
    fn argument_and_application_visitors_enumerate_exact_edges() {
        let values = values();
        let first = number(1);
        let second = number(2);
        let third = number(3);
        let call = BuiltinCall {
            builtin: Builtin::Append,
            arguments: Arc::from([
                first.duplicate_for_test(&values),
                second.duplicate_for_test(&values),
            ]),
        };
        assert_edges(
            &values,
            &call,
            &[
                first.duplicate_for_test(&values),
                second.duplicate_for_test(&values),
            ],
        );

        let application = LazyApplication {
            function: first.duplicate_for_test(&values),
            arguments: Arc::from([
                second.duplicate_for_test(&values),
                third.duplicate_for_test(&values),
            ]),
        };
        assert_edges(
            &values,
            &application,
            &[
                first.duplicate_for_test(&values),
                second.duplicate_for_test(&values),
                third.duplicate_for_test(&values),
            ],
        );

        let access = LazySource::Access {
            path: Arc::from([CoreDataKey::Index]),
            arguments: Arc::from([
                first.duplicate_for_test(&values),
                second.duplicate_for_test(&values),
            ]),
        };
        assert_edges(
            &values,
            &access,
            &[
                first.duplicate_for_test(&values),
                second.duplicate_for_test(&values),
            ],
        );

        let function_call = LazySource::FunctionCall {
            function: fixture_function(&values),
            arguments: Arc::from([
                second.duplicate_for_test(&values),
                third.duplicate_for_test(&values),
            ]),
        };
        assert_edges(&values, &function_call, &[second, third]);
    }

    #[test]
    fn compatibility_recursive_payload_visitors_enumerate_exact_edges() {
        let values = values();
        let first = number(11);
        let second = number(22);
        let failure_emission = number(33);
        let failure_context = number(44);

        let semantic = SemanticComputation {
            operation: return_first_capture,
            captures: Arc::from([
                first.duplicate_for_test(&values),
                second.duplicate_for_test(&values),
            ]),
        };
        assert_edges(
            &values,
            &semantic,
            &[
                first.duplicate_for_test(&values),
                second.duplicate_for_test(&values),
            ],
        );
        assert_edges(
            &values,
            &FixpointComputation::Function(first.duplicate_for_test(&values)),
            &[first.duplicate_for_test(&values)],
        );
        assert_edges(
            &values,
            &FixpointComputation::ObjectInstance(first.duplicate_for_test(&values)),
            &[first.duplicate_for_test(&values)],
        );
        assert_edges(
            &values,
            &MetadataCarrier::new(second.duplicate_for_test(&values)),
            &[second.duplicate_for_test(&values)],
        );

        let reflection_value = Value::reflection_gate(
            &values,
            first.duplicate_for_test(&values),
            second.duplicate_for_test(&values),
        );
        let Value::Lazy(reflection_lazy) = reflection_value else {
            unreachable!("the reflection fixture must be lazy")
        };
        let Some(LazySource::ReflectionTask(reflection)) = reflection_lazy.source_snapshot(&values)
        else {
            unreachable!("the reflection fixture must retain its source")
        };
        assert_edges(
            &values,
            reflection.as_ref(),
            &[
                first.duplicate_for_test(&values),
                second.duplicate_for_test(&values),
            ],
        );

        let promise = PromisedValue::new(&values, "compatibility visitor promise");
        assert!(
            crate::core::set_test_promise(&values, &promise, first.duplicate_for_test(&values))
                .is_ok(),
            "the fresh promise should accept one assignment"
        );
        values.assert_same_representation_for_test(
            &promise.assignment(&values),
            &Some(Ok(first.duplicate_for_test(&values))),
        );

        let failed_promise = PromisedValue::new(&values, "compatibility visitor failure");
        assert!(
            crate::core::fail_test_promise(
                &values,
                &failed_promise,
                Arc::new(values.with_runtime_value_access(|access| {
                    EvaluationFailure::emission(access.duplicate_value(&failure_emission))
                        .with_context_in(&access, access.duplicate_value(&failure_context))
                })),
            )
            .is_ok()
        );
        assert!(matches!(failed_promise.assignment(&values), Some(Err(_))));

        let pending = LazyValue::semantic_computation(
            &values,
            "compatibility visitor semantic source",
            [
                first.duplicate_for_test(&values),
                second.duplicate_for_test(&values),
            ],
            return_first_capture,
        );
        let source = pending
            .source_snapshot(&values)
            .expect("the pending lazy must retain its source");
        assert_edges(
            &values,
            &source,
            &[
                first.duplicate_for_test(&values),
                second.duplicate_for_test(&values),
            ],
        );

        let complete = LazyValue::semantic_computation(
            &values,
            "compatibility visitor result",
            [second],
            return_first_capture,
        );
        let evaluated = EvaluatedValue::from_whnf(first.duplicate_for_test(&values))
            .expect("a number is already in weak-head normal form");
        let cached = crate::core::cache_test_lazy(&values, &complete, Ok(evaluated));
        let Ok(cached) = cached else {
            panic!("the completed lazy should accept its first result")
        };
        values.assert_same_representation_for_test(
            &cached,
            &EvaluatedValue::from_whnf(first.duplicate_for_test(&values)).unwrap(),
        );
        let Some(Ok(cached)) = complete.cached(&values) else {
            panic!("the completed lazy must retain its successful result")
        };
        assert_edges(&values, &cached, &[first]);
    }

    #[test]
    fn shared_cyclic_failure_context_traces_exactly() {
        let values = values();
        let shared = Value::List(crate::core::List::from_values(vec![number(7)]));
        let cycle = Arc::new(LazyCycle {
            members: vec![
                LazyCycleMember {
                    id: LazyId(NonZeroU64::new(1).unwrap()),
                    label: Arc::from("left"),
                },
                LazyCycleMember {
                    id: LazyId(NonZeroU64::new(2).unwrap()),
                    label: Arc::from("right"),
                },
            ]
            .into_boxed_slice(),
        });
        let failure = values.with_runtime_value_access(|access| {
            let first = access.duplicate_value(&shared);
            let second = access.duplicate_value(&shared);
            EvaluationFailure::dependency_cycle(cycle.clone())
                .with_context_in(&access, first)
                .with_context_in(&access, second)
        });

        assert_eq!(failure.contexts().len(), 2);
        assert_eq!(
            failure
                .dependency_cycle_value()
                .expect("the fixture should retain dependency-cycle data"),
            &cycle,
            "cycle members are diagnostic leaf data, not semantic Value edges"
        );
    }

    #[test]
    fn failure_trace_invokes_no_semantic_service() {
        let values = values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the failure-trace fixture should start collectible");
        let forced = Arc::new(AtomicBool::new(false));
        let forced_by_thunk = forced.clone();
        let root = values.construct_runtime_value_root(|access| {
            let sentinel = LazyValue::external_host_call_in(
                access,
                "failure visitor sentinel",
                HostCallRecord::external_without_semantic_values(
                    "failure visitor sentinel",
                    "src/core/managed/payload_edges.rs",
                    "no semantic captures",
                ),
                [],
                move |_| {
                    forced_by_thunk.store(true, Ordering::Release);
                    panic!("failure edge visitation must not evaluate its values")
                },
            );
            let sentinel = Value::Lazy(sentinel);
            let failure = Arc::new(
                EvaluationFailure::emission(access.duplicate_value(&sentinel))
                    .with_context_in(access, sentinel),
            );
            Value::Lazy(
                access
                    .construct_failed_managed_lazy("failure trace owner", failure)
                    .expect("the failed lazy should fit one collector run"),
            )
        });
        assert!(!forced.load(Ordering::Acquire));

        let live = values
            .collect_managed_for_test()
            .expect("collector tracing must retain the failure's lazy value");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 3);
        assert!(!forced.load(Ordering::Acquire));
        drop(root);
    }

    #[test]
    fn external_host_call_reports_its_explicit_semantic_edges() {
        let values = values();
        let baseline = values
            .collect_managed_for_test()
            .expect("the host-call fixture should start collectible");
        let root = values.construct_runtime_value_root(|access| {
            let promise = access
                .construct_managed_promise("host-call capture")
                .expect("the capture promise should fit one collector run");
            Value::Lazy(LazyValue::external_host_call_in(
                access,
                "compatibility visitor host call",
                HostCallRecord::external_with_semantic_values(
                    "compatibility visitor host call",
                    "src/core/managed/payload_edges.rs",
                    "one explicit semantic capture",
                ),
                [Value::Promised(promise)],
                |_| Err(Arc::new(EvaluationFailure::message("not invoked"))),
            ))
        });

        let live = values
            .collect_managed_for_test()
            .expect("collector tracing must retain the host-call capture");
        assert_eq!(live.root_entries(), baseline.root_entries() + 1);
        assert_eq!(live.marked_slots(), baseline.marked_slots() + 3);
        drop(root);
    }
}
