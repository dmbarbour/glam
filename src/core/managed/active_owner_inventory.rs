//! I4F.2b inventory and closure proof for formerly active destruction below
//! `core::Value`.
//!
//! Most compatibility payloads recursively release values, synchronized net
//! storage, scheduler identities, or ordinary Rust resources. I4F.2b.1-.3
//! moved the three active frontiers into a runtime registry: host-call closure
//! environments, reflection reservations, and opaque payloads. Managed-
//! reachable values now retain only passive handles. The source latches below
//! keep both sides of that boundary explicit.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::core::{
    Builtin, ClosedCompatibilityValue, Dict, FunctionValue, HostCallProducer, HostCallRecord,
    LazySource, LazyValue, List, NetValue, OpaquePayloadFamily, OpaquePayloadRecord, OpaqueValue,
    PromisedValue, ReflectionComputation, ReflectionComputationOwner, Value, set_test_promise,
};
use crate::core_net::CoreSpecialization;
use crate::interaction_net::NetBuilder;
use crate::runtime::RuntimeValueRoot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveDestructionKind {
    HostCallback,
    ReflectionReservation,
    OpaquePayload,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecursiveBackedgePolicy {
    DeferredToI10A,
    ClosedByI6D1,
    ForbiddenByAdmission,
}

struct ActiveDestructionFrontier {
    kind: ActiveDestructionKind,
    path: &'static str,
    owner: &'static str,
    active_action: &'static str,
    extraction: &'static str,
    recursive_backedge: RecursiveBackedgePolicy,
}

const ACTIVE_DESTRUCTION_FRONTIERS: &[ActiveDestructionFrontier] = &[
    ActiveDestructionFrontier {
        kind: ActiveDestructionKind::HostCallback,
        path: "Value::Lazy -> LazyCell::source -> LazySource::HostCall -> HostCallProducer::handle -> runtime external-owner registry",
        owner: "runtime-owned HostCallOwner closure environment",
        active_action: "arbitrary host-capture destruction is externally drained",
        extraction: "I4F.2b.1 external host-call registry",
        recursive_backedge: RecursiveBackedgePolicy::DeferredToI10A,
    },
    ActiveDestructionFrontier {
        kind: ActiveDestructionKind::ReflectionReservation,
        path: "Value::Lazy -> LazyCell::source -> LazySource::ReflectionTask -> ReflectionComputation::handle -> runtime external-owner registry",
        owner: "runtime-owned edge-free reflection task observation",
        active_action: "unactivated reservation cancellation is externally drained",
        extraction: "I4F.2b.2 registry with I6D.1 semantic-edge closure",
        recursive_backedge: RecursiveBackedgePolicy::ClosedByI6D1,
    },
    ActiveDestructionFrontier {
        kind: ActiveDestructionKind::OpaquePayload,
        path: "Value::Opaque -> OpaqueValue::handle -> runtime external-owner registry",
        owner: "runtime-owned admitted opaque payload family",
        active_action: "type-erased or transitive external retirement is externally drained",
        extraction: "I4F.2b.3 opaque-payload registry",
        recursive_backedge: RecursiveBackedgePolicy::ForbiddenByAdmission,
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
        needle: "pub(crate) struct HostCallProducer {",
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
        needle: "pub(crate) struct ReflectionComputation {",
        expected: 1,
        frontier: ActiveDestructionKind::ReflectionReservation,
    },
    SourceLatch {
        path: "src/core.rs",
        needle: "task: OnceLock<Result<ReflectionTaskObservation, Arc<str>>>",
        expected: 1,
        frontier: ActiveDestructionKind::ReflectionReservation,
    },
    SourceLatch {
        path: "src/evaluation/session.rs",
        needle: "struct ReflectionTaskObservationInner {",
        expected: 1,
        frontier: ActiveDestructionKind::ReflectionReservation,
    },
    SourceLatch {
        path: "src/evaluation/session.rs",
        needle: "pub(crate) struct ReflectionTaskActivationPermit {",
        expected: 1,
        frontier: ActiveDestructionKind::ReflectionReservation,
    },
    SourceLatch {
        path: "src/evaluation/coordinator/task.rs",
        needle: "pub(crate) fn discard_reservation(&self)",
        expected: 1,
        frontier: ActiveDestructionKind::ReflectionReservation,
    },
    SourceLatch {
        path: "src/core.rs",
        needle: "pub struct OpaqueValue {",
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
            let _: &LazyValue = lazy;
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
        target,
        handle,
        completion,
    } = computation;
    let _: &Value = effect;
    let _: &Option<Value> = target;
    let _ = (handle, completion);
}

fn assert_reflection_owner_fields(owner: &ReflectionComputationOwner) {
    let ReflectionComputationOwner { task } = owner;
    let _: &std::sync::OnceLock<Result<crate::evaluation::ReflectionTaskObservation, Arc<str>>> =
        task;
}

fn assert_opaque_fields(opaque: &OpaqueValue) {
    let OpaqueValue { handle } = opaque;
    let _ = handle;
}

fn assert_closed_compatibility_fields(value: &ClosedCompatibilityValue) {
    let ClosedCompatibilityValue { value, drops } = value;
    let _: &Value = value;
    let _: &Arc<AtomicUsize> = drops;
}

pub(super) fn compatibility_variant_name(value: &Value) -> &'static str {
    match value {
        Value::Atom(_) => "atom",
        Value::Number(_) => "number",
        Value::Binary(_) => "binary",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        Value::Builtin(_) => "builtin",
        Value::PartialBuiltin(_) => "partial builtin",
        Value::Function(_) => "function",
        Value::Net(_) => "net",
        Value::Lazy(_) => "lazy",
        Value::Promised(_) => "promised",
        Value::Metadata(_) => "metadata",
        Value::Opaque(_) => "opaque",
    }
}

