//! I4B source-backed inventory for deferred capture and opaque construction.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::core::{
    EvaluationFailure, EvaluationHalt, HostCallRecord, LazySource, LazyValue, OpaquePayloadFamily,
    OpaquePayloadRecord, OpaqueValue, PromisedValue, Value, set_test_promise,
};
use crate::evaluation::EvaluatorStepContext;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ContainmentCounts {
    semantic_thunk: usize,
    semantic_computation: usize,
    external_host_call: usize,
    opaque_constructor: usize,
    opaque_admission: usize,
}

impl ContainmentCounts {
    const fn new(counts: [usize; 5]) -> Self {
        Self {
            semantic_thunk: counts[0],
            semantic_computation: counts[1],
            external_host_call: counts[2],
            opaque_constructor: counts[3],
            opaque_admission: counts[4],
        }
    }

    fn in_source(source: &str) -> Self {
        Self {
            semantic_thunk: source.matches("::semantic_thunk(").count(),
            semantic_computation: source.matches("::semantic_computation(").count()
                + source.matches("::semantic_computation_in(").count(),
            external_host_call: source.matches("::external_host_call(").count()
                + source.matches("::external_host_call_in(").count(),
            opaque_constructor: source.matches("OpaqueValue::new(").count(),
            opaque_admission: source.matches("OpaquePayloadFamily for").count(),
        }
    }

    fn is_empty(self) -> bool {
        self == Self::new([0; 5])
    }
}

struct InventoryEntry {
    path: &'static str,
    counts: ContainmentCounts,
    owner: &'static str,
}

