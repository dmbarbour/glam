//! I4F.2b inventory and closure proof for formerly active destruction below
//! `core::Value`.
//!
//! Most compatibility payloads recursively release values, synchronized net
//! storage, scheduler identities, or ordinary Rust resources. I4F.2b.1-.3
//! moved the three active frontiers into a runtime registry: host-call closure
//! environments, reflection reservation cancellation, and opaque payloads.
//! GCI5R-005 then restored reflection effect/target values to the managed
//! computation's exact trace and left only an edge-free stable observation in
//! that registry. The one-use activation permit is a transient external root,
//! not registry state. Managed-reachable values therefore retain only passive
//! handles, while arbitrary host callback environments remain deferred to
//! I10A. The source latches below keep both sides of that boundary explicit;
//! arbitrary external callback environments remain conservative host owners.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use syn::{Attribute, Item, Type};

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
    ConservativeExternalOwner,
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
        recursive_backedge: RecursiveBackedgePolicy::ConservativeExternalOwner,
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
        needle: "dyn Fn(HostCallRootBundle)",
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
        path: "src/evaluation/session.rs",
        needle: "fn reflection_reservation_storage_separates_stable_observation_from_activation_payload()",
        expected: 1,
        frontier: ActiveDestructionKind::ReflectionReservation,
    },
    SourceLatch {
        path: "src/evaluation/coordinator/task.rs",
        needle: "fn evaluation_task_handle_retains_only_identity_wait_and_weak_coordinator_authority()",
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveRaiiDisposition {
    ExternalLifecycleOwner,
    DetachedRetirementWork,
    BoundedClaimGuard,
    EdgeFreeNotificationCompanion,
    RuntimeInfrastructure,
}

#[derive(Clone, Copy, Debug)]
struct ActiveRaiiEntry {
    path: &'static str,
    owner: &'static str,
    disposition: ActiveRaiiDisposition,
    retirement: &'static str,
    verification: &'static str,
}

/// Every production `Drop` implementation which performs more than passive
/// field destruction. The complete syntax-backed source scan below prevents a
/// new active destructor from being hidden beneath a managed value.
const ACTIVE_RAII_INVENTORY: &[ActiveRaiiEntry] = &[
    ActiveRaiiEntry {
        path: "src/api/diagnostics.rs",
        owner: "DiagnosticSubscriptionInner",
        disposition: ActiveRaiiDisposition::ExternalLifecycleOwner,
        retirement: "remove one weakly addressed diagnostic subscription",
        verification: "diagnostic_bus_sequences_counts_and_delivers_only_to_current_subscribers",
    },
    ActiveRaiiEntry {
        path: "src/api/value.rs",
        owner: "EffectToken",
        disposition: ActiveRaiiDisposition::ExternalLifecycleOwner,
        retirement: "remove one opaque payload from its external token domain",
        verification: "effect_token_domain_retirement_is_external",
    },
    ActiveRaiiEntry {
        path: "src/api/value.rs",
        owner: "PromiseResolver",
        disposition: ActiveRaiiDisposition::ExternalLifecycleOwner,
        retirement: "idempotently fail and release an unresolved registered promise root",
        verification: "promise_resolver_drop_invokes_idempotent_retire_once",
    },
    ActiveRaiiEntry {
        path: "src/eval/net.rs",
        owner: "CoreCallClaim",
        disposition: ActiveRaiiDisposition::BoundedClaimGuard,
        retirement: "restore one bracketed callable claim before scoped net access ends",
        verification: "fresh_call_claim_unwind_restores_ready_work",
    },
    ActiveRaiiEntry {
        path: "src/eval/net.rs",
        owner: "CoreOperatorClaim",
        disposition: ActiveRaiiDisposition::BoundedClaimGuard,
        retirement: "restore one bracketed operator claim before scoped net access ends",
        verification: "fresh_operator_claim_unwind_restores_ready_work",
    },
    ActiveRaiiEntry {
        path: "src/evaluation/coordinator.rs",
        owner: "SessionClosureWork",
        disposition: ActiveRaiiDisposition::DetachedRetirementWork,
        retirement: "finish already-detached spark and client-demand retirements outside coordinator locks",
        verification: "client_demand_retirement_publishes_after_runtime_unlock",
    },
    ActiveRaiiEntry {
        path: "src/evaluation/coordinator/client_demand.rs",
        owner: "ClientDemandHandle",
        disposition: ActiveRaiiDisposition::ExternalLifecycleOwner,
        retirement: "idempotently abandon a pending client demand",
        verification: "client_demand_retirement_publishes_after_runtime_unlock",
    },
    ActiveRaiiEntry {
        path: "src/evaluation/executor.rs",
        owner: "EvaluationExecutor",
        disposition: ActiveRaiiDisposition::RuntimeInfrastructure,
        retirement: "stop worker admission, publish executor stop, and detach worker joins",
        verification: "executor_shutdown_wakes_idle_workers_without_owning_the_coordinator",
    },
    ActiveRaiiEntry {
        path: "src/evaluation/executor.rs",
        owner: "WorkerThreadCacheRetirement",
        disposition: ActiveRaiiDisposition::RuntimeInfrastructure,
        retirement: "release inactive collector allocation cursors at worker-thread exit",
        verification: "worker_termination_releases_inactive_collector_caches",
    },
    ActiveRaiiEntry {
        path: "src/evaluation/session.rs",
        owner: "ReflectionTaskObservationInner",
        disposition: ActiveRaiiDisposition::ExternalLifecycleOwner,
        retirement: "discard an unactivated edge-free reflection reservation",
        verification: "reflection_gate_observer_and_activation_orderings_are_forced",
    },
    ActiveRaiiEntry {
        path: "src/evaluation/session.rs",
        owner: "ReflectionTaskActivationPermit",
        disposition: ActiveRaiiDisposition::ExternalLifecycleOwner,
        retirement: "drop its one-use effect root before cancelling the reservation",
        verification: "abandoned_reflection_activation_permit_discards_reserved_work_before_owner_drain",
    },
    ActiveRaiiEntry {
        path: "src/evaluation/session.rs",
        owner: "PendingReflectionTaskInner",
        disposition: ActiveRaiiDisposition::ExternalLifecycleOwner,
        retirement: "cancel a reserved task which was never committed",
        verification: "pending_session_activation_roots_retire_with_their_reservations",
    },
    ActiveRaiiEntry {
        path: "src/evaluation/session.rs",
        owner: "EvaluationSession",
        disposition: ActiveRaiiDisposition::ExternalLifecycleOwner,
        retirement: "close the demand domain and terminalize its detached work",
        verification: "owner_session_drop_exactly_wakes_a_cross_session_task_waiter",
    },
    ActiveRaiiEntry {
        path: "src/interaction_net/runtime.rs",
        owner: "NormalizationBatchGuard",
        disposition: ActiveRaiiDisposition::BoundedClaimGuard,
        retirement: "close one borrowed normalization batch",
        verification: "scoped_normalization_batch_closes_on_unwind",
    },
    ActiveRaiiEntry {
        path: "src/interaction_net/runtime.rs",
        owner: "RuntimeNetCell",
        disposition: ActiveRaiiDisposition::EdgeFreeNotificationCompanion,
        retirement: "close and wake only the edge-free disturbance companion",
        verification: "disturbance_companion_does_not_retain_the_net_and_close_wakes_a_waiter",
    },
    ActiveRaiiEntry {
        path: "src/interaction_net/runtime.rs",
        owner: "CursorClaimGuard",
        disposition: ActiveRaiiDisposition::BoundedClaimGuard,
        retirement: "restore one unconsumed cursor claim through the net mutation gateway",
        verification: "cursor_claim_unwind_restores_both_owner_forms_to_ready",
    },
    ActiveRaiiEntry {
        path: "src/reflection/lifecycle.rs",
        owner: "ScheduledEffectRun",
        disposition: ActiveRaiiDisposition::ExternalLifecycleOwner,
        retirement: "release the scheduled effect's session lease",
        verification: "dropping_blocked_scheduled_effect_releases_its_session_and_publishes_abandonment",
    },
    ActiveRaiiEntry {
        path: "src/reflection/store.rs",
        owner: "EvaluationQueryHandle",
        disposition: ActiveRaiiDisposition::ExternalLifecycleOwner,
        retirement: "enqueue private-volume cleanup through a weak query domain",
        verification: "query_state_is_transactional_and_retired_after_the_last_handle",
    },
    ActiveRaiiEntry {
        path: "src/runtime.rs",
        owner: "RuntimeMutationGuard",
        disposition: ActiveRaiiDisposition::BoundedClaimGuard,
        retirement: "release mutation admission before publishing a conservative activity wake",
        verification: "runtime_input_admission_wakes_after_releasing_mutation_admission",
    },
];

#[derive(Clone, Copy, Debug)]
struct ExternalOwnerFamily {
    owner: &'static str,
    strong_capability: &'static str,
    registered_roots: &'static str,
    explicit_retirement: &'static str,
    drop_fallback: &'static str,
    terminal_semantics: &'static str,
    lock_boundary: &'static str,
    verification: &'static str,
    changed_after_i5: bool,
}

/// The I5E lifecycle families consumed by I9F. Several Rust types may
/// implement one family (notably reflection reservation and session closure),
/// but no managed allocation may contain any of these active owners.
const EXTERNAL_OWNER_FAMILIES: &[ExternalOwnerFamily] = &[
    ExternalOwnerFamily {
        owner: "promise resolver",
        strong_capability: "temporary upgraded RuntimeValueDomain during completion",
        registered_roots: "one optional ManagedPromiseRoot",
        explicit_retirement: "PromiseResolver::retire / take_for_completion",
        drop_fallback: "fail unresolved promise once when the value domain remains live",
        terminal_semantics: "one permanent promise failure and one completion wake",
        lock_boundary: "drop upgraded domain access after publication; wake outside component locks",
        verification: "promise_resolver_drop_invokes_idempotent_retire_once",
        changed_after_i5: false,
    },
    ExternalOwnerFamily {
        owner: "promise producer root",
        strong_capability: "task coordinator or local producer owner",
        registered_roots: "one ManagedPromiseRoot until terminal publication",
        explicit_retirement: "guarded or detached promise publication removes the producer root",
        drop_fallback: "owning session closure abandons or cancels unresolved production",
        terminal_semantics: "exactly one complete, failed, cancelled, or abandoned wait result",
        lock_boundary: "PromiseProducerPublication carries the removed root through unlock and wake",
        verification: "promise_settlement_releases_task_and_local_owner_roots",
        changed_after_i5: false,
    },
    ExternalOwnerFamily {
        owner: "evaluation session",
        strong_capability: "Arc<EvaluationWorkCoordinator>",
        registered_roots: "roots held by its detached tasks, sparks, demands, and reports",
        explicit_retirement: "EvaluationWorkCoordinator::close_session",
        drop_fallback: "EvaluationSession::drop performs closure",
        terminal_semantics: "owned work is abandoned or cancelled and waiters are woken once",
        lock_boundary: "SessionClosureWork destroys payloads after coordinator mutation",
        verification: "owner_session_drop_exactly_wakes_a_cross_session_task_waiter",
        changed_after_i5: false,
    },
    ExternalOwnerFamily {
        owner: "client demand",
        strong_capability: "weak work coordinator plus strong result cell",
        registered_roots: "operation and terminal result roots owned by coordinator records",
        explicit_retirement: "ClientDemandHandle::abandon_inner",
        drop_fallback: "ClientDemandHandle::drop abandons pending work",
        terminal_semantics: "one abandoned result; completion and abandonment race through one cell",
        lock_boundary: "ClientDemandRetirement finishes detached payloads after coordinator unlock",
        verification: "client_demand_retirement_publishes_after_runtime_unlock",
        changed_after_i5: false,
    },
    ExternalOwnerFamily {
        owner: "reflection reservation",
        strong_capability: "edge-free task observation plus one-use activation context",
        registered_roots: "effect root exists only in ReflectionTaskActivationPermit",
        explicit_retirement: "begin activation or cancel/discard reservation",
        drop_fallback: "observation, activation permit, and pending task cancel only while reserved",
        terminal_semantics: "activation or cancellation wins exactly once",
        lock_boundary: "activation payload drops before coordinator cancellation; no managed finalizer acts",
        verification: "reflection_gate_observer_and_activation_orderings_are_forced",
        changed_after_i5: true,
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
    let HostCallProducer {
        handle,
        record,
        captures,
    } = producer;
    let _ = (handle, record, captures);
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
    let function = FunctionValue::new(NetValue::new(runtime.duplicate_for_test(values)), 1);
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
            HostCallRecord::external_without_semantic_values(
                "I4F.2b passive closure host probe",
                "src/core/managed/active_owner_inventory.rs",
                "one external drop probe",
            ),
            [],
            move |_| {
                let _ = &host_probe;
                unreachable!("passive-closure collection must not invoke a host callback")
            },
        ),
        Value::Promised(PromisedValue::new(values, "I4F.2b passive closure promise")),
        values.initial_metadata(),
        Value::Opaque(OpaqueValue::new(values, opaque_probe)),
    ]
}

fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("test")
            || match &attribute.meta {
                syn::Meta::List(list) if list.path.is_ident("cfg") => {
                    list.tokens.to_string().split_whitespace().any(|token| {
                        token.trim_matches(|character: char| !character.is_ascii_alphanumeric())
                            == "test"
                    })
                }
                _ => false,
            }
    })
}

fn drop_type_name(value: &Type) -> Option<String> {
    match value {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        Type::Group(group) => drop_type_name(&group.elem),
        Type::Paren(paren) => drop_type_name(&paren.elem),
        Type::Reference(reference) => drop_type_name(&reference.elem),
        _ => None,
    }
}

fn collect_drop_impls(
    relative: &Path,
    items: &[Item],
    found: &mut BTreeMap<String, ActiveRaiiDisposition>,
) {
    for item in items {
        match item {
            Item::Impl(item) if !is_test_only(&item.attrs) => {
                let is_drop = item
                    .trait_
                    .as_ref()
                    .and_then(|(_, path, _)| path.segments.last())
                    .is_some_and(|segment| segment.ident == "Drop");
                if !is_drop {
                    continue;
                }
                let owner = drop_type_name(&item.self_ty).unwrap_or_else(|| {
                    panic!(
                        "{} contains a Drop target which the I9 inventory cannot name",
                        relative.display()
                    )
                });
                let key = format!("{}::{owner}", relative.display());
                let expected = ACTIVE_RAII_INVENTORY
                    .iter()
                    .find(|entry| entry.path == relative.to_string_lossy() && entry.owner == owner)
                    .unwrap_or_else(|| panic!("unclassified production Drop implementation {key}"));
                assert!(
                    found.insert(key.clone(), expected.disposition).is_none(),
                    "duplicate production Drop implementation {key}"
                );
            }
            Item::Mod(module) if !is_test_only(&module.attrs) => {
                if let Some((_, nested)) = &module.content {
                    collect_drop_impls(relative, nested, found);
                }
            }
            _ => {}
        }
    }
}

