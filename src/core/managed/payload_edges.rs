//! Exact semantic-value edges in compatibility structural payloads.
//!
//! I5 replaced the recursive lazy, promise, and core-net identities. The
//! remaining adapters keep immutable Rust-owned shells compile-exhaustive and
//! compose into those exact managed leaves. I6-I8 may retire an adapter only
//! when an audited managed replacement reports the same edges.

use super::super::{
    BuiltinCall, EvaluatedValue, EvaluationFailure, FixpointComputation, LazyApplication,
    LazySource, MetadataCarrier, ReflectionComputation, SemanticComputation, Value,
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
}

mod managed;
mod persistent;
mod runtime_net;

pub(super) use managed::{
    visit_compatibility_managed_edges, visit_compatibility_payload_managed_edges,
};
pub(super) use runtime_net::{CompatibilityNetEdges, visit_halt_value_edges};

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
}

impl CompatibilityValueEdges for EvaluatedValue {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        visit(&self.0);
    }
}

impl CompatibilityValueEdges for EvaluationFailure {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        self.visit_direct_values(visit);
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

impl CompatibilityValueEdges for SemanticComputation {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        visit_values(&self.captures, visit);
    }
}

impl CompatibilityValueEdges for ReflectionComputation {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        // The managed lazy owns these immutable semantic edges directly. The
        // external handle reaches only lifecycle/cancellation authority and
        // must never substitute registered roots for this trace.
        visit(&self.effect);
        if let Some(target) = &self.target {
            visit(target);
        }
    }
}

