//! Independent Gate G2 composition over the authoritative integration gates.
//!
//! The owning modules retain their exact source-count baselines. This module
//! deliberately does not duplicate them: it proves that the final gate still
//! names every required concern and that each authoritative verification is
//! present at its reviewed source boundary.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

struct GateEvidence {
    concern: &'static str,
    path: &'static str,
    verification: &'static str,
}

const SOURCE_EVIDENCE: &[GateEvidence] = &[
    GateEvidence {
        concern: "values and traces",
        path: "src/core/managed/value_node.rs",
        verification: "managed_value_node_family_contract_and_lifecycle",
    },
    GateEvidence {
        concern: "managed recursive identities",
        path: "src/core/managed/recursive_identity_inventory.rs",
        verification: "recursive_identity_source_inventory_is_complete",
    },
    GateEvidence {
        concern: "managed family layout and reclamation",
        path: "src/core/managed/recursive_cells.rs",
        verification: "recursive_cell_family_contracts_and_registered_root_lifecycles",
    },
    GateEvidence {
        concern: "roots",
        path: "src/core/managed/durable_owner_inventory.rs",
        verification: "runtime_root_source_inventory_is_reconciled",
    },
    GateEvidence {
        concern: "runtime root lifecycle",
        path: "src/core/managed/durable_owner_inventory.rs",
        verification: "runtime_root_lifecycle_delta_is_reconciled",
    },
    GateEvidence {
        concern: "external active owners",
        path: "src/core/managed/active_owner_inventory.rs",
        verification: "active_external_raii_inventory_is_reconciled",
    },
    GateEvidence {
        concern: "managed owner exclusion",
        path: "src/core/managed/active_owner_inventory.rs",
        verification: "managed_graph_reaches_no_active_raii_owner",
    },
    GateEvidence {
        concern: "closures and opaque families",
        path: "src/core/managed/containment_inventory.rs",
        verification: "final_closure_opaque_and_any_inventory_is_reconciled",
    },
    GateEvidence {
        concern: "opaque family constructors and readers",
        path: "src/core/managed/containment_inventory.rs",
        verification: "opaque_family_inventory_is_reconciled",
    },
    GateEvidence {
        concern: "opaque type erasure",
        path: "src/core/managed/containment_inventory.rs",
        verification: "opaque_type_erasure_inventory_is_reconciled",
    },
    GateEvidence {
        concern: "caches",
        path: "src/core/runtime_cache.rs",
        verification: "runtime_cache_family_source_inventory_is_complete",
    },
    GateEvidence {
        concern: "persistent collections",
        path: "src/core/managed/payload_edges/persistent.rs",
        verification: "persistent_representation_to_visitor_inventory_is_complete",
    },
    GateEvidence {
        concern: "interaction-net mutations",
        path: "src/core_net.rs",
        verification: "managed_core_net_semantic_writers_use_exact_delta_gateways",
    },
    GateEvidence {
        concern: "regional allocation callers",
        path: "src/core/managed/recursive_cells.rs",
        verification: "regional_constructor_callers_are_exact_and_classified",
    },
    GateEvidence {
        concern: "first-owner publication chronology",
        path: "src/core/managed/recursive_cells.rs",
        verification: "fresh_managed_facades_survive_until_first_publication",
    },
    GateEvidence {
        concern: "managed domain backedges",
        path: "src/core/managed/active_owner_inventory.rs",
        verification: "managed_payloads_have_no_strong_value_domain_backedge",
    },
    GateEvidence {
        concern: "managed access regions",
        path: "src/evaluation/access_inventory.rs",
        verification: "all_managed_entries_have_bounded_mutator_regions",
    },
    GateEvidence {
        concern: "effect callback boundaries",
        path: "src/reflection/machine/tests.rs",
        verification: "effect_interpreter_callbacks_do_not_inherit_evaluator_mutators",
    },
    GateEvidence {
        concern: "reflection activation boundaries",
        path: "src/eval/tests.rs",
        verification: "reflection_gate_reserves_inside_and_activates_outside_scope",
    },
    GateEvidence {
        concern: "event callback boundaries",
        path: "src/api/tests/runtime_tests.rs",
        verification: "event_delivery_invokes_callback_without_mutator",
    },
    GateEvidence {
        concern: "diagnostic callback boundaries",
        path: "src/bin/glam/rendering.rs",
        verification: "diagnostic_rendering_invokes_writer_without_mutator",
    },
    GateEvidence {
        concern: "worker cache retirement",
        path: "src/evaluation/executor.rs",
        verification: "worker_termination_releases_inactive_collector_caches",
    },
    GateEvidence {
        concern: "production collection policy",
        path: "src/core.rs",
        verification: "scoped_factories_share_one_no_auto_runtime_value_domain",
    },
];

