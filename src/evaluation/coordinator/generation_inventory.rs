//! Source-backed W9C coordinator-generation publication census.
//!
//! A broad generation remains useful to waiters and settlement even after
//! exact routes gain a narrower validation revision. Keep every production
//! advance behind the factual mutation-kind boundary so new coordinator work
//! cannot silently escape profiling and route-effect review.

const SOURCES: &[(&str, &str)] = &[
    ("coordinator.rs", include_str!("../coordinator.rs")),
    ("client_demand.rs", include_str!("client_demand.rs")),
    ("completion.rs", include_str!("completion.rs")),
    ("deferred.rs", include_str!("deferred.rs")),
    ("reflection.rs", include_str!("reflection.rs")),
    ("settlement.rs", include_str!("settlement.rs")),
    ("spark.rs", include_str!("spark.rs")),
    ("task.rs", include_str!("task.rs")),
];

#[test]
fn coordinator_generation_advances_have_one_authoritative_boundary() {
    let direct = SOURCES
        .iter()
        .flat_map(|(name, source)| {
            source.lines().enumerate().filter_map(move |(index, line)| {
                line.contains(".work_generation = ")
                    .then_some((*name, index + 1, line.trim()))
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(
        direct.len(),
        1,
        "production coordinator revisions must advance only through WorkCoordinatorState::advance_work_generation",
    );
    assert_eq!(direct[0].0, "coordinator.rs");
    assert_eq!(
        direct[0].2,
        "self.work_generation = self.work_generation.wrapping_add(1);"
    );
}

#[test]
fn coordinator_mutation_kind_vocabulary_is_exact() {
    let coordinator = syn::parse_file(SOURCES[0].1).expect("coordinator source must parse");
    let mutation_kind = coordinator
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == "CoordinatorMutationKind" => Some(item),
            _ => None,
        })
        .expect("CoordinatorMutationKind must remain explicit");
    let variants = mutation_kind
        .variants
        .iter()
        .map(|variant| variant.ident.to_string())
        .collect::<Vec<_>>();

    assert_eq!(
        variants,
        [
            "DemandSessionRegistry",
            "ExecutorAvailability",
            "FreshWorkAdmission",
            "WorkActivation",
            "ClientDemandAdmission",
            "TaskPromiseIndexAdmission",
            "TaskPromiseIndexRetirement",
            "WorkClaim",
            "WorkRequeue",
            "WorkRelease",
            "ClientDemandRelease",
            "DependencyPromotion",
            "DependencyWake",
            "ObservationWake",
            "WorkPark",
            "Cancellation",
            "SessionClosure",
            "TerminalSettlement",
            "WorkRetirement",
            "FailureLedger",
            "StageSettlement",
            "TestTransition",
        ],
    );
}