fn collect_rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("the source tree should be readable") {
        let path = entry.expect("a source entry should be readable").path();
        if path.is_dir() {
            collect_rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
}

fn is_production_drop_source(relative: &Path) -> bool {
    if relative
        .components()
        .any(|component| component.as_os_str() == "tests")
    {
        return false;
    }
    if relative
        .file_name()
        .is_some_and(|name| name == "tests.rs" || name == "test_support.rs")
    {
        return false;
    }
    !matches!(
        relative.to_str(),
        Some(
            "src/core/managed/active_owner_inventory.rs"
                | "src/core/managed/containment_inventory.rs"
                | "src/core/managed/durable_owner_inventory.rs"
                | "src/core/managed/recursive_identity_inventory.rs"
        )
    )
}

fn production_drop_inventory() -> BTreeMap<String, ActiveRaiiDisposition> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);
    sources.sort();
    let mut found = BTreeMap::new();
    for path in sources {
        let relative = path
            .strip_prefix(manifest)
            .expect("a discovered source should belong to this package");
        if !is_production_drop_source(relative) {
            continue;
        }
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} should be readable: {error}", relative.display()));
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} should parse: {error}", relative.display()));
        collect_drop_impls(relative, &syntax.items, &mut found);
    }
    found
}

fn declaration_fragment<'source>(source: &'source str, declaration: &str) -> &'source str {
    let start = source
        .find(declaration)
        .unwrap_or_else(|| panic!("missing declaration {declaration}"));
    let body = &source[start..];
    let end = body
        .find("\n}")
        .unwrap_or_else(|| panic!("unterminated declaration {declaration}"));
    &body[..end + 2]
}