const INVENTORY: &[InventoryEntry] = &[
    InventoryEntry {
        path: "src/api/value.rs",
        counts: ContainmentCounts::new([0, 0, 0, 1, 1]),
        owner: "I4B edge-free effect-token identity; external domain lifecycle rechecked in I9/I10",
    },
    InventoryEntry {
        path: "src/compiler.rs",
        counts: ContainmentCounts::new([0, 0, 2, 0, 0]),
        owner: "I4B explicit rooted import arguments; external loader callback finalized in I10A",
    },
    InventoryEntry {
        path: "src/diagnostic.rs",
        counts: ContainmentCounts::new([0, 0, 0, 1, 1]),
        owner: "I4B non-value compilation provenance",
    },
    InventoryEntry {
        path: "src/eval/builtins/list_effect/implementation.rs",
        counts: ContainmentCounts::new([0, 1, 0, 0, 0]),
        owner: "I4B explicit semantic value captures",
    },
    InventoryEntry {
        path: "src/eval/builtins/net/construction.rs",
        counts: ContainmentCounts::new([0, 0, 0, 1, 1]),
        owner: "I4B edge-free construction-local port token",
    },
    InventoryEntry {
        path: "src/reflection/requests.rs",
        counts: ContainmentCounts::new([0, 0, 0, 1, 1]),
        owner: "I4B external task/query capability; lifecycle rechecked in I9/I10",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExternalCaptureDisposition {
    /// Recursive Glam values are explicit managed state; the erased callback
    /// receives one typed same-runtime root bundle rather than hidden values.
    TraceableDeferredValues,
    /// Concrete fields, rather than an erased closure, name the complete host
    /// and specialization ownership.
    ExplicitHostFields,
    /// The callback is an embedding API owner. Its arbitrary environment is
    /// deliberately conservative and can retain public roots supplied by the
    /// host, but is not reachable from a managed value.
    ExternalFacade,
    /// The callback is bounded to compilation and does not survive as a
    /// managed semantic edge.
    BoundedCompiler,
    /// The callback publishes only a wake/notification after state commit.
    Notification,
}

struct ExternalCallbackEntry {
    path: &'static str,
    needle: &'static str,
    count: usize,
    disposition: ExternalCaptureDisposition,
    owner: &'static str,
}

/// The finite production callback surface after I10A. This records boundary
/// declarations and their sole production constructors, not incidental
/// callback-local iterator closures.
const EXTERNAL_CALLBACK_INVENTORY: &[ExternalCallbackEntry] = &[
    ExternalCallbackEntry {
        path: "src/core.rs",
        needle: "dyn Fn(HostCallRootBundle)",
        count: 1,
        disposition: ExternalCaptureDisposition::TraceableDeferredValues,
        owner: "runtime external-owner registry; semantic captures remain on HostCallProducer",
    },
    ExternalCallbackEntry {
        path: "src/compiler.rs",
        needle: "pub(crate) type ModuleLoader =",
        count: 1,
        disposition: ExternalCaptureDisposition::ExternalFacade,
        owner: "compiler host callback; deferred invocation receives explicit rooted arguments",
    },
    ExternalCallbackEntry {
        path: "src/compiler.rs",
        needle: "pub(crate) type BinaryFileLoader =",
        count: 1,
        disposition: ExternalCaptureDisposition::ExternalFacade,
        owner: "compiler host callback with value-free request provenance",
    },
    ExternalCallbackEntry {
        path: "src/compiler.rs",
        needle: "pub(crate) type CompileDiagnosticEmitter =",
        count: 1,
        disposition: ExternalCaptureDisposition::BoundedCompiler,
        owner: "one synchronous source-compilation context",
    },
    ExternalCallbackEntry {
        path: "src/api/runtime.rs",
        needle: "pub fn input_endpoint<T, F>",
        count: 1,
        disposition: ExternalCaptureDisposition::ExternalFacade,
        owner: "host input handle with a weak runtime route",
    },
    ExternalCallbackEntry {
        path: "src/api/runtime.rs",
        needle: "pub fn output_endpoint<T, D, A>",
        count: 1,
        disposition: ExternalCaptureDisposition::ExternalFacade,
        owner: "host output handle with a weak runtime route",
    },
    ExternalCallbackEntry {
        path: "src/api/diagnostics.rs",
        needle: "pub fn subscribe_shared(",
        count: 1,
        disposition: ExternalCaptureDisposition::ExternalFacade,
        owner: "host diagnostic subscription with explicit removal lifecycle",
    },
    ExternalCallbackEntry {
        path: "src/evaluation.rs",
        needle: "launcher: OnceLock<Arc<dyn ReflectionTaskLauncher>>",
        count: 1,
        disposition: ExternalCaptureDisposition::ExplicitHostFields,
        owner: "immutable reflection profile",
    },
    ExternalCallbackEntry {
        path: "src/reflection/lifecycle.rs",
        needle: "struct EffectTaskLauncher<S: TaskSpecialization>",
        count: 1,
        disposition: ExternalCaptureDisposition::ExplicitHostFields,
        owner: "reflection specialization, host, and exit policy are named fields",
    },
    ExternalCallbackEntry {
        path: "src/evaluation/coordinator/task.rs",
        needle: "pub(crate) struct TaskStatusPublisher",
        count: 1,
        disposition: ExternalCaptureDisposition::ExternalFacade,
        owner: "coordinator status observer with registered-root captures",
    },
    ExternalCallbackEntry {
        path: "src/reflection/requests.rs",
        needle: "fn task_status_publisher(",
        count: 1,
        disposition: ExternalCaptureDisposition::ExternalFacade,
        owner: "reflection query writer, handle, and value factory",
    },
    ExternalCallbackEntry {
        path: "src/api/assembly.rs",
        needle: "fn module_loader(",
        count: 1,
        disposition: ExternalCaptureDisposition::ExternalFacade,
        owner: "deferred assembler route plus weak compilation execution",
    },
    ExternalCallbackEntry {
        path: "src/api/assembly.rs",
        needle: "fn binary_loader(&self)",
        count: 1,
        disposition: ExternalCaptureDisposition::ExternalFacade,
        owner: "deferred assembler route",
    },
    ExternalCallbackEntry {
        path: "src/api/assembly.rs",
        needle: "fn compile_diagnostic_emitter(",
        count: 1,
        disposition: ExternalCaptureDisposition::BoundedCompiler,
        owner: "one synchronous source-compilation context",
    },
    ExternalCallbackEntry {
        path: "src/api/assembly.rs",
        needle: "Box<dyn FnOnce() + Send>",
        count: 1,
        disposition: ExternalCaptureDisposition::Notification,
        owner: "post-commit reflection query wake",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpaqueBootstrapDisposition {
    ExternalEdgeFree,
    ExternalCapability,
}

struct OpaqueFamilyInventoryEntry {
    family: &'static str,
    path: &'static str,
    admission: &'static str,
    constructor: &'static str,
    downcast: &'static str,
    disposition: OpaqueBootstrapDisposition,
    retention: &'static str,
}

/// I10B.0's complete production opaque-family surface. Each family has one
/// admitted representation, one production wrapper site, and one typed reader.
const OPAQUE_FAMILY_INVENTORY: &[OpaqueFamilyInventoryEntry] = &[
    OpaqueFamilyInventoryEntry {
        family: "CompilationOrigin",
        path: "src/diagnostic.rs",
        admission: "OpaquePayloadFamily for CompilationOrigin",
        constructor: "OpaqueValue::new(",
        downcast: ".downcast::<CompilationOrigin>",
        disposition: OpaqueBootstrapDisposition::ExternalEdgeFree,
        retention: "immutable source provenance only",
    },
    OpaqueFamilyInventoryEntry {
        family: "ConstructionPort",
        path: "src/eval/builtins/net/construction.rs",
        admission: "OpaquePayloadFamily for ConstructionPort",
        constructor: "OpaqueValue::new(",
        downcast: ".downcast::<ConstructionPort>",
        disposition: OpaqueBootstrapDisposition::ExternalEdgeFree,
        retention: "construction-local brand and scalar port identity only",
    },
    OpaqueFamilyInventoryEntry {
        family: "EffectToken<T>",
        path: "src/api/value.rs",
        admission: "OpaquePayloadFamily for EffectToken<T>",
        constructor: "OpaqueValue::new(",
        downcast: ".downcast::<EffectToken<T>>",
        disposition: OpaqueBootstrapDisposition::ExternalCapability,
        retention: "weak token-domain route; generic payload remains in external domain state",
    },
    OpaqueFamilyInventoryEntry {
        family: "TaskHandleCell",
        path: "src/reflection/requests.rs",
        admission: "OpaquePayloadFamily for TaskHandleCell",
        constructor: "OpaqueValue::new(",
        downcast: ".downcast::<TaskHandleCell>",
        disposition: OpaqueBootstrapDisposition::ExternalCapability,
        retention: "external task/query lifecycle may retain rooted terminal data",
    },
];

struct RuntimeCacheInventoryEntry {
    family: &'static str,
    path: &'static str,
    admission: &'static str,
}

const RUNTIME_CACHE_INVENTORY: &[RuntimeCacheInventoryEntry] = &[
    RuntimeCacheInventoryEntry {
        family: "GCompilerValues",
        path: "src/g_syntax/compiler_values.rs",
        admission: "RuntimeCacheFamily for GCompilerValues",
    },
    RuntimeCacheInventoryEntry {
        family: "CachedDiagnosticFormatter",
        path: "src/g_syntax/diagnostic_formatter.rs",
        admission: "RuntimeCacheFamily for CachedDiagnosticFormatter",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OpaqueSurfaceCounts {
    admissions: usize,
    constructors: usize,
    downcasts: usize,
}

impl OpaqueSurfaceCounts {
    const fn new(admissions: usize, constructors: usize, downcasts: usize) -> Self {
        Self {
            admissions,
            constructors,
            downcasts,
        }
    }

    fn in_source(source: &str) -> Self {
        Self::new(
            source.matches("OpaquePayloadFamily for").count(),
            source.matches("OpaqueValue::new(").count(),
            source.matches(".downcast::<").count(),
        )
    }

    fn is_empty(self) -> bool {
        self == Self::new(0, 0, 0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TypeErasureCounts {
    erased_any: usize,
    borrowed_panic_any: usize,
}

impl TypeErasureCounts {
    const fn new(erased_any: usize, borrowed_panic_any: usize) -> Self {
        Self {
            erased_any,
            borrowed_panic_any,
        }
    }

    fn in_source(source: &str) -> Self {
        Self::new(
            source.matches("dyn Any").count() + source.matches("dyn std::any::Any").count(),
            source.matches("dyn std::any::Any + Send").count(),
        )
    }

    fn is_empty(self) -> bool {
        self == Self::new(0, 0)
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

fn is_production_inventory_source(relative: &Path) -> bool {
    relative != Path::new("src/core.rs")
        && relative != Path::new("src/core/managed.rs")
        && relative != Path::new("src/core/managed/active_owner_inventory.rs")
        && relative != Path::new("src/core/managed/containment_inventory.rs")
        && relative != Path::new("src/eval/access_inventory.rs")
        && !relative
            .components()
            .any(|component| component.as_os_str() == "tests")
        && relative.file_name().is_none_or(|name| name != "tests.rs")
}

/// Drops conventional trailing unit-test modules without trying to parse Rust.
/// Constructor-owning production files in this inventory keep `mod tests` at
/// the end; a new exceptional layout changes the latched counts and requires
/// an explicit inventory decision.
fn production_prefix(source: &str) -> &str {
    source
        .split_once("\n#[cfg(test)]\nmod tests")
        .map_or(source, |(production, _)| production)
}

#[test]
fn closure_and_opaque_constructor_inventory_is_classified() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);

    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(manifest)
                .expect("source path should be below the manifest");
            if !is_production_inventory_source(relative) {
                return None;
            }
            let source = fs::read_to_string(&path).expect("Rust source should be readable");
            let counts = ContainmentCounts::in_source(production_prefix(&source));
            (!counts.is_empty()).then(|| (relative.to_path_buf(), counts))
        })
        .collect::<BTreeMap<_, _>>();
    let expected = INVENTORY
        .iter()
        .map(|entry| {
            assert!(
                !entry.owner.is_empty(),
                "every deferred or opaque constructor needs an explicit owner"
            );
            (PathBuf::from(entry.path), entry.counts)
        })
        .collect::<BTreeMap<_, _>>();

    assert_eq!(actual, expected);
}

#[test]
fn deferred_closure_constructor_inventory_is_reconciled() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for entry in EXTERNAL_CALLBACK_INVENTORY {
        assert!(
            !entry.owner.is_empty(),
            "every callback needs a named owner"
        );
        let source = fs::read_to_string(manifest.join(entry.path))
            .expect("inventoried callback source should be readable");
        assert_eq!(
            production_prefix(&source).matches(entry.needle).count(),
            entry.count,
            "external callback boundary `{}` in {} changed without an I10A classification update",
            entry.needle,
            entry.path,
        );
    }

    assert!(
        EXTERNAL_CALLBACK_INVENTORY.iter().any(|entry| {
            entry.disposition == ExternalCaptureDisposition::TraceableDeferredValues
        })
    );
    assert!(
        EXTERNAL_CALLBACK_INVENTORY
            .iter()
            .any(|entry| entry.disposition == ExternalCaptureDisposition::ExplicitHostFields)
    );
    assert!(
        EXTERNAL_CALLBACK_INVENTORY
            .iter()
            .any(|entry| entry.disposition == ExternalCaptureDisposition::ExternalFacade)
    );
    assert!(
        EXTERNAL_CALLBACK_INVENTORY
            .iter()
            .any(|entry| entry.disposition == ExternalCaptureDisposition::BoundedCompiler)
    );
    assert!(
        EXTERNAL_CALLBACK_INVENTORY
            .iter()
            .any(|entry| entry.disposition == ExternalCaptureDisposition::Notification)
    );
}

#[test]
fn external_callback_constructors_require_capture_classification() {
    let core = include_str!("../../core.rs");
    assert!(core.contains("captures: Arc<[Value]>,"));
    assert!(core.contains("pub(crate) struct HostCallRootBundle"));
    assert!(core.contains("roots: Box<[RuntimeValueRoot]>,"));
    assert!(core.contains("HostCallSemanticCaptures::None"));
    assert!(core.contains("HostCallSemanticCaptures::Explicit"));
    assert!(core.contains("host-call semantic captures must match their source-backed record"));
    assert!(!core.contains("dyn Fn() -> Result<RuntimeValueRoot"));
    assert!(!core.contains(
        "HostCallRootBundle {\n    runtime: EvaluationRuntimeId,\n    roots: Box<[Value]>"
    ));
    assert!(!core.contains(
        "HostCallRootBundle {\n    runtime: EvaluationRuntimeId,\n    roots: Box<[glam_gc::Gc"
    ));

    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let mismatch = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        LazyValue::external_host_call(
            &values,
            "misclassified host-call capture",
            HostCallRecord::external_without_semantic_values(
                "misclassified host-call capture",
                "src/core/managed/containment_inventory.rs",
                "incorrectly claims no semantic captures",
            ),
            [Value::Number(1.into())],
            |_| Err(Arc::new(EvaluationFailure::message("not invoked"))),
        )
    }));
    assert!(
        mismatch.is_err(),
        "a host-call constructor must reject capture state which contradicts its record"
    );
}

fn assert_opaque_family_inventory() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut dispositions = BTreeMap::new();
    for entry in OPAQUE_FAMILY_INVENTORY {
        assert!(!entry.family.is_empty(), "opaque family must be named");
        assert!(
            !entry.retention.is_empty(),
            "{} needs an explicit retention decision",
            entry.family
        );
        let source = fs::read_to_string(manifest.join(entry.path))
            .expect("reviewed opaque-family source should be readable");
        let source = production_prefix(&source);
        for (kind, needle) in [
            ("admission", entry.admission),
            ("constructor", entry.constructor),
            ("downcast", entry.downcast),
        ] {
            assert_eq!(
                source.matches(needle).count(),
                1,
                "opaque family {} has source drift at its {kind}: {needle:?}",
                entry.family
            );
        }
        assert!(
            dispositions
                .insert(entry.family, entry.disposition)
                .is_none(),
            "opaque family {} is reviewed more than once",
            entry.family
        );
    }
    assert_eq!(
        dispositions
            .values()
            .filter(|disposition| **disposition == OpaqueBootstrapDisposition::ExternalEdgeFree)
            .count(),
        2
    );
    assert_eq!(
        dispositions
            .values()
            .filter(|disposition| **disposition == OpaqueBootstrapDisposition::ExternalCapability)
            .count(),
        2
    );

    for entry in RUNTIME_CACHE_INVENTORY {
        assert!(
            !entry.family.is_empty(),
            "runtime cache family must be named"
        );
        let source = fs::read_to_string(manifest.join(entry.path))
            .expect("reviewed runtime-cache source should be readable");
        assert_eq!(
            production_prefix(&source).matches(entry.admission).count(),
            1,
            "runtime cache family {} drifted from the I10B.0 type-erasure review",
            entry.family
        );
    }

    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);
    let actual_opaque = sources
        .iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(manifest)
                .expect("source path should be below the manifest");
            if !is_production_inventory_source(relative) {
                return None;
            }
            let source = fs::read_to_string(path).expect("Rust source should be readable");
            let counts = OpaqueSurfaceCounts::in_source(production_prefix(&source));
            (!counts.is_empty()).then(|| (relative.to_path_buf(), counts))
        })
        .collect::<BTreeMap<_, _>>();
    let expected_opaque = OPAQUE_FAMILY_INVENTORY
        .iter()
        .map(|entry| (PathBuf::from(entry.path), OpaqueSurfaceCounts::new(1, 1, 1)))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual_opaque, expected_opaque,
        "the production opaque family/constructor/downcast surface changed"
    );

    let actual_caches = sources
        .iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(manifest)
                .expect("source path should be below the manifest");
            if !is_production_inventory_source(relative) {
                return None;
            }
            let source = fs::read_to_string(path).expect("Rust source should be readable");
            let count = production_prefix(&source)
                .matches("RuntimeCacheFamily for")
                .count();
            (count != 0).then(|| (relative.to_path_buf(), count))
        })
        .collect::<BTreeMap<_, _>>();
    let expected_caches = RUNTIME_CACHE_INVENTORY
        .iter()
        .map(|entry| (PathBuf::from(entry.path), 1))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual_caches, expected_caches,
        "the production runtime-cache family surface changed"
    );

    // The generic `OpaqueValue` API lives in `core.rs`, which the older I4B
    // call-site inventory excludes to avoid counting its test-only helpers.
    // Keep that declaration owner explicitly closed as part of I10B.1.
    let core = fs::read_to_string(manifest.join("src/core.rs"))
        .expect("the core value declaration source should be readable");
    assert!(
        OpaqueSurfaceCounts::in_source(production_prefix(&core)).is_empty(),
        "core.rs gained an opaque family or call site outside the production inventory"
    );
    assert_eq!(
        production_prefix(&core)
            .matches("RuntimeCacheFamily for")
            .count(),
        0,
        "core.rs gained a production runtime-cache family outside its dedicated modules"
    );

    // `core/managed.rs` owns the admission trait and one deliberately
    // test-only scalar fixture before its conventional test module. A second
    // implementation here would be an unreviewed production-family shortcut.
    let managed = fs::read_to_string(manifest.join("src/core/managed.rs"))
        .expect("the managed admission source should be readable");
    assert_eq!(managed.matches("OpaquePayloadFamily for").count(), 1);
    assert!(
        managed.contains("#[cfg(test)]\n// SAFETY: scalar opaque fixtures contain no managed edge")
    );
}