struct StableFamilyRecord {
    family: &'static str,
    row_prefix: &'static str,
    required: &'static [&'static str],
}

const STABLE_FAMILIES: &[StableFamilyRecord] = &[
    StableFamilyRecord {
        family: "ManagedValueNode",
        row_prefix: "| Production managed core value node; `ManagedValueNode`;",
        required: &[
            "Current x86-64 layout is 64/8",
            "requested extent is 64 bytes",
            "allocator discovery is exercised",
            "Passive drop under I4.0",
        ],
    },
    StableFamilyRecord {
        family: "ManagedLazyCell",
        row_prefix: "| `ManagedLazyCell` / `LazySource`",
        required: &[
            "Current x86-64 layout is 144/8",
            "requested extent is 144 bytes",
            "allocator discovery is exercised",
            "Private `allocate_managed_lazy`",
        ],
    },
    StableFamilyRecord {
        family: "ManagedPromiseCell",
        row_prefix: "| `ManagedPromiseCell` (`core/managed/recursive_cells.rs`)",
        required: &[
            "Current x86-64 layout is 104/8",
            "requested extent is 104 bytes",
            "allocator discovery is exercised",
            "Private `allocate_managed_promise`",
        ],
    },
    StableFamilyRecord {
        family: "ManagedCoreNetCell",
        row_prefix: "| `ManagedCoreNetCell` (introduced in I5)",
        required: &[
            "Current x86-64 layout is 248/8",
            "requested extent is 248 bytes",
            "allocator discovery is exercised",
            "Private `allocate_managed_core_net`",
        ],
    },
];

#[test]
fn gate_g2_source_inventory_is_closed() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let concerns = SOURCE_EVIDENCE
        .iter()
        .map(|entry| entry.concern)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        concerns.len(),
        SOURCE_EVIDENCE.len(),
        "each Gate G2 concern must have one independently named authority"
    );

    for entry in SOURCE_EVIDENCE {
        let source = fs::read_to_string(manifest.join(entry.path))
            .unwrap_or_else(|error| panic!("{} should be readable: {error}", entry.path));
        let declaration = format!("fn {}(", entry.verification);
        assert_eq!(
            source.matches(&declaration).count(),
            1,
            "Gate G2 concern {:?} lost its authoritative verification {} at {}",
            entry.concern,
            entry.verification,
            entry.path
        );
    }

    let review =
        fs::read_to_string(manifest.join("docs/reviews/GarbageCollectorGateG2_2026-09-11.md"))
            .expect("the dated Gate G2 review should be checked in with certification");
    assert!(review.contains("Status: complete. Gate G2 passes"));
    assert!(review.contains("Production remains `CollectionPolicy::NoAuto`"));
}

#[test]
fn gate_g2_stable_ledger_records_are_complete() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ledger = fs::read_to_string(
        manifest.join("docs/plans/GarbageCollectorOwnershipLedger_2026-08-20.md"),
    )
    .expect("the GC ownership ledger should be readable");

    for record in STABLE_FAMILIES {
        let rows = ledger
            .lines()
            .filter(|line| line.starts_with(record.row_prefix))
            .collect::<Vec<_>>();
        assert_eq!(
            rows.len(),
            1,
            "{} must have one stable Gate G2 ledger row",
            record.family
        );
        for required in record.required {
            assert!(
                rows[0].contains(required),
                "{} ledger row is missing {required:?}",
                record.family
            );
        }
    }

    assert!(ledger.contains("## Gate G2 Reconciliation Record"));
    assert!(ledger.contains("Gate G2 passed on 2026-09-11"));
    assert!(
        !ledger.lines().any(|line| {
            line.starts_with('|') && (line.contains("| **D** |") || line.contains("| D |"))
        }),
        "a durable boundary-defect row still blocks Gate G2"
    );
}