#[test]
fn active_external_raii_inventory_is_reconciled() {
    let actual = production_drop_inventory();
    let expected = ACTIVE_RAII_INVENTORY
        .iter()
        .map(|entry| {
            (
                format!("{}::{}", entry.path, entry.owner),
                entry.disposition,
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual, expected,
        "production Drop inventory drift requires an external-lifecycle review"
    );

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut verification_sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut verification_sources);
    let verification_sources = verification_sources
        .into_iter()
        .filter(|path| !path.ends_with("core/managed/active_owner_inventory.rs"))
        .map(|path| {
            fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()))
        })
        .collect::<Vec<_>>();
    let verification_exists = |name: &str| {
        let declaration = format!("fn {name}(");
        verification_sources
            .iter()
            .any(|source| source.contains(&declaration))
    };

    for entry in ACTIVE_RAII_INVENTORY {
        assert!(
            !entry.retirement.is_empty(),
            "{} has no retirement",
            entry.owner
        );
        assert!(
            !entry.verification.is_empty(),
            "{} has no verification",
            entry.owner
        );
        assert!(
            verification_exists(entry.verification),
            "{} names missing verification {}",
            entry.owner,
            entry.verification
        );
    }
    assert_eq!(
        EXTERNAL_OWNER_FAMILIES
            .iter()
            .filter(|owner| owner.changed_after_i5)
            .map(|owner| owner.owner)
            .collect::<Vec<_>>(),
        ["reflection reservation"],
        "I6 reflection activation is the only active-owner delta after I5"
    );
    for owner in EXTERNAL_OWNER_FAMILIES {
        for (label, value) in [
            ("owner", owner.owner),
            ("strong capability", owner.strong_capability),
            ("registered roots", owner.registered_roots),
            ("explicit retirement", owner.explicit_retirement),
            ("Drop fallback", owner.drop_fallback),
            ("terminal semantics", owner.terminal_semantics),
            ("lock boundary", owner.lock_boundary),
            ("verification", owner.verification),
        ] {
            assert!(!value.is_empty(), "external owner has no {label}");
        }
        assert!(
            verification_exists(owner.verification),
            "{} names missing verification {}",
            owner.owner,
            owner.verification
        );
    }
}

