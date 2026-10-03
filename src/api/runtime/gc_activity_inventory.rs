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
    selected: EntryClass,
}

const ENTRY_INVENTORY: &[EntryRecord] = &[
    EntryRecord {
        family: "CoreValueFactory::with_runtime_value_access",
        current: EntryClass::CannotCollect,
        selected: EntryClass::CannotCollect,
    },
    EntryRecord {
        family: "CoreValueFactory::with_managed_values",
        current: EntryClass::CannotCollect,
        selected: EntryClass::CannotCollect,
    },
    EntryRecord {
        family: "higher-level value/evaluator/compiler/reflection delegates",
        current: EntryClass::CannotCollect,
        selected: EntryClass::CannotCollect,
    },
    EntryRecord {
        family: "recursive same-heap entry behind the factory facade",
        current: EntryClass::CannotCollect,
        selected: EntryClass::CannotCollect,
    },
    EntryRecord {
        family: "EvaluationRuntime::service_managed_collection",
        current: EntryClass::ExplicitCollection,
        selected: EntryClass::ExplicitCollection,
    },
    EntryRecord {
        family: "explicit runtime maintenance request",
        current: EntryClass::RequestOnly,
        selected: EntryClass::RequestOnly,
    },
    EntryRecord {
        family: "stable pump collector-pressure promotion",
        current: EntryClass::RequestOnly,
        selected: EntryClass::RequestOnly,
    },
    EntryRecord {
        family: "aggressive pre-outer-entry verification",
        current: EntryClass::MayElect,
        selected: EntryClass::MayElect,
    },
    EntryRecord {
        family: "runtime construction and canonical cache initialization",
        current: EntryClass::CannotCollect,
        selected: EntryClass::CannotCollect,
    },
    EntryRecord {
        family: "isolated CoreValueFactory and glam-gc Heap fixtures",
        current: EntryClass::Isolated,
        selected: EntryClass::Isolated,
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
            .any(|record| record.selected == EntryClass::MayElect)
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
    assert!(runtime.contains("pub fn request_managed_collection"));
    assert!(runtime.contains("pub fn service_managed_collection"));
    assert!(runtime.contains("enable_collection_before_outer_entry_for_verification"));
}

#[test]
fn ordinary_no_auto_entries_compile_without_runtime_lease_work() {
    let managed = include_str!("../../core/managed.rs");
    assert!(managed.contains(
        "#[cfg(not(any(test, feature = \"aggressive-gc-verification\")))]\n    #[inline]\n    fn with_maybe_collecting_entry"
    ));
    assert_eq!(
        managed
            .matches("let lease = admission.begin_gc_activity();")
            .count(),
        1,
        "only the test/aggressive entry facade may register a GC lease"
    );

    let core = include_str!("../../core.rs");
    assert_eq!(
        core.matches("gc_activity_for_entries: AtomicBool").count(),
        2,
        "the verification-only enable flag should have one declaration and one initializer"
    );
    assert!(core.contains("#[cfg(any(test, feature = \"aggressive-gc-verification\"))]"));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConstructorPolicy {
    NoAuto,
    Delegates,
    RetainsCallerRuntime,
    Isolated,
}

#[derive(Clone, Copy, Debug)]
struct ConstructorRecord {
    family: &'static str,
    policy: ConstructorPolicy,
}

const CONSTRUCTOR_INVENTORY: &[ConstructorRecord] = &[
    ConstructorRecord {
        family: "CoreValueFactory::new",
        policy: ConstructorPolicy::NoAuto,
    },
    ConstructorRecord {
        family: "EvaluationRuntime::new",
        policy: ConstructorPolicy::Delegates,
    },
    ConstructorRecord {
        family: "EvaluationRuntime::with_conflict_analysis",
        policy: ConstructorPolicy::Delegates,
    },
    ConstructorRecord {
        family: "AssemblerBuilder::default / AssemblerBuilder::new",
        policy: ConstructorPolicy::Delegates,
    },
    ConstructorRecord {
        family: "AssemblerBuilder::evaluation_runtime",
        policy: ConstructorPolicy::RetainsCallerRuntime,
    },
    ConstructorRecord {
        family: "batch configuration in `src/bin/glam/configuration/mod.rs`",
        policy: ConstructorPolicy::Delegates,
    },
    ConstructorRecord {
        family: "crate-local compiler/evaluator test factories",
        policy: ConstructorPolicy::Delegates,
    },
    ConstructorRecord {
        family: "direct glam-gc Heap fixtures",
        policy: ConstructorPolicy::Isolated,
    },
];

#[test]
fn runtime_gc_policy_review_inventory_is_complete() {
    let families = CONSTRUCTOR_INVENTORY
        .iter()
        .map(|record| record.family)
        .collect::<BTreeSet<_>>();
    assert_eq!(families.len(), CONSTRUCTOR_INVENTORY.len());
    assert_eq!(
        CONSTRUCTOR_INVENTORY
            .iter()
            .filter(|record| record.policy == ConstructorPolicy::NoAuto)
            .count(),
        1,
        "the value-domain constructor is the one authoritative runtime policy selection"
    );

    let core = include_str!("../../core.rs");
    assert_eq!(core.matches("Heap::new_with_policy(").count(), 1);
    assert!(core.contains("heap: Heap::new_with_policy(CollectionPolicy::NoAuto)"));
    assert!(!core.contains("CollectionPolicy::Automatic"));

    let runtime = include_str!("../runtime.rs");
    assert!(runtime.contains("pub fn new(worker_threads: usize) -> Result<Self, Error>"));
    assert!(runtime.contains("Self::with_conflict_analysis(worker_threads"));
    assert!(runtime.contains("core: CoreValueFactory::new(id, ids.clone())"));
    assert!(!runtime.contains("CollectionPolicy::Automatic"));

    let assembly = include_str!("../assembly.rs");
    assert!(assembly.contains("let runtime = EvaluationRuntime::new(0)"));
    assert!(assembly.contains("pub fn evaluation_runtime(mut self, runtime: EvaluationRuntime)"));

    let configuration = include_str!("../../bin/glam/configuration/mod.rs");
    assert!(configuration.contains("let runtime = EvaluationRuntime::new(0)"));
}

#[test]
fn runtime_gc_policy_plan_has_no_live_transition() {
    let heap = include_str!("../../../crates/glam-gc/src/heap.rs");
    assert!(!heap.contains("set_collection_policy"));

    let may_elect = ENTRY_INVENTORY
        .iter()
        .filter(|record| record.selected == EntryClass::MayElect)
        .collect::<Vec<_>>();
    assert_eq!(may_elect.len(), 1);
    assert_eq!(
        may_elect[0].family,
        "aggressive pre-outer-entry verification"
    );
}

#[test]
fn stable_pressure_promotion_has_one_explicit_runtime_boundary() {
    let api_runtime = include_str!("../runtime.rs");
    let admission = include_str!("../../runtime.rs");

    assert_eq!(
        api_runtime
            .matches("admission.promote_gc_pressure_request(")
            .count(),
        1,
        "the stable runtime pump should be the only pressure-promotion caller"
    );
    assert!(api_runtime.contains("let stable = maintenance.active_leases == 0"));
    assert_eq!(
        admission
            .matches("pub(crate) fn promote_gc_pressure_request(")
            .count(),
        1,
        "runtime mutation admission should own the one publication facade"
    );
    assert!(admission.contains("_settlement: &RuntimeSettlementGuard<'_>"));
}