#[test]
fn opaque_representation_review_inventory_is_complete() {
    assert_opaque_family_inventory();
}

#[test]
fn opaque_family_inventory_is_reconciled() {
    assert_opaque_family_inventory();
}

#[test]
fn opaque_type_erasure_inventory_is_reconciled() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);
    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(manifest)
                .expect("source path should be below the manifest");
            if !is_production_inventory_source(relative) {
                return None;
            }
            let source = fs::read_to_string(&path).expect("Rust source should be readable");
            let counts = TypeErasureCounts::in_source(production_prefix(&source));
            (!counts.is_empty()).then(|| (relative.to_path_buf(), counts))
        })
        .collect::<BTreeMap<_, _>>();
    let expected = [
        (
            PathBuf::from("src/api/runtime/events.rs"),
            TypeErasureCounts::new(1, 1),
        ),
        (
            PathBuf::from("src/core/managed/external_owners.rs"),
            TypeErasureCounts::new(1, 0),
        ),
        (
            PathBuf::from("src/core/runtime_cache.rs"),
            TypeErasureCounts::new(1, 0),
        ),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual, expected,
        "an owned or borrowed type-erasure boundary changed without I10B.0 review"
    );
}

#[test]
fn opaque_representation_plan_has_no_undecided_family() {
    let review =
        include_str!("../../../docs/reviews/GarbageCollectorOpaqueRepresentation_2026-09-11.md");
    let plan = include_str!("../../../docs/plans/GarbageCollectorIntegration_2026-08-19.md");
    assert!(
        review.contains("Phase I10B.0 selects **external-only opaque storage** for the\nbootstrap")
    );
    assert!(
        plan.contains("I10B implements the selected external-only policy. Arbitrary host `Any`")
    );
    assert!(!plan.contains("This phase is deliberately not implementation-ready until I10B.0"));
    assert_eq!(OPAQUE_FAMILY_INVENTORY.len(), 4);
}

