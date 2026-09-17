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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LazyProducerCounts {
    semantic_thunk: usize,
    semantic_computation: usize,
    external_host_call: usize,
}

impl LazyProducerCounts {
    const fn new(
        semantic_thunk: usize,
        semantic_computation: usize,
        external_host_call: usize,
    ) -> Self {
        Self {
            semantic_thunk,
            semantic_computation,
            external_host_call,
        }
    }

    fn in_source(source: &str) -> Self {
        Self {
            semantic_thunk: source.matches("::semantic_thunk(").count(),
            semantic_computation: source.matches("::semantic_computation(").count()
                + source.matches("::semantic_computation_in(").count(),
            external_host_call: source.matches("::external_host_call(").count()
                + source.matches("::external_host_call_in(").count(),
        }
    }

    fn is_empty(self) -> bool {
        self == Self::new(0, 0, 0)
    }
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

/// Every evaluator function which already carries scoped access, plus every
/// deliberate durable-context seam which later checkpoints must split.
///
/// Counts are per source owner rather than one aggregate: adding, removing, or
/// moving an evaluator entry requires naming the receiving migration phase.
/// The test-only `apply_builtin` compatibility wrapper remains visible in
/// `builtins.rs`; it does not create another production admission gate.
const CONTEXT_INVENTORY: &[ContextInventoryEntry] = &[
    context_entry!(
        "src/eval/access_machine.rs",
        [9, 6],
        "W3C.2-W3C.3 scoped projection and durable access/key/list source owner"
    ),
    context_entry!(
        "src/eval/annotation_machine.rs",
        [9, 3],
        "W6E.1-W6E.4 durable annotation recognition, collection, metadata, and reflection owner"
    ),
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
        "src/eval/effect_machine.rs",
        [7, 2],
        "W6E.5-W6E.6 durable effect dispatch, map traversal, and fixpoint construction"
    ),
    context_entry!(
        "src/eval/builtins/net.rs",
        [2, 0],
        "I3D.4 scoped interaction-net builtin dispatch"
    ),
    context_entry!(
        "src/eval/builtins/net/construction.rs",
        [3, 0],
        "I3D.4 scoped result decoding; isolated-search construction takes owned durable context"
    ),
    context_entry!(
        "src/eval/builtin_machine.rs",
        [7, 7],
        "W6C.2-W6C.6 durable conditional, assertion, numeric, provenance, and strategy owners with callback-free regional result projection"
    ),
    context_entry!(
        "src/eval/comparison_machine.rs",
        [9, 6],
        "W6C.3 durable recursive comparison owner with scoped classification and result projection"
    ),
    context_entry!(
        "src/eval/dict_machine.rs",
        [5, 3],
        "W6D.1-W6D.2 durable dictionary operands with access-qualified merge and update leaves"
    ),
    context_entry!(
        "src/eval/builtins/object.rs",
        [1, 0],
        "I3B.1 scoped object dispatch"
    ),
    context_entry!(
        "src/eval/builtins/object/implementation.rs",
        [1, 0],
        "I3B.1 remaining scoped object builtins; W3B.2b pollable source construction"
    ),
    context_entry!(
        "src/eval/object_builtin_machine.rs",
        [10, 2],
        "W6F.2/W6F.4a durable object specification, diagnostic normalization, local-name, and instance ownership"
    ),
    context_entry!(
        "src/eval/object_composition_machine.rs",
        [12, 3],
        "W6F.3 durable ordinary extension, composed-definition application, and recursive override ownership"
    ),
    context_entry!(
        "src/eval/list_effect_machine.rs",
        [9, 1],
        "W3D pollable list-effect recipe owner"
    ),
    context_entry!(
        "src/eval/list_machine.rs",
        [4, 2],
        "W3B.2b/W6D.3 shared resumable logical-list front and back owners"
    ),
    context_entry!(
        "src/eval/list_observation_machine.rs",
        [13, 3],
        "W6D.3 durable list-observation owner with scoped result publication"
    ),
    context_entry!(
        "src/eval/list_transform_machine.rs",
        [6, 3],
        "W6D.4 durable list-transform owners and resumable text extraction"
    ),
    context_entry!(
        "src/eval/net.rs",
        [21, 7],
        "I3D.3d-I3D.4 scoped batches and claims; I8A.0 normalization roots; W4C.1 persistent driver and net-WHNF owner; NC1 shared net-WHNF budget driver; NC3-NC5 regional callable spill, resumption, and cold exact terminalization; NC6 retired the synchronous deferred-callable context; W6B.4b.1 retired synchronous access resolution"
    ),
    context_entry!(
        "src/eval/object_machine.rs",
        [14, 3],
        "W3B.2b explicit C3, composed-definition, and object-mixin source owner"
    ),
    context_entry!(
        "src/eval/pattern_machine.rs",
        [12, 5],
        "W6D.5 resumable compiler-pattern observations"
    ),
    context_entry!(
        "src/eval/sequence.rs",
        [2, 3],
        "I3B.2 and I3D/I3E direct sequence callers"
    ),
    context_entry!(
        "src/eval/strategy_machine.rs",
        [2, 1],
        "W6C.6 shared resumable seq and best-effort spark demand"
    ),
    context_entry!(
        "src/eval/tagged_machine.rs",
        [2, 3],
        "W6C.3 shared tagged-payload and semantic-undefined owner"
    ),
    context_entry!(
        "src/eval/value.rs",
        [15, 9],
        "I3B.2/I3C.2 scoped wait and I4F.1c.2 failure-root projection; I3D reflection/net; I3E.1 deferred producers; GCI5R-003D explicit lazy/promise observation; GCI5R-008 root-only retry projection; W2A.2 exact lazy-root admission; W2B.2 removes the follower's recursive halt adapter; W3B.2 removes the direct fixpoint helper"
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
        && relative != Path::new("src/g_syntax/access_inventory.rs")
        && !relative
            .components()
            .any(|component| component.as_os_str() == "tests")
        && relative.file_name().is_none_or(|name| name != "tests.rs")
}

fn is_evaluator_surface_source(relative: &Path) -> bool {
    relative.starts_with("src/eval")
        && !relative
            .components()
            .any(|component| component.as_os_str() == "tests")
        && !matches!(
            relative.to_str(),
            Some("src/eval/access_inventory.rs" | "src/eval/test_support.rs" | "src/eval/tests.rs")
        )
}

fn is_lazy_producer_inventory_source(relative: &Path) -> bool {
    relative.starts_with("src")
        && relative != Path::new("src/core.rs")
        && relative != Path::new("src/core/managed/active_owner_inventory.rs")
        && relative != Path::new("src/core/managed/containment_inventory.rs")
        && relative != Path::new("src/eval/access_inventory.rs")
        && !relative
            .components()
            .any(|component| component.as_os_str() == "tests")
        && relative.file_name().is_none_or(|name| name != "tests.rs")
}

fn production_prefix(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]\nmod tests")
        .map_or(source, |(production, _)| production)
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
    let expected = BTreeMap::<PathBuf, EntryCounts>::new();

