use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum EntryClass {
    CannotCollect,
    MayElect,
    ExplicitCollection,
    RequestOnly,
    Isolated,
}

#[derive(Clone, Copy, Debug)]
struct EntryRecord {
    family: &'static str,
    current: EntryClass,
    future: EntryClass,
}

const ENTRY_INVENTORY: &[EntryRecord] = &[
    EntryRecord {
        family: "CoreValueFactory::with_runtime_value_access",
        current: EntryClass::CannotCollect,
        future: EntryClass::MayElect,
    },
    EntryRecord {
        family: "CoreValueFactory::with_managed_values",
        current: EntryClass::CannotCollect,
        future: EntryClass::MayElect,
    },
    EntryRecord {
        family: "higher-level value/evaluator/compiler/reflection delegates",
        current: EntryClass::CannotCollect,
        future: EntryClass::MayElect,
    },
    EntryRecord {
        family: "recursive same-heap entry behind the factory facade",
        current: EntryClass::CannotCollect,
        future: EntryClass::MayElect,
    },
    EntryRecord {
        family: "EvaluationRuntime::collect_managed_for_maintenance",
        current: EntryClass::ExplicitCollection,
        future: EntryClass::ExplicitCollection,
    },
    EntryRecord {
        family: "explicit runtime maintenance request",
        current: EntryClass::RequestOnly,
        future: EntryClass::RequestOnly,
    },
    EntryRecord {
        family: "aggressive pre-outer-entry verification",
        current: EntryClass::MayElect,
        future: EntryClass::MayElect,
    },
    EntryRecord {
        family: "runtime construction and canonical cache initialization",
        current: EntryClass::CannotCollect,
        future: EntryClass::MayElect,
    },
    EntryRecord {
        family: "isolated CoreValueFactory and glam-gc Heap fixtures",
        current: EntryClass::Isolated,
        future: EntryClass::Isolated,
    },
];

#[test]
fn gc_activity_entry_inventory_is_complete() {
    let families = ENTRY_INVENTORY
        .iter()
        .map(|record| record.family)
        .collect::<BTreeSet<_>>();
    assert_eq!(families.len(), ENTRY_INVENTORY.len());
    assert!(
        ENTRY_INVENTORY
            .iter()
            .any(|record| record.current == EntryClass::ExplicitCollection)
    );
    assert!(
        ENTRY_INVENTORY
            .iter()
            .any(|record| record.future == EntryClass::MayElect)
    );

    let managed = include_str!("../../core/managed.rs");
    let direct_mutator_entry = [".with_", "mutator("].concat();
    let explicit_collection = [".collect_", "full("].concat();
    let collection_request = [".request_", "collection("].concat();
    assert_eq!(managed.matches(&direct_mutator_entry).count(), 2);
    assert_eq!(managed.matches(&explicit_collection).count(), 1);
    assert_eq!(managed.matches(&collection_request).count(), 1);
    assert!(managed.contains("pub(crate) fn with_runtime_value_access"));
    assert!(managed.contains("pub(crate) fn with_managed_values"));

    let core = include_str!("../../core.rs");
    let heap_constructor = ["Heap::new_", "with_policy("].concat();
    assert_eq!(core.matches(&heap_constructor).count(), 1);
    assert!(core.contains(&format!("{heap_constructor}CollectionPolicy::NoAuto)")));

    let runtime = include_str!("../runtime.rs");
    assert!(runtime.contains("pub(crate) fn collect_managed_for_maintenance"));
    assert!(runtime.contains("enable_collection_before_outer_entry_for_verification"));
}

#[test]
fn gc_readiness_plan_has_one_authoritative_activity_source() {
    let review =
        include_str!("../../../docs/reviews/GarbageCollectorReadinessIntegration_2026-10-02.md");
    let plan = include_str!("../../../docs/plans/GarbageCollectorIntegration_2026-08-19.md");

    assert!(
        review.contains(
            "`RuntimeMutationAdmission` remains the only runtime-wide authority boundary."
        )
    );
    assert!(review.contains(
        "Readiness and settlement validate\nthe revision, never the parking generation and never a sampled collector"
    ));
    assert!(plan.contains("I12A.0 selected the runtime activity/readiness protocol"));
    assert!(!plan.contains("The review must also select a durable disposition"));
}

#[test]
fn pending_finalizer_batch_has_durable_nonbusy_disposition() {
    let review =
        include_str!("../../../docs/reviews/GarbageCollectorReadinessIntegration_2026-10-02.md");
    let plan = include_str!("../../../docs/plans/GarbageCollectorIntegration_2026-08-19.md");

    assert!(review.contains("Any recoverable collector panic is represented as `RetryRequired`;"));
    assert!(review.contains(
        "For a finalizer panic, that state owns the collector's\ninactive pending batch."
    ));
    assert!(review.contains(
        "A later successful collection clears `RetryRequired`, but does not erase\n  the historical failure."
    ));
    assert!(review.contains("A permanently poisoned heap becomes `MaintenanceFailed`."));
    assert!(
        plan.contains("collector panic records a durable maintenance failure and `RetryRequired`")
    );
}
