//! PNC1 source latch for the deliberately narrow semantic replay boundary.

#[test]
fn semantic_netlist_replay_has_no_effect_or_scheduler_boundary() {
    let source = include_str!("netlist.rs");
    for forbidden in [
        "crate::reflection",
        "IsolatedEffectSearch",
        "RequestContext",
        "WorkDependency",
        "WhnfComputation",
        "RuntimeValueRoot",
        "ManagedLazyRoot",
        "ManagedPromiseRoot",
        "ManagedCoreNetRoot",
        "PublicValue",
        "FunctionValue",
        "EvalContext",
        "EvaluatorStepContext",
        "apply_value",
        "apply_values",
        ".evaluate(",
        ".poll(",
        ".wait",
        ".root(",
        "root_in(",
        "root_runtime_value",
        "construct_runtime_value_root",
        "construct_rooted_",
    ] {
        assert!(
            !source.contains(forbidden),
            "semantic netlist replay acquired forbidden boundary `{forbidden}`"
        );
    }
}