    assert_eq!(actual, expected);
}

#[test]
fn lazy_producer_roles_are_explicit_and_complete() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);

    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(manifest)
                .expect("source path should be below the manifest");
            if !is_lazy_producer_inventory_source(relative) {
                return None;
            }
            let source = fs::read_to_string(&path).expect("Rust source should be readable");
            assert!(
                !source.contains("::deferred("),
                "{relative:?} must classify lazy producers as semantic thunks or host calls"
            );
            let counts = LazyProducerCounts::in_source(production_prefix(&source));
            (!counts.is_empty()).then(|| (relative.to_path_buf(), counts))
        })
        .collect::<BTreeMap<_, _>>();
    let expected = [(
        PathBuf::from("src/compiler.rs"),
        LazyProducerCounts::new(0, 0, 2),
    )]
    .into_iter()
    .collect::<BTreeMap<_, _>>();

    assert_eq!(actual, expected);
}

#[test]
fn direct_evaluator_admission_has_one_internal_compatibility_gate() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);
    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(manifest)
                .expect("source path should be below the manifest");
            if relative == Path::new("src/eval/access_inventory.rs") {
                return None;
            }
            let source = fs::read_to_string(&path).expect("Rust source should be readable");
            let count = source
                .matches("EvaluatorStepContext::for_direct_compatibility(")
                .count();
            (count != 0).then(|| (relative.to_path_buf(), count))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual,
        [(PathBuf::from("src/eval.rs"), 1)].into_iter().collect(),
        "direct evaluation must retain one centralized internal admission"
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
        "src/eval/builtins/net/construction.rs",
        "src/bin/glam/configuration/logger/effects.rs",
        "src/bin/glam/command_line/configured/effects.rs",
        "src/bin/glam/command_line/configured/token/effects.rs",
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
fn specialization_callbacks_have_no_nested_semantic_evaluator() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/reflection/requests.rs",
        "src/g_syntax/macro_expansion/effects.rs",
        "src/eval/builtins/net/construction.rs",
        "src/bin/glam/configuration/logger/effects.rs",
        "src/bin/glam/command_line/configured/effects.rs",
        "src/bin/glam/command_line/configured/token/effects.rs",
    ] {
        let source = fs::read_to_string(manifest.join(relative))
            .expect("specialization source should be readable");
        for forbidden in [
            "context.evaluate(",
            ".evaluator().eval(",
            "eval::eval_value(",
            "eval::eval_value_in(",
            "evaluate_root_whnf(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{relative} must transfer semantic demand to owned request work, not `{forbidden}`"
            );
        }
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
            "{relative} must demand callback arguments through owned request work, not `{forbidden}`"
        );
    }
    assert!(
        !source.contains("context.evaluate("),
        "construction callbacks must own semantic demand as resumable request work"
    );
    assert!(
        source.contains("SpecializationRequestPoll::Demand(outputs)")
            && source.contains("Self::WireRight { left }"),
        "copy and ordered wire preparation must remain explicit request-work phases"
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
        0,
        "builtin dispatch must not downgrade evaluator-step authority"
    );
    assert!(
        dispatcher.contains("| Builtin::Seq\n        | Builtin::Spark => Ok(Value::Lazy("),
        "strategy builtins must install durable lazy owners"
    );
    assert!(
        dispatcher.contains("Builtin::Anno => Ok(Value::Lazy("),
        "annotation dispatch must install its durable lazy owner"
    );
    assert!(
        dispatcher.contains(
            "Builtin::InteractionNet | Builtin::NetArity => net::apply(context, builtin, arguments)"
        ),
        "interaction-net dispatch must retain evaluator-step authority"
    );

    let annotation = fs::read_to_string(manifest.join("src/eval/annotation_machine.rs"))
        .expect("annotation machine source should be readable");
    for annotation_boundary in [
        "pub(crate) struct AnnotationBuiltinMachine",
        "AnnotationPhase::MetadataItems",
        "LazyValue::from_reflection_gate_in",
        "Value::reflection_task_result_in",
        "RecognizedAnnotation::Seq(value)",
        "RecognizedAnnotation::Spark(value)",
    ] {
        assert!(
            annotation.contains(annotation_boundary),
            "missing annotation authority boundary `{annotation_boundary}`"
        );
    }
}