#[test]
fn opaque_representation_plan_links_are_consistent() {
    let review =
        include_str!("../../../docs/reviews/GarbageCollectorOpaqueRepresentation_2026-09-11.md");
    let integration = include_str!("../../../docs/plans/GarbageCollectorIntegration_2026-08-19.md");
    let ledger = include_str!("../../../docs/plans/GarbageCollectorOwnershipLedger_2026-08-20.md");
    let roadmap = include_str!("../../../docs/plans/GarbageCollectionRoadmap_2026-08-19.md");

    assert!(review.contains("No managed opaque arm"));
    assert!(integration.contains(
        "[`GarbageCollectorOpaqueRepresentation_2026-09-11.md`](../reviews/GarbageCollectorOpaqueRepresentation_2026-09-11.md)"
    ));
    assert!(integration.contains("Require I10B.0's external-only four-family mapping"));
    assert!(integration.contains("Every opaque value satisfies I10B.0's external-only policy"));
    assert!(ledger.contains("I10B.0 selected external-only storage"));
    assert!(roadmap.contains("Opaque values are external handles, never managed storage"));
}

fn return_second_capture(
    _context: &EvaluatorStepContext<'_>,
    captures: &[Value],
) -> Result<Value, EvaluationHalt> {
    let [_, result] = captures else {
        unreachable!("the semantic-computation fixture has two captures")
    };
    Ok(result.clone())
}