struct ExternalDropProbe(Arc<AtomicUsize>);

impl Drop for ExternalDropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: this test payload contains no Glam value or managed pointer. It is
// intentionally external so the closure test can distinguish managed
// finalization from the later safe registry drain.
unsafe impl OpaquePayloadFamily for ExternalDropProbe {
    const PAYLOAD_RECORD: OpaquePayloadRecord = OpaquePayloadRecord::external(
        "I4F.2b passive-closure opaque probe",
        "src/core/managed/active_owner_inventory.rs",
    );
}

fn closed_net(values: &crate::core::CoreValueFactory) -> crate::core_net::CoreRuntimeNet {
    let mut builder = NetBuilder::<CoreSpecialization>::new();
    let exposed = builder.data(Value::Number(0.into()));
    values.instantiate_core_net(&builder.finish(exposed))
}

pub(super) fn closed_compatibility_variants(
    values: &crate::core::CoreValueFactory,
    active_drops: &Arc<AtomicUsize>,
) -> Vec<Value> {
    let runtime = closed_net(values);
    let function = FunctionValue::new(NetValue::new(runtime.clone()), 1);
    let host_probe = ExternalDropProbe(Arc::clone(active_drops));
    let opaque_probe = Arc::new(ExternalDropProbe(Arc::clone(active_drops)));

    vec![
        values.unit(),
        Value::Number(1.into()),
        Value::Binary(Bytes::from_static(b"closed")),
        Value::List(List::from_values(vec![Value::Number(2.into())])),
        Value::Dict(Dict::new_sync().insert(
            crate::core::Key::binary_from_text("field"),
            Value::Number(3.into()),
        )),
        Value::Builtin(Builtin::Append),
        Value::builtin_call(values, Builtin::Append, vec![Value::Number(4.into())]),
        Value::Function(function),
        Value::Net(NetValue::new(runtime)),
        Value::external_host_call(
            values,
            "I4F.2b passive closure host probe",
            HostCallRecord::external(
                "I4F.2b passive closure host probe",
                "src/core/managed/active_owner_inventory.rs",
                "one external drop probe",
            ),
            move || {
                let _ = &host_probe;
                unreachable!("passive-closure collection must not invoke a host callback")
            },
        ),
        Value::Promised(PromisedValue::new(values, "I4F.2b passive closure promise")),
        values.initial_metadata(),
        Value::Opaque(OpaqueValue::new(values, opaque_probe)),
    ]
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

#[test]
fn external_owner_recursive_backedges_are_explicitly_classified() {
    let deferred = ACTIVE_DESTRUCTION_FRONTIERS
        .iter()
        .filter(|frontier| frontier.recursive_backedge == RecursiveBackedgePolicy::DeferredToI10A)
        .map(|frontier| frontier.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        deferred,
        [ActiveDestructionKind::HostCallback],
        "only arbitrary host-callback environments remain I10A work"
    );
    let closed = ACTIVE_DESTRUCTION_FRONTIERS
        .iter()
        .filter(|frontier| frontier.recursive_backedge == RecursiveBackedgePolicy::ClosedByI6D1)
        .map(|frontier| frontier.kind)
        .collect::<Vec<_>>();
    assert_eq!(closed, [ActiveDestructionKind::ReflectionReservation]);
    let forbidden = ACTIVE_DESTRUCTION_FRONTIERS
        .iter()
        .filter(|frontier| {
            frontier.recursive_backedge == RecursiveBackedgePolicy::ForbiddenByAdmission
        })
        .map(|frontier| frontier.kind)
        .collect::<Vec<_>>();
    assert_eq!(forbidden, [ActiveDestructionKind::OpaquePayload]);

    let core = include_str!("../../core.rs");
    assert_eq!(
        core.matches(".external_owners.insert(").count(),
        2,
        "inline external-owner insertions require a recursive-backedge classification"
    );
    assert_eq!(
        core.matches(".external_owners\n            .insert(")
            .count(),
        1,
        "the formatted host-call insertion requires a recursive-backedge classification"
    );
    assert!(core.contains("type HostCallOperation = dyn Fn() -> Result<RuntimeValueRoot"));
    let _: fn(&ReflectionComputationOwner) = assert_reflection_owner_fields;
    assert!(core.contains("effect: Value"));
    assert!(core.contains("target: Option<Value>"));
    assert!(!core.contains("effect: RuntimeValueRoot"));
    assert!(!core.contains("target: Option<RuntimeValueRoot>"));
    assert_eq!(
        core.matches("fn new<T: OpaquePayloadFamily>").count(),
        1,
        "opaque insertion must remain gated by the reviewed family contract"
    );

    let managed = include_str!("../managed.rs");
    assert!(managed.contains("The payload must contain no bare `Gc`"));
    assert!(managed.contains("`RuntimeValueRoot`, or other unreported managed edge"));
}

#[test]
fn host_callback_root_backedge_remains_explicitly_deferred_to_i10a() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let baseline = values
        .collect_managed_for_test()
        .expect("the host-backedge fixture should start collectible");
    let captured_root = Arc::new(Mutex::new(None::<RuntimeValueRoot>));
    let callback_capture = Arc::clone(&captured_root);
    let value = Value::external_host_call(
        &values,
        "I5F.4 external-root backedge",
        HostCallRecord::external(
            "I5F.4 external-root backedge",
            "src/core/managed/active_owner_inventory.rs",
            "one explicitly removable same-runtime root",
        ),
        move || {
            let _ = &callback_capture;
            Err(Arc::new(crate::core::EvaluationFailure::message(
                "the containment fixture must not invoke its callback",
            )))
        },
    );
    *captured_root
        .lock()
        .expect("the host capture should not be poisoned") =
        Some(RuntimeValueRoot::new(&values, value));

    let retained = values
        .collect_managed_for_test()
        .expect("the explicit external root should retain its lazy");
    assert_eq!(retained.root_entries(), baseline.root_entries() + 1);
    assert_eq!(
        retained.marked_slots(),
        baseline.marked_slots() + 2,
        "the external root owns one value shell which reaches the managed lazy"
    );
    assert_eq!(values.external_owner_count_for_test(), 1);

    drop(
        captured_root
            .lock()
            .expect("the host capture should not be poisoned")
            .take(),
    );
    let reclaimed = values
        .collect_managed_for_test()
        .expect("removing the explicit external root should make the lazy collectible");
    assert_eq!(reclaimed.root_entries(), baseline.root_entries());
    assert_eq!(reclaimed.finalized_slots(), 2);
    assert_eq!(values.drain_external_owners_for_test(), 1);
    assert_eq!(values.external_owner_count_for_test(), 0);
}

