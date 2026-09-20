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
        [5, 8],
        "W6G.1f.3d.1-.3 regional key/list bridge plus edge-owned access state and boundary adapter; W6G.1f.3g.1b exposes required and optional key-list conversion through that shared regional child"
    ),
    context_entry!(
        "src/eval/annotation_machine.rs",
        [9, 3],
        "W6E.1-W6E.4 durable annotation recognition, collection, metadata, and reflection owner"
    ),
    context_entry!(
        "src/eval/application.rs",
        [0, 3],
        "W6F.4d.3 ordinary lazy application test constructors and access-qualified leaves"
    ),
    context_entry!(
        "src/eval/builtins.rs",
        [0, 1],
        "W6C.1b regional dispatcher and test-only durable compatibility wrapper"
    ),
    context_entry!(
        "src/eval/effect_machine.rs",
        [7, 2],
        "W6E.5-W6E.6 durable effect dispatch, map traversal, and fixpoint construction"
    ),
    context_entry!(
        "src/eval/builtins/net/construction.rs",
        [4, 1],
        "I3D.4/W6F.6-W6F.7 scoped result decoding and replay with one durable exposed-port WHNF owner"
    ),
    context_entry!(
        "src/eval/builtin_machine.rs",
        [1, 1],
        "W6F.5 retains the builtin compatibility owner; W6G.1f.3g.2a-.2d move numeric, assertion, provenance, conditional, net, seq, and spark state beneath caller-supplied regional access"
    ),
    context_entry!(
        "src/eval/object_builtin_machine.rs",
        [12, 2],
        "W6F.2/W6F.4a-W6F.4c durable object specification, diagnostic normalization, local-name, instance, definition-adapter, and plain-dictionary conversion ownership"
    ),
    context_entry!(
        "src/eval/object_composition_machine.rs",
        [12, 3],
        "W6F.3 durable ordinary extension, composed-definition application, and recursive override ownership"
    ),
    context_entry!(
        "src/eval/list_machine.rs",
        [2, 6],
        "W6G.1f.3e.1 and W6G.1f.3g.1a expose regional logical-list front/back bridges while retaining the compatibility owners until parent-family cutover"
    ),
    context_entry!(
        "src/eval/net.rs",
        [22, 7],
        "I3D.3d-I3D.4 scoped batches and claims; I8A.0 normalization roots; W4C.1 persistent driver and net-WHNF owner; NC1 shared net-WHNF budget driver; NC3-NC5 regional callable spill, resumption, and cold exact terminalization; NC6 retired the synchronous deferred-callable context; W6B.4b.1 retired synchronous access resolution; W6G.1f.3c drives managed net checkpoints beneath caller-supplied access"
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
        [1, 1],
        "W6G.1f.3g.2d leaves only the detached spark worker's durable demand owner; seq and spark admission are regional builtin state"
    ),
    context_entry!(
        "src/eval/tagged_machine.rs",
        [1, 3],
        "W6G.1f.3g.3b regional tagged-payload owner with one legacy semantic-undefined owner pending pattern cutover"
    ),
    context_entry!(
        "src/eval/value.rs",
        [21, 8],
        "I3B.2/I3C.2 scoped wait and I4F.1c.2 failure-root projection; I3D reflection/net; I3E.1 deferred producers; GCI5R-003D explicit lazy/promise observation; GCI5R-008 root-only retry projection; W2A.2 exact lazy-root admission; W2B.2 removes the follower's recursive halt adapter; W3B.2 removes the direct fixpoint helper; W6G.1f.2a installs and polls lazy-owned WHNF checkpoints; W6G.1f.3a.1 bounds host-call checkpoint projection and rooted-outcome publication on either side of the mutator-free callback; W6G.1f.3b removes the route-owned reflection evaluator context; W6G.1f.3c installs and transitions managed net checkpoints in bounded access; W6G.1f.3d.2-.3 does the same for computed access; W6G.1f.3e.3 and W6G.1f.3f directly install and poll object/list-effect checkpoints; W6G.1f.3g.2a polls the numeric checkpoint under bounded access"
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
        source.contains("NetConstructionState::Exposed { journal, demand }")
            && source.contains("demand: WhnfComputation::from_root")
            && source.contains("construction_port_value(&access, &value, &self.brand)"),
        "the completed construction result must retain an explicit durable WHNF owner and inspect its port only under evaluator access"
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
        dispatcher.contains("access: &EvaluationValueAccess<'_>")
            && dispatcher.contains("LazyValue::from_builtin_in(\n            access.values()")
            && !dispatcher.contains("context.construct_lazy"),
        "saturated builtin dispatch must use only the caller's bounded regional access"
    );

    let source = fs::read_to_string(manifest.join("src/eval/value.rs"))
        .expect("lazy-source owner should be readable");
    assert!(
        source.contains("apply_builtin_in(&access, call.builtin, arguments, argument)")
            && source.contains("|value| access.values().root_runtime_value(value)"),
        "the lazy-source owner must publish an immediate builtin result before regional access closes"
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