#[test]
fn semantic_computation_captures_are_explicit() {
    let values = crate::core::test_value_factory();
    let lazy = LazyValue::semantic_computation(
        &values,
        "explicit capture fixture",
        [Value::Number(1.into()), Value::Number(2.into())],
        return_second_capture,
    );
    let Some(LazySource::SemanticComputation(computation)) = lazy.source_snapshot(&values) else {
        panic!("semantic computation should retain its explicit source")
    };

    assert_eq!(computation.captures.len(), 2);
    assert_eq!(
        computation.captures[1],
        Value::Number(2.into()),
        "the function pointer receives the exact ordered capture array"
    );
}

#[test]
fn external_host_call_requires_a_source_backed_record() {
    let values = crate::core::test_value_factory();
    let lazy = LazyValue::external_host_call(
        &values,
        "external call fixture",
        HostCallRecord::external_without_semantic_values(
            "external call fixture",
            "src/core/managed/containment_inventory.rs",
            "no value captures",
        ),
        [],
        |_| {
            Err(std::sync::Arc::new(EvaluationFailure::message(
                "not invoked",
            )))
        },
    );
    let Some(LazySource::HostCall(producer)) = lazy.source_snapshot(&values) else {
        panic!("external host call should retain its classified source")
    };

    assert_eq!(
        producer.record().fields(),
        (
            "external call fixture",
            "src/core/managed/containment_inventory.rs",
            "no value captures",
        )
    );
}

