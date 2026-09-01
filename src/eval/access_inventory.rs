//! I3B.1 inventory of managed-access evaluator surfaces and direct production
//! entries into recursive evaluation.
//!
//! These compatibility calls do not yet inspect managed semantic pointers,
//! and production collection remains `NoAuto`. The inventory prevents a new
//! authority-free entry from appearing while I3B-I3E replace each listed
//! caller with a scheduler- or runtime-service-owned evaluator-step context.
//! The context-surface inventory separately accounts for every scoped
//! evaluator function and every durable I3B.2/I3D/I3E seam below `src/eval`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EntryCounts {
    eval_value: usize,
    apply_values: usize,
    demand_strategy_value: usize,
    eval_key_path_list: usize,
    list_to_value_items: usize,
}

impl EntryCounts {
    const fn new(counts: [usize; 5]) -> Self {
        Self {
            eval_value: counts[0],
            apply_values: counts[1],
            demand_strategy_value: counts[2],
            eval_key_path_list: counts[3],
            list_to_value_items: counts[4],
        }
    }

    fn in_source(source: &str) -> Self {
        Self {
            eval_value: source.matches("eval::eval_value(").count(),
            apply_values: source.matches("eval::apply_values(").count(),
            demand_strategy_value: source.matches("eval::demand_strategy_value(").count(),
            eval_key_path_list: source.matches("eval::eval_key_path_list(").count(),
            list_to_value_items: source.matches("eval::list_to_value_items(").count(),
        }
    }

    fn is_empty(self) -> bool {
        self == Self::new([0; 5])
    }
}

struct InventoryEntry {
    path: &'static str,
    counts: EntryCounts,
    owner: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ContextCounts {
    scoped: usize,
    durable: usize,
}

impl ContextCounts {
    const fn new(scoped: usize, durable: usize) -> Self {
        Self { scoped, durable }
    }

    fn in_source(source: &str) -> Self {
        Self {
            scoped: source.matches("context: &EvaluatorStepContext").count(),
            durable: source.matches("context: &EvalContext").count(),
        }
    }

