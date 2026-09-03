//! I4F.2b.0 inventory of active destruction reachable from `core::Value`.
//!
//! Most compatibility payloads recursively release values, synchronized net
//! storage, scheduler identities, or ordinary Rust resources. Those drops are
//! passive. Three frontiers are not: host-call closure environments have an
//! unconstrained destructor, reflection reservations cancel unactivated work
//! on drop, and opaque payloads can hide reviewed external retirement. The
//! source latches below keep those exceptional paths explicit until I4F.2b.1
//! through I4F.2b.3 move their owners outside the managed graph.

use std::fs;
use std::path::Path;

use crate::core::{
    HostCallProducer, LazyCell, LazySource, OpaqueValue, ReflectionComputation, Value,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveDestructionKind {
    HostCallback,
    ReflectionReservation,
    OpaquePayload,
}

struct ActiveDestructionFrontier {
    kind: ActiveDestructionKind,
    path: &'static str,
    owner: &'static str,
    active_action: &'static str,
    extraction: &'static str,
}

const ACTIVE_DESTRUCTION_FRONTIERS: &[ActiveDestructionFrontier] = &[
    ActiveDestructionFrontier {
        kind: ActiveDestructionKind::HostCallback,
        path: "Value::Lazy -> LazyCell::source -> LazySource::HostCall -> HostCallProducer::handle -> runtime external-owner registry",
        owner: "runtime-owned HostCallOwner closure environment",
        active_action: "arbitrary host-capture destruction is externally drained",
        extraction: "I4F.2b.1 external host-call registry",
    },
    ActiveDestructionFrontier {
        kind: ActiveDestructionKind::ReflectionReservation,
        path: "Value::Lazy -> LazyCell::source -> LazySource::ReflectionTask -> ReflectionComputation::task -> ReflectionTaskReservationInner",
        owner: "lazy reflection reservation and its rooted activation",
        active_action: "unactivated reservation drop cancels coordinator work",
        extraction: "I4F.2b.2 reflection-reservation registry",
    },
    ActiveDestructionFrontier {
        kind: ActiveDestructionKind::OpaquePayload,
        path: "Value::Opaque -> OpaqueValue::payload",
        owner: "admitted Arc<dyn Any + Send + Sync> payload family",
        active_action: "type-erased or transitive external retirement",
        extraction: "I4F.2b.3 opaque-payload registry",
    },
];

struct SourceLatch {
    path: &'static str,
    needle: &'static str,
    expected: usize,
    frontier: ActiveDestructionKind,
}

// These are deliberately narrow source latches, not a Rust parser. The
// existing I4F.1 durable-owner inventory parses every production declaration
// containing values, roots, nets, type erasure, or callbacks. This companion
// table identifies the exceptional fields and drop implementations within
// that already compile-exhaustive declaration set.
const SOURCE_LATCHES: &[SourceLatch] = &[
    SourceLatch {
        path: "src/core.rs",
        needle: "type HostCallOperation = dyn Fn()",
        expected: 1,
        frontier: ActiveDestructionKind::HostCallback,
    },
    SourceLatch {
        path: "src/core.rs",
        needle: "handle: ExternalOwnerHandle",
        expected: 1,
        frontier: ActiveDestructionKind::HostCallback,
    },
    SourceLatch {
        path: "src/core/managed/external_owners.rs",
        needle: "owner: Box<dyn Any + Send + Sync>",
        expected: 1,
        frontier: ActiveDestructionKind::HostCallback,
    },
    SourceLatch {
        path: "src/core.rs",
        needle: "task: OnceLock<Result<ReflectionTaskReservation, Arc<EvaluationFailure>>>",
        expected: 1,
        frontier: ActiveDestructionKind::ReflectionReservation,
    },
    SourceLatch {
        path: "src/evaluation/session.rs",
        needle: "impl Drop for ReflectionTaskReservationInner",
        expected: 1,
        frontier: ActiveDestructionKind::ReflectionReservation,
    },
    SourceLatch {
        path: "src/evaluation/session.rs",
        needle: "self.context.cancel_reserved_task(&self.handle)",
        expected: 2,
        frontier: ActiveDestructionKind::ReflectionReservation,
    },
    SourceLatch {
        path: "src/core.rs",
        needle: "payload: Arc<dyn Any + Send + Sync>",
        expected: 1,
        frontier: ActiveDestructionKind::OpaquePayload,
    },
    SourceLatch {
        path: "src/api/value.rs",
        needle: "OpaquePayloadRecord::external(\"revocable effect token\"",
        expected: 1,
        frontier: ActiveDestructionKind::OpaquePayload,
    },
    SourceLatch {
        path: "src/api/value.rs",
        needle: "impl<T> Drop for EffectToken<T>",
        expected: 1,
        frontier: ActiveDestructionKind::OpaquePayload,
    },
    SourceLatch {
        path: "src/reflection/requests.rs",
        needle: "\"reflection task handle\"",
        expected: 1,
        frontier: ActiveDestructionKind::OpaquePayload,
    },
    SourceLatch {
        path: "src/reflection/requests.rs",
        needle: "status: Arc<EvaluationQueryHandle>",
        expected: 1,
        frontier: ActiveDestructionKind::OpaquePayload,
    },
    SourceLatch {
        path: "src/reflection/store.rs",
        needle: "impl Drop for EvaluationQueryHandle",
        expected: 1,
        frontier: ActiveDestructionKind::OpaquePayload,
    },
];

/// Compile-exhaustive direct path from `Value` to the three active frontiers.
///
/// The recursive containers and net/function variants are intentionally
/// grouped as passive shells here. Their semantic edges remain exhaustively
/// covered by the I4B-I4E visitors; only active destruction is classified in
/// this module.
#[allow(dead_code)]
fn assert_value_active_destruction_paths(value: &Value) {
    match value {
        Value::Atom(_)
        | Value::Number(_)
        | Value::Binary(_)
        | Value::List(_)
        | Value::Dict(_)
        | Value::Builtin(_)
        | Value::PartialBuiltin(_)
        | Value::Function(_)
        | Value::Net(_)
        | Value::Promised(_)
        | Value::Metadata(_) => {}
        Value::Lazy(lazy) => {
            let LazyCell {
                id: _,
                label: _,
                source: _,
                result: _,
            } = lazy.0.as_ref();
        }
        Value::Opaque(opaque) => assert_opaque_fields(opaque),
    }
}

#[allow(dead_code)]
fn assert_lazy_source_active_destruction_paths(source: &LazySource) {
    match source {
        LazySource::Error
        | LazySource::ComputedFixpoint(_)
        | LazySource::SemanticComputation(_)
        | LazySource::Access { .. }
        | LazySource::Application(_)
        | LazySource::Builtin(_)
        | LazySource::NetConstruction(_)
        | LazySource::NetComputation(_)
        | LazySource::FunctionCall { .. } => {}
        #[cfg(test)]
        LazySource::SemanticThunk(_) => {}
        LazySource::HostCall(producer) => assert_host_call_fields(producer),
        LazySource::ReflectionTask(computation) => assert_reflection_fields(computation),
    }
}

fn assert_host_call_fields(producer: &HostCallProducer) {
    let HostCallProducer { handle, record } = producer;
    let _ = (handle, record);
}

fn assert_reflection_fields(computation: &ReflectionComputation) {
    let ReflectionComputation {
        effect,
        completion,
        task,
    } = computation;
    let _ = (effect, completion, task);
}

fn assert_opaque_fields(opaque: &OpaqueValue) {
    let OpaqueValue { payload } = opaque;
    let _ = payload;
}

#[test]
fn active_value_destruction_frontiers_are_source_latched() {
    assert_eq!(ACTIVE_DESTRUCTION_FRONTIERS.len(), 3);
    for frontier in ACTIVE_DESTRUCTION_FRONTIERS {
        for (label, value) in [
            ("path", frontier.path),
            ("owner", frontier.owner),
            ("active action", frontier.active_action),
            ("extraction", frontier.extraction),
        ] {
            assert!(!value.is_empty(), "{:?} has no {label}", frontier.kind);
        }
        assert!(
            SOURCE_LATCHES
                .iter()
                .any(|latch| latch.frontier == frontier.kind),
            "{:?} has no source latch",
            frontier.kind
        );
    }

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for latch in SOURCE_LATCHES {
        let source = fs::read_to_string(manifest.join(latch.path))
            .unwrap_or_else(|error| panic!("{} should be readable: {error}", latch.path));
        assert_eq!(
            source.matches(latch.needle).count(),
            latch.expected,
            "{:?} source drift at {}: expected {} occurrence(s) of {:?}",
            latch.frontier,
            latch.path,
            latch.expected,
            latch.needle
        );
    }
}
