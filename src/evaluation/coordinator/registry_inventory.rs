//! Source-backed W6G.1 registry-boundary audit.
//!
//! Completion registrations retain one runtime-global work ID, so every path
//! which resolves such an ID must deliberately account for the foreground and
//! background registries. These checks make the separation fail closed while
//! the coordinator is reorganized further.

const COORDINATOR: &str = include_str!("../coordinator.rs");
const CLIENT: &str = include_str!("client_demand.rs");
const SETTLEMENT: &str = include_str!("settlement.rs");

#[test]
fn foreground_records_are_not_a_background_work_kind() {
    let file = syn::parse_file(COORDINATOR).expect("coordinator source must parse");
    let work_kind = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == "WorkKind" => Some(item),
            _ => None,
        })
        .expect("WorkKind must remain explicit");
    let variants = work_kind
        .variants
        .iter()
        .map(|variant| variant.ident.to_string())
        .collect::<Vec<_>>();
    assert_eq!(variants, ["Spark", "Reflection", "Deferred"]);
    assert!(COORDINATOR.contains("client_demands: HashMap<EvaluationWorkId, ClientDemandRecord>"));
    assert!(COORDINATOR.contains(
        "client_demands_by_session: HashMap<EvaluationSessionId, HashSet<EvaluationWorkId>>"
    ));
}

#[test]
fn cross_registry_lifecycle_paths_account_for_foreground_records() {
    assert!(
        COORDINATOR
            .contains("if let Some(record) = state.client_demands.get_mut(&registration.work)")
    );
    assert!(COORDINATOR.contains(".client_demands_by_session\n                .get(&session)"));
    assert!(CLIENT.contains("state.client_demands.insert(id, record)"));
    assert!(CLIENT.contains("state\n        .client_demands\n        .remove(&id)"));
    assert!(SETTLEMENT.contains("for record in state.client_demands.values()"));
    assert!(SETTLEMENT.contains("if state.client_demands.contains_key(&proposed.work)"));
}

#[test]
fn foreground_ready_state_has_no_executor_selector() {
    assert!(CLIENT.contains("fn claim_client_demand("));
    assert!(CLIENT.contains("fn claim_ready_client_demand("));
    assert!(CLIENT.contains("#[cfg(test)]\nfn claim_ready_client_demand("));
    assert!(!COORDINATOR.contains("WorkKind::ClientDemand"));
}