    fn is_empty(self) -> bool {
        self == Self::new(0, 0)
    }
}

struct ContextInventoryEntry {
    path: &'static str,
    counts: ContextCounts,
    owner: &'static str,
}

macro_rules! context_entry {
    ($path:literal, [$scoped:literal, $durable:literal], $owner:literal) => {
        ContextInventoryEntry {
            path: $path,
            counts: ContextCounts::new($scoped, $durable),
            owner: $owner,
        }
    };
}

macro_rules! entry {
    ($path:literal, $counts:expr, $owner:literal) => {
        InventoryEntry {
            path: $path,
            counts: EntryCounts::new($counts),
            owner: $owner,
        }
    };
}

const INVENTORY: &[InventoryEntry] = &[
    entry!("src/api/assembly.rs", [1, 0, 0, 0, 0], "I3E.1"),
    entry!("src/compiler.rs", [3, 0, 0, 0, 0], "I3E.1"),
    entry!("src/diagnostic.rs", [10, 2, 0, 0, 0], "I3E.3"),
    entry!("src/g_syntax/compiler_values.rs", [1, 0, 0, 0, 0], "I3E.2"),
    entry!(
        "src/g_syntax/diagnostic_formatter.rs",
        [1, 0, 0, 0, 0],
        "I3E.2"
    ),
    entry!(
        "src/g_syntax/macro_expansion/runner.rs",
        [1, 0, 0, 0, 0],
        "I3E.2"
    ),
    entry!("src/g_syntax/parser/source.rs", [1, 0, 0, 0, 0], "I3E.2"),
];

/// Every evaluator function which already carries scoped access, plus every
/// deliberate durable-context seam which later checkpoints must split.
///
/// Counts are per source owner rather than one aggregate: adding, removing, or
/// moving an evaluator entry requires naming the receiving migration phase.
/// The test-only `apply_builtin` compatibility wrapper remains visible in
/// `builtins.rs`; it does not create another production admission gate.
const CONTEXT_INVENTORY: &[ContextInventoryEntry] = &[
    context_entry!(
        "src/eval/application.rs",
        [4, 3],
        "I3B.2 and I3D/I3E direct callers"
    ),
    context_entry!(
        "src/eval/builtins.rs",
        [1, 1],
        "I3D/I3E dispatcher and test compatibility"
    ),
    context_entry!(
        "src/eval/builtins/annotation.rs",
        [1, 0],
        "I3B.1 scoped annotation dispatch"
    ),
    context_entry!(
        "src/eval/builtins/annotation/implementation.rs",
        [13, 2],
        "I3B.1 pure annotations; I3D.1/I3D.2 reflection and strategy seams"
    ),
    context_entry!(
        "src/eval/builtins/assertion.rs",
        [2, 0],
        "I3B.1 scoped assertions"
    ),
    context_entry!(
        "src/eval/builtins/comparison.rs",
        [1, 0],
        "I3B.1 scoped comparison dispatch"
    ),
    context_entry!(
        "src/eval/builtins/comparison/implementation.rs",
        [7, 0],
        "I3B.1 scoped recursive comparisons"
    ),
    context_entry!(
        "src/eval/builtins/conditional.rs",
        [1, 0],
        "I3B.1 scoped conditionals"
    ),
    context_entry!(
        "src/eval/builtins/dict.rs",
        [1, 0],
        "I3B.1 scoped dictionary dispatch"
    ),
    context_entry!(
        "src/eval/builtins/dict/basic.rs",
        [3, 0],
        "I3B.1 scoped dictionary operations"
    ),
    context_entry!(
        "src/eval/builtins/dict/merge.rs",
        [1, 0],
        "I3B.1 scoped dictionary merge"
    ),
    context_entry!(
        "src/eval/builtins/effect.rs",
        [0, 1],
        "I3D.1/I3D.2 effect boundary"
    ),
    context_entry!(
        "src/eval/builtins/effect/implementation.rs",
        [0, 3],
        "I3D.1/I3D.2 effect control and reflection gates"
    ),
    context_entry!(
        "src/eval/builtins/list.rs",
        [1, 0],
        "I3B.1 scoped list dispatch"
    ),
    context_entry!(
        "src/eval/builtins/list/implementation.rs",
        [11, 0],
        "I3B.1 scoped list operations"
    ),
    context_entry!(
        "src/eval/builtins/list_effect.rs",
        [1, 0],
        "I3B.1 scoped list-effect construction"
    ),
    context_entry!(
        "src/eval/builtins/list_effect/implementation.rs",
        [5, 1],
        "I3B.1 list-effect construction; I3E.1 semantic thunk"
    ),
    context_entry!(
        "src/eval/builtins/net.rs",
        [2, 0],
        "I3D.4 scoped interaction-net builtin dispatch"
    ),
    context_entry!(
        "src/eval/builtins/net/construction.rs",
        [2, 0],
        "I3D.4 scoped result decoding; isolated-search construction takes owned durable context"
    ),
    context_entry!(
        "src/eval/builtins/numeric.rs",
        [1, 0],
        "I3B.1 scoped numeric dispatch"
    ),
    context_entry!(
        "src/eval/builtins/numeric/implementation.rs",
        [4, 0],
        "I3B.1 scoped numeric operations"
    ),
    context_entry!(
        "src/eval/builtins/object.rs",
        [1, 0],
        "I3B.1 scoped object dispatch"
    ),
    context_entry!(
        "src/eval/builtins/object/implementation.rs",
        [16, 0],
        "I3B.1 scoped object construction and linearization"
    ),
    context_entry!(
        "src/eval/builtins/pattern.rs",
        [17, 0],
        "I3B.1 scoped pattern inspection"
    ),
    context_entry!(
        "src/eval/builtins/provenance.rs",
        [0, 1],
        "I3E.3/I10 opaque origin inspection"
    ),
    context_entry!(
        "src/eval/builtins/strategy.rs",
        [1, 4],
        "I3C scoped strategy demand and durable scheduling boundary"
    ),
    context_entry!(
        "src/eval/net.rs",
        [14, 7],
        "I3D.3d-I3D.4 scoped batches, claims, access, and one-shot contention handoff; test and non-net durable helpers"
    ),
    context_entry!(
        "src/eval/operator.rs",
        [2, 0],
        "I3D.4 scoped core-net operator application"
    ),
    context_entry!(
        "src/eval/sequence.rs",
        [4, 3],
        "I3B.2 and I3D/I3E direct sequence callers"
    ),
    context_entry!(
        "src/eval/value.rs",
        [19, 10],
        "I3B.2/I3C.2 scoped wait projection; I3D reflection/net; I3E.1 deferred producers"
    ),
];

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

fn is_external_production_source(relative: &Path) -> bool {
    !relative.starts_with("src/eval")
        && !relative.starts_with("src/bin")
        && !relative
            .components()
            .any(|component| component.as_os_str() == "tests")
        && relative.file_name().is_none_or(|name| name != "tests.rs")
}

fn is_evaluator_surface_source(relative: &Path) -> bool {
    relative.starts_with("src/eval")
        && !matches!(
            relative.to_str(),
            Some("src/eval/access_inventory.rs" | "src/eval/test_support.rs" | "src/eval/tests.rs")
        )
}

#[test]
fn evaluator_context_surfaces_are_complete() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src/eval"), &mut sources);

    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(manifest)
                .expect("evaluator source should belong to this package");
            if !is_evaluator_surface_source(relative) {
                return None;
            }
            let source = fs::read_to_string(&path).expect("Rust source should be readable");
            let counts = ContextCounts::in_source(&source);
            (!counts.is_empty()).then(|| (relative.to_path_buf(), counts))
        })
        .collect::<BTreeMap<_, _>>();
    let expected = CONTEXT_INVENTORY
        .iter()
        .map(|entry| {
            assert!(
                !entry.owner.is_empty(),
                "every evaluator context surface needs a migration owner"
            );
            (PathBuf::from(entry.path), entry.counts)
        })
        .collect::<BTreeMap<_, _>>();

    assert_eq!(actual, expected);
}

