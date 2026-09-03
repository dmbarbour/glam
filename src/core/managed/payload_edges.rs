//! Exact semantic-value edges in the pre-managed compatibility representation.
//!
//! I5-I8 replace these adapters in the same checkpoint that each payload
//! becomes a collector-managed representation. Until then, they make the
//! required edge vocabulary compile-exhaustive without pretending that an
//! owning `Arc<Value>` is already a `Gc` edge.

use super::super::{
    BuiltinCall, EvaluatedValue, EvaluationFailure, FixpointComputation, LazyApplication,
    LazyResult, LazySource, LazyValue, MetadataCarrier, PromiseAssignment, PromisedValue,
    ReflectionComputation, SemanticComputation, Value,
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
    reason = "I4C installs compatibility adapters consumed as managed families migrate in I5-I8"
)]
pub(crate) trait CompatibilityValueEdges {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value));
}

mod persistent;
mod runtime_net;

fn visit_values(values: &[Value], visit: &mut dyn FnMut(&Value)) {
    for value in values {
        visit(value);
    }
}

fn visit_failure_result(result: &LazyResult, visit: &mut dyn FnMut(&Value)) {
    match result {
        Ok(value) => value.visit_compatibility_value_edges(visit),
        Err(failure) => failure.visit_compatibility_value_edges(visit),
    }
}

fn visit_promise_assignment(assignment: &PromiseAssignment, visit: &mut dyn FnMut(&Value)) {
    match assignment {
        Ok(value) => visit(value.as_core()),
        Err(failure) => failure.visit_compatibility_value_edges(visit),
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
    fn visit_compatibility_value_edges(&self, _visit: &mut dyn FnMut(&Value)) {
        // I4F.2b.2 keeps effect, target, and reservation failure values rooted
        // in the runtime external-owner registry. The managed-reachable
        // computation is an edge-free lease.
    }
}

impl CompatibilityValueEdges for LazySource {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        match self {
            Self::Error | Self::HostCall(_) | Self::NetComputation(_) => {}
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
            } => visit_values(arguments, visit),
        }
    }
}

impl CompatibilityValueEdges for LazyValue {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        // Result publication precedes source removal. Prefer the result once
        // visible, otherwise clone one stable source snapshot. Cloning neither
        // forces nor formats a contained value and avoids invoking the visitor
        // while holding the source mutex.
        if let Some(result) = self.0.result.get().cloned() {
            visit_failure_result(&result, visit);
            return;
        }
        let source = {
            let source = self.0.source.lock().expect("lazy source cell was poisoned");
            if let Some(result) = self.0.result.get().cloned() {
                drop(source);
                visit_failure_result(&result, visit);
                return;
            }
            source
                .as_ref()
                .expect("an unresolved lazy must retain its source")
                .clone()
        };
        source.visit_compatibility_value_edges(visit);
    }
}

impl CompatibilityValueEdges for PromisedValue {
    fn visit_compatibility_value_edges(&self, visit: &mut dyn FnMut(&Value)) {
        if let Some(assignment) = self.0.assignment.get() {
            visit_promise_assignment(assignment, visit);
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
        LazyId, NetValue,
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
        let Some(LazySource::ReflectionTask(reflection)) = reflection_lazy.source_snapshot() else {
            unreachable!("the reflection fixture must retain its source")
        };
        assert!(
            edges(reflection.as_ref()).is_empty(),
            "reflection semantic values must remain in the external owner"
        );

        let promise = PromisedValue::new(&values, "compatibility visitor promise");
        promise
            .set(first.clone())
            .expect("the fresh promise should accept one assignment");
        assert_eq!(edges(&promise), vec![first.clone()]);

        let failed_promise = PromisedValue::new(&values, "compatibility visitor failure");
        failed_promise
            .fail(Arc::new(
                EvaluationFailure::emission(failure_emission.clone())
                    .with_context(failure_context.clone()),
            ))
            .expect("the fresh promise should accept one failure");
        assert_eq!(edges(&failed_promise), [failure_emission, failure_context]);

        let pending = LazyValue::semantic_computation(
            &values,
            "compatibility visitor semantic source",
            [first.clone(), second.clone()],
            return_first_capture,
        );
        assert_eq!(edges(&pending), [first.clone(), second.clone()]);

        let complete = LazyValue::semantic_computation(
            &values,
            "compatibility visitor result",
            [second],
            return_first_capture,
        );
        let evaluated = EvaluatedValue::try_from(first.clone())
            .expect("a number is already in weak-head normal form");
        assert_eq!(
            complete.cache(Ok(evaluated)),
            Ok(EvaluatedValue(first.clone()))
        );
        assert_eq!(
            edges(&complete),
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
        let Value::Lazy(sentinel_lazy) = &sentinel else {
            unreachable!("the sentinel constructor always produces a lazy value")
        };
        assert!(edges(sentinel_lazy).is_empty());
        assert!(!forced.load(Ordering::Acquire));

        let failure = EvaluationFailure::emission(sentinel.clone()).with_context(sentinel.clone());

        assert_eq!(edges(&failure), [sentinel.clone(), sentinel]);
        assert!(!forced.load(Ordering::Acquire));
    }

    #[test]
    fn recursive_edge_mutations_use_representation_gateways() {
        let source = include_str!("../../core.rs");
        assert_eq!(
            source.matches(".result.set(result)").count(),
            1,
            "LazyValue::cache must remain the sole terminal-result writer"
        );
        assert_eq!(
            source.matches("self.assignment.set(assignment)").count(),
            2,
            "promise assignment writes remain inside publish/publish_guarded"
        );
        assert_eq!(
            source.matches(".task\n            .get_or_init").count(),
            1,
            "reflection task reservation has one representation-local initializer"
        );
    }

    #[test]
    fn external_host_call_has_no_reported_semantic_edge() {
        let source = LazyValue::external_host_call(
            &values(),
            "compatibility visitor host call",
            HostCallRecord::external(
                "compatibility visitor host call",
                "src/core/managed/payload_edges.rs",
                "no captures",
            ),
            || Err(Arc::new(EvaluationFailure::message("not invoked"))),
        )
        .source_snapshot()
        .expect("the host call should remain pending");

        assert!(edges(&source).is_empty());
    }
}