#[test]
fn managed_graph_reaches_no_active_raii_owner() {
    let managed_value = include_str!("value_node.rs");
    let recursive = include_str!("recursive_cells.rs");
    let managed_declarations = [
        declaration_fragment(managed_value, "pub(crate) struct ManagedValueNode"),
        declaration_fragment(recursive, "pub(crate) struct ManagedLazyCell"),
        declaration_fragment(recursive, "pub(crate) struct ManagedPromiseCell"),
        declaration_fragment(recursive, "pub(crate) struct ManagedCoreNetCell"),
    ];
    for declaration in managed_declarations {
        for forbidden in [
            "PromiseResolver",
            "TaskOwnedPromiseObligation",
            "LocalPromiseObligation",
            "EvaluationSession",
            "ClientDemandHandle",
            "ReflectionTaskObservationInner",
            "ReflectionTaskActivationPermit",
            "PendingReflectionTaskInner",
            "ScheduledEffectRun",
            "EvaluationQueryHandle",
            "DiagnosticSubscriptionInner",
            "EffectToken",
            "HostCallOwner",
            "ReflectionComputationOwner",
        ] {
            assert!(
                !declaration.contains(forbidden),
                "managed declaration reaches active RAII owner {forbidden}: {declaration}"
            );
        }
    }

    let contract = include_str!("../managed.rs");
    assert!(contract.contains("Any external owner which performs active retirement"));
    assert!(contract.contains("must remain outside"));
    assert!(contract.contains("the managed graph and hold its runtime capability"));
}