#[test]
fn external_closure_bundle_retains_only_declared_roots() {
    let values = crate::core::test_value_factory();
    let observed = Arc::new(std::sync::Mutex::new(None));
    let callback_observed = Arc::clone(&observed);
    let lazy = LazyValue::external_host_call(
        &values,
        "explicit host root bundle",
        HostCallRecord::external_with_semantic_values(
            "explicit host root bundle",
            "src/core/managed/containment_inventory.rs",
            "one explicit numeric value",
        ),
        [Value::Number(42.into())],
        move |captures| {
            let runtime = captures.runtime_id();
            let mut roots = captures.into_roots().into_vec();
            *callback_observed
                .lock()
                .expect("host-call observation mutex should not be poisoned") =
                Some((runtime, roots.len()));
            Ok(roots
                .pop()
                .expect("the declared host-call capture should be present"))
        },
    );
    let Some(LazySource::HostCall(producer)) = lazy.source_snapshot(&values) else {
        panic!("external host call should retain its classified source")
    };

    let result = producer
        .invoke(&values)
        .expect("the explicit root-bundle callback should succeed");
    assert_eq!(
        result.clone_core_for_test(),
        Value::Number(42.into()),
        "the callback receives the declared semantic value as a temporary runtime root"
    );
    assert_eq!(
        *observed
            .lock()
            .expect("host-call observation mutex should not be poisoned"),
        Some((values.runtime_id(), 1))
    );
}