impl CompatibilityValueEdges for LazySource {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        match self {
            Self::Error | Self::HostCall(_) => {}
            Self::NetComputation(_) => {}
            Self::ComputedFixpoint(computation) => {
                computation.visit_compatibility_value_edges(visit);
            }
            Self::SemanticComputation(computation) => {
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
            Self::NetConstruction(effect) => visit(effect),
            Self::FunctionCall {
                function: _,
                arguments,
            } => {
                visit_values(arguments, visit);
            }
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

    fn values() -> CoreValueFactory {
        crate::core::test_value_factory()
    }

    fn number(value: i64) -> Value {
        Value::Number(value.into())
    }

    fn edges(value: &impl CompatibilityValueEdges) -> Vec<Value> {
        let mut edges = Vec::new();
        value.visit_compatibility_value_edges(&mut |value| edges.push(value.clone()));
        edges
    }

    fn fixture_function(values: &CoreValueFactory) -> FunctionValue {
        let mut builder = NetBuilder::<CoreSpecialization>::new();
        let exposed = builder.data(number(0));
        let template = builder.finish(exposed);
        FunctionValue::new(NetValue::new(values.instantiate_core_net(&template)), 1)
    }

    fn return_first_capture(
        _context: &crate::evaluation::EvaluatorStepContext<'_>,
        captures: &[Value],
    ) -> Result<Value, crate::core::EvaluationHalt> {
        let [first, ..] = captures else {
            unreachable!("the fixture always supplies a capture")
        };
        Ok(first.clone())
    }

    #[test]
    fn argument_and_application_visitors_enumerate_exact_edges() {
        let first = number(1);
        let second = number(2);
        let third = number(3);
        let call = BuiltinCall {
            builtin: Builtin::Append,
            arguments: Arc::from([first.clone(), second.clone()]),
        };
        assert_eq!(edges(&call), [first.clone(), second.clone()]);

        let application = LazyApplication {
            function: first.clone(),
            arguments: Arc::from([second.clone(), third.clone()]),
        };
        assert_eq!(
            edges(&application),
            [first.clone(), second.clone(), third.clone()]
        );

        let access = LazySource::Access {
            path: Arc::from([CoreDataKey::Index]),
            arguments: Arc::from([first.clone(), second.clone()]),
        };
        assert_eq!(edges(&access), [first.clone(), second.clone()]);

        let function_call = LazySource::FunctionCall {
            function: fixture_function(&values()),
            arguments: Arc::from([second.clone(), third.clone()]),
        };
        assert_eq!(
            edges(&function_call),
            [second, third],
            "the function stage is a net edge owned by I4E, not a hidden Value edge"
        );
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
            captures: Arc::from([first.clone(), second.clone()]),
        };
        assert_eq!(edges(&semantic), [first.clone(), second.clone()]);
        assert_eq!(
            edges(&FixpointComputation::ObjectInstance(first.clone())),
            vec![first.clone()]
        );
        assert_eq!(
            edges(&MetadataCarrier::new(second.clone())),
            vec![second.clone()]
        );

        let reflection_value = Value::reflection_gate(&values, first.clone(), second.clone());
        let Value::Lazy(reflection_lazy) = reflection_value else {
            unreachable!("the reflection fixture must be lazy")
        };
        let Some(LazySource::ReflectionTask(reflection)) = reflection_lazy.source_snapshot(&values)
        else {
            unreachable!("the reflection fixture must retain its source")
        };
        assert_eq!(
            edges(reflection.as_ref()),
            [first.clone(), second.clone()],
            "reflection effect and target are direct managed semantic edges"
        );

        let promise = PromisedValue::new(&values, "compatibility visitor promise");
        crate::core::set_test_promise(&values, &promise, first.clone())
            .expect("the fresh promise should accept one assignment");
        assert_eq!(promise.assignment(&values), Some(Ok(first.clone())));

        let failed_promise = PromisedValue::new(&values, "compatibility visitor failure");
        crate::core::fail_test_promise(
            &values,
            &failed_promise,
            Arc::new(
                EvaluationFailure::emission(failure_emission.clone())
                    .with_context(failure_context.clone()),
            ),
        )
        .expect("the fresh promise should accept one failure");
        assert!(matches!(failed_promise.assignment(&values), Some(Err(_))));

        let pending = LazyValue::semantic_computation(
            &values,
            "compatibility visitor semantic source",
            [first.clone(), second.clone()],
            return_first_capture,
        );
        let source = pending
            .source_snapshot(&values)
            .expect("the pending lazy must retain its source");
        assert_eq!(edges(&source), [first.clone(), second.clone()]);

        let complete = LazyValue::semantic_computation(
            &values,
            "compatibility visitor result",
            [second],
            return_first_capture,
        );
        let evaluated = EvaluatedValue::try_from(first.clone())
            .expect("a number is already in weak-head normal form");
        assert_eq!(
            crate::core::cache_test_lazy(&values, &complete, Ok(evaluated)),
            Ok(EvaluatedValue(first.clone()))
        );
        assert_eq!(
            edges(
                &complete
                    .cached(&values)
                    .expect("the completed lazy must retain its result")
                    .expect("the completed lazy should succeed")
            ),
            [first],
            "terminal result publication replaces the source capture edge"
        );
    }

    #[test]
    fn shared_cyclic_failure_context_traces_exactly() {
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
        let failure = EvaluationFailure::dependency_cycle(cycle.clone())
            .with_context(shared.clone())
            .with_context(shared.clone());

        assert_eq!(edges(&failure), [shared.clone(), shared]);
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
        let forced = Arc::new(AtomicBool::new(false));
        let forced_by_thunk = forced.clone();
        let sentinel = Value::Lazy(LazyValue::semantic_thunk(
            &values(),
            "failure visitor sentinel",
            move |_| {
                forced_by_thunk.store(true, Ordering::Release);
                panic!("failure edge visitation must not evaluate its values")
            },
        ));
        assert!(!forced.load(Ordering::Acquire));

        let failure = EvaluationFailure::emission(sentinel.clone()).with_context(sentinel.clone());

        assert_eq!(edges(&failure), [sentinel.clone(), sentinel]);
        assert!(!forced.load(Ordering::Acquire));
    }

    #[test]
    fn external_host_call_has_no_reported_semantic_edge() {
        let values = values();
        let source = LazyValue::external_host_call(
            &values,
            "compatibility visitor host call",
            HostCallRecord::external(
                "compatibility visitor host call",
                "src/core/managed/payload_edges.rs",
                "no captures",
            ),
            || Err(Arc::new(EvaluationFailure::message("not invoked"))),
        )
        .source_snapshot(&values)
        .expect("the host call should remain pending");

        assert!(edges(&source).is_empty());
    }
}