#[test]
fn managed_payloads_have_no_strong_value_domain_backedge() {
    let managed_value = include_str!("value_node.rs");
    let recursive = include_str!("recursive_cells.rs");
    let core = include_str!("../../core.rs");
    let managed_declarations = [
        declaration_fragment(managed_value, "pub(crate) struct ManagedValueNode"),
        declaration_fragment(recursive, "pub(crate) struct ManagedLazyCell"),
        declaration_fragment(recursive, "pub(crate) struct ManagedPromiseCell"),
        declaration_fragment(recursive, "pub(crate) struct ManagedCoreNetCell"),
        declaration_fragment(core, "pub struct OpaqueValue"),
        declaration_fragment(core, "pub(crate) struct HostCallProducer"),
        declaration_fragment(core, "pub(crate) struct ReflectionComputation"),
    ];
    for declaration in managed_declarations {
        for forbidden in [
            "RuntimeValueDomain",
            "CoreValueFactory",
            "RuntimeValueAccess",
            "RuntimeValueObserver",
            "EvaluationRuntime",
            "RuntimeSharedResources",
            "RuntimeValueRoot",
            "Root<",
        ] {
            assert!(
                !declaration.contains(forbidden),
                "managed-reachable declaration regained value-domain authority through {forbidden}: {declaration}"
            );
        }
    }

    // The direct declaration latch composes with the exhaustive variant,
    // callback, opaque-family, recursive-identity, and active-owner scans. It
    // does not pretend that spelling searches replace those transitive gates.
    let containment = include_str!("containment_inventory.rs");
    for verification in [
        "fn closure_and_opaque_constructor_inventory_is_classified()",
        "fn deferred_closure_constructor_inventory_is_reconciled()",
        "fn opaque_family_inventory_is_reconciled()",
        "fn opaque_type_erasure_inventory_is_reconciled()",
    ] {
        assert_eq!(containment.matches(verification).count(), 1);
    }
}

#[test]
fn opaque_external_lifecycle_matches_active_raii_inventory() {
    let active = production_drop_inventory();
    assert_eq!(
        active.get("src/api/value.rs::EffectToken"),
        Some(&ActiveRaiiDisposition::ExternalLifecycleOwner)
    );
    assert_eq!(
        active.get("src/reflection/store.rs::EvaluationQueryHandle"),
        Some(&ActiveRaiiDisposition::ExternalLifecycleOwner)
    );
    assert!(!active.contains_key("src/reflection/requests.rs::TaskHandleCell"));
    assert!(!active.contains_key("src/diagnostic.rs::CompilationOrigin"));
    assert!(!active.contains_key("src/eval/builtins/net/construction.rs::ConstructionPort"));
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
    let conservative = ACTIVE_DESTRUCTION_FRONTIERS
        .iter()
        .filter(|frontier| {
            frontier.recursive_backedge == RecursiveBackedgePolicy::ConservativeExternalOwner
        })
        .map(|frontier| frontier.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        conservative,
        [ActiveDestructionKind::HostCallback],
        "only arbitrary host-callback environments use conservative external ownership"
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
    assert!(core.contains("dyn Fn(HostCallRootBundle) -> Result<RuntimeValueRoot"));
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
fn arbitrary_host_callback_root_backedge_is_conservative_external_ownership() {
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
        HostCallRecord::external_without_semantic_values(
            "I5F.4 external-root backedge",
            "src/core/managed/active_owner_inventory.rs",
            "one explicitly removable same-runtime root",
        ),
        [],
        move |_| {
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
fn production_reflection_gate_target_backedge_reclaims_without_an_external_root() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let baseline = values
        .collect_managed_for_test()
        .expect("the reflection-target fixture should start collectible");
    {
        let promise = PromisedValue::new(&values, "reflection target backedge");
        let reflected = values.with_runtime_value_access(|access| {
            Value::Lazy(LazyValue::from_reflection_gate_in(
                &access,
                values.unit(),
                Value::Promised(promise.clone()),
            ))
        });
        set_test_promise(&values, &promise, reflected.clone())
            .expect("the reflection target promise should start unassigned");
    }

    let reclaimed = values
        .collect_managed_for_test()
        .expect("the direct reflection target edge should close its managed cycle");
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