#[test]
fn managed_deferred_state_cycle_reclaims() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let baseline = values
        .collect_managed_for_test()
        .expect("the deferred-state fixture should start collectible");
    {
        let promise = PromisedValue::new(&values, "host-call capture backedge");
        let lazy = LazyValue::external_host_call(
            &values,
            "traceable host-call capture",
            HostCallRecord::external_with_semantic_values(
                "traceable host-call capture",
                "src/core/managed/containment_inventory.rs",
                "one explicit promised value",
            ),
            [Value::Promised(promise.clone())],
            |_| Err(Arc::new(EvaluationFailure::message("not invoked"))),
        );
        set_test_promise(&values, &promise, Value::Lazy(lazy))
            .expect("the cycle promise should start unassigned");
    }

    let reclaimed = values
        .collect_managed_for_test()
        .expect("the explicit deferred-state cycle should collect");
    assert_eq!(reclaimed.root_entries(), baseline.root_entries());
    assert_eq!(reclaimed.marked_slots(), baseline.marked_slots());
    assert!(
        reclaimed.finalized_slots() >= 2,
        "the host-call lazy and promise cycle should both be reclaimed"
    );
    assert_eq!(values.drain_external_owners_for_test(), 1);
}

struct HostCaptureDrop(Arc<AtomicUsize>);