#[test]
fn direct_evaluator_compatibility_entries_are_complete() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);

    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(manifest)
                .expect("source path should be below the manifest");
            if !is_external_production_source(relative) {
                return None;
            }
            let source = fs::read_to_string(&path).expect("Rust source should be readable");
            let counts = EntryCounts::in_source(&source);
            (!counts.is_empty()).then(|| (relative.to_path_buf(), counts))
        })
        .collect::<BTreeMap<_, _>>();
    let expected = INVENTORY
        .iter()
        .map(|entry| {
            assert!(
                !entry.owner.is_empty(),
                "every entry needs a migration owner"
            );
            (PathBuf::from(entry.path), entry.counts)
        })
        .collect::<BTreeMap<_, _>>();

    assert_eq!(actual, expected);
}

#[test]
fn direct_evaluator_admission_has_one_internal_compatibility_gate() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(manifest.join("src/eval.rs"))
        .expect("evaluator module source should be readable");
    assert_eq!(
        source
            .matches("EvaluatorStepContext::for_direct_compatibility(")
            .count(),
        1,
        "direct evaluation must remain centralized until I3D/I3E remove it"
    );
}

#[test]
fn effect_interpreter_sources_have_no_direct_compatibility_entry() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/reflection/machine.rs",
        "src/reflection/protocol.rs",
        "src/reflection/requests.rs",
        "src/g_syntax/macro_expansion/effects.rs",
    ] {
        let source = fs::read_to_string(manifest.join(relative))
            .expect("effect interpreter source should be readable");
        assert!(
            !source.contains("EvaluatorStepContext::for_direct_compatibility("),
            "{relative} must re-enter evaluation only through its admitted poll context"
        );
        assert!(
            EntryCounts::in_source(&source).is_empty(),
            "{relative} must not call the durable-context evaluator compatibility API"
        );
    }
}

#[test]
fn net_construction_callbacks_have_no_direct_compatibility_entry() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let relative = "src/eval/builtins/net/construction.rs";
    let source = fs::read_to_string(manifest.join(relative))
        .expect("net-construction source should be readable");

    for forbidden in [
        "eval_value(",
        "eval_index_number(",
        "with_direct_evaluator(",
    ] {
        assert!(
            !source.contains(forbidden),
            "{relative} must demand callback arguments through RequestContext, not `{forbidden}`"
        );
    }
    assert!(
        source.contains("context.evaluate(value)"),
        "construction callbacks must use RequestContext's bounded evaluator service"
    );
    assert!(
        source.contains("fn construction_port_in(\n    context: &EvaluatorStepContext"),
        "the completed construction result must retain its owning evaluator-step authority"
    );
}

#[test]
fn builtin_durable_context_downgrades_are_explicit_and_complete() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dispatcher = fs::read_to_string(manifest.join("src/eval/builtins.rs"))
        .expect("builtin dispatcher source should be readable");

    assert_eq!(
        dispatcher.matches("context.context()").count(),
        3,
        "only effects, strategies, and provenance may downgrade in the dispatcher"
    );
    for durable_call in [
        "effect::apply(context.context(), builtin, arguments)",
        "strategy::apply(context.context(), builtin, arguments)",
        "provenance::apply(context.context(), arguments)",
    ] {
        assert!(
            dispatcher.contains(durable_call),
            "missing durable builtin boundary `{durable_call}`"
        );
    }
    assert!(
        dispatcher.contains("Builtin::Anno => annotation::apply(context, arguments)"),
        "annotation dispatch must retain evaluator-step authority"
    );
    assert!(
        dispatcher.contains(
            "Builtin::InteractionNet | Builtin::NetArity => net::apply(context, builtin, arguments)"
        ),
        "interaction-net dispatch must retain evaluator-step authority"
    );

    let annotation =
        fs::read_to_string(manifest.join("src/eval/builtins/annotation/implementation.rs"))
            .expect("annotation implementation source should be readable");
    for durable_annotation_seam in [
        "fn defer_reflection_annotation(context: &EvalContext",
        "fn defer_metadata_reflection(context: &EvalContext",
        "strategy::seq(context.context(), &value, target)",
        "strategy::spark(\n            context.context(),",
    ] {
        assert!(
            annotation.contains(durable_annotation_seam),
            "missing durable annotation seam `{durable_annotation_seam}`"
        );
    }
}