#[test]
fn production_reflection_result_edges_do_not_need_an_external_root() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let baseline = values
        .collect_managed_for_test()
        .expect("the reflection-edge fixture should start collectible");
    {
        let promise = PromisedValue::new(&values, "reflection effect backedge");
        let reflected = values.with_runtime_value_access(|access| {
            Value::reflection_task_result_in(&access, Value::Promised(promise.clone()))
        });
        set_test_promise(&values, &promise, reflected.clone())
            .expect("the reflection backedge promise should start unassigned");
    }
    let reclaimed = values
        .collect_managed_for_test()
        .expect("direct reflection edges should be traced without registered roots");
    assert_eq!(reclaimed.root_entries(), baseline.root_entries());
    assert_eq!(reclaimed.marked_slots(), baseline.marked_slots());
    assert_eq!(reclaimed.finalized_slots(), 2);
    assert_eq!(values.drain_external_owners_for_test(), 1);
    assert_eq!(values.external_owner_count_for_test(), 0);
}

#[test]
fn every_real_value_variant_has_passive_managed_destruction() {
    assert_eq!(
        <ClosedCompatibilityValue as super::ManagedFamily>::DROP_RECORD.fields(),
        (
            "I4F.2b closed compatibility value fixture",
            "src/core/managed.rs",
            "direct Drop updates only an external atomic counter",
            "compatibility Value ownership is passive after active-owner extraction",
        )
    );
    let _: fn(&ClosedCompatibilityValue) = assert_closed_compatibility_fields;

    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let managed_drops = Arc::new(AtomicUsize::new(0));
    let active_drops = Arc::new(AtomicUsize::new(0));
    let variants = closed_compatibility_variants(&values, &active_drops);
    let baseline = values
        .collect_managed_for_test()
        .expect("canonical roots should collect before the compatibility fixture");

    let roots = values.with_managed_values(|scope| {
        let allocator = scope
            .allocator::<ClosedCompatibilityValue>()
            .expect("the closed compatibility wrapper should fit a managed run");
        variants
            .into_iter()
            .map(|value| {
                scope.root(allocator.alloc(ClosedCompatibilityValue::new(value, &managed_drops)))
            })
            .collect::<Vec<_>>()
    });

    let live = values
        .collect_managed_for_test()
        .expect("rooted closed compatibility values should survive collection");
    assert_eq!(live.marked_slots(), baseline.marked_slots() + roots.len());
    assert_eq!(managed_drops.load(Ordering::Relaxed), 0);
    assert_eq!(active_drops.load(Ordering::Relaxed), 0);
    values.with_managed_values(|scope| {
        assert_eq!(
            roots
                .iter()
                .map(|root| compatibility_variant_name(scope.get(root).value()))
                .collect::<Vec<_>>(),
            [
                "atom",
                "number",
                "binary",
                "list",
                "dict",
                "builtin",
                "partial builtin",
                "function",
                "net",
                "lazy",
                "promised",
                "metadata",
                "opaque",
            ]
        );
    });

    drop(roots);
    let dead = values
        .collect_managed_for_test()
        .expect("unrooted closed compatibility values should be reclaimed");
    assert_eq!(dead.finalized_slots(), 13);
    assert_eq!(managed_drops.load(Ordering::Relaxed), 13);
    assert_eq!(
        active_drops.load(Ordering::Relaxed),
        0,
        "managed finalization must not retire external callback or opaque owners"
    );

    assert_eq!(values.drain_external_owners_for_test(), 2);
    assert_eq!(active_drops.load(Ordering::Relaxed), 2);
}