impl Drop for HostCaptureDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn host_call_capture_retires_only_during_external_registry_drain() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let drops = Arc::new(AtomicUsize::new(0));
    let capture = HostCaptureDrop(Arc::clone(&drops));
    {
        let _lazy = LazyValue::external_host_call(
            &values,
            "external owner fixture",
            HostCallRecord::external_without_semantic_values(
                "external owner fixture",
                "src/core/managed/containment_inventory.rs",
                "passive drop observer",
            ),
            [],
            move |_| {
                let _ = &capture;
                Err(Arc::new(EvaluationFailure::message("not invoked")))
            },
        );
        assert_eq!(values.external_owner_count_for_test(), 1);
    }
    assert_eq!(
        drops.load(Ordering::Relaxed),
        0,
        "ending the lazy edge's scope must not destroy its host capture"
    );
    values
        .collect_managed_for_test()
        .expect("the unrooted managed lazy should retire its external-owner handle");
    assert_eq!(values.drain_external_owners_for_test(), 1);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(values.external_owner_count_for_test(), 0);
}

struct OpaqueDropSignal(Arc<AtomicUsize>);

impl Drop for OpaqueDropSignal {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: this fixture contains no Glam value or managed pointer. Its drop
// observer models an arbitrary opaque destructor owned by the external
// registry rather than the managed-reachable token.
unsafe impl OpaquePayloadFamily for OpaqueDropSignal {
    const PAYLOAD_RECORD: OpaquePayloadRecord = OpaquePayloadRecord::external(
        "opaque external-owner fixture",
        "src/core/managed/containment_inventory.rs",
    );
}

#[test]
fn opaque_payload_requires_matching_runtime_and_retires_during_registry_drain() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let other_values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let drops = Arc::new(AtomicUsize::new(0));
    let payload = Arc::new(OpaqueDropSignal(Arc::clone(&drops)));
    let retained = Arc::downgrade(&payload);
    let opaque = OpaqueValue::new(&values, payload);

    assert!(opaque.downcast::<OpaqueDropSignal>(&other_values).is_none());
    assert!(opaque.downcast::<OpaqueDropSignal>(&values).is_some());
    drop(opaque);
    assert!(retained.upgrade().is_some());
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    assert_eq!(values.drain_external_owners_for_test(), 1);
    assert!(retained.upgrade().is_none());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn opaque_external_capabilities_retain_only_reviewed_routes() {
    crate::api::assert_effect_token_family_shape();
    crate::reflection::assert_task_handle_family_shape();
}

#[test]
fn opaque_downcast_requires_matching_runtime_and_preserves_owner_identity() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let other_values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let payload = Arc::new(OpaqueDropSignal(Arc::new(AtomicUsize::new(0))));
    let opaque = OpaqueValue::new(&values, Arc::clone(&payload));
    let same_owner = opaque.clone();
    let distinct_owner = OpaqueValue::new(&values, Arc::clone(&payload));

    assert_eq!(opaque, same_owner);
    assert_ne!(opaque, distinct_owner);
    let extracted = opaque
        .downcast::<OpaqueDropSignal>(&values)
        .expect("matching runtime and family should open the opaque owner");
    assert!(Arc::ptr_eq(&extracted, &payload));
    assert!(opaque.downcast::<u64>(&values).is_none());
    assert!(opaque.downcast::<OpaqueDropSignal>(&other_values).is_none());
}
