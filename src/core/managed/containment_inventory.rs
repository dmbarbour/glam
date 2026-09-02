//! I4B source-backed inventory for deferred capture and opaque construction.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::{
    EvaluationFailure, EvaluationHalt, HostCallRecord, LazySource, LazyValue, Value,
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
            semantic_computation: source.matches("::semantic_computation(").count(),
            external_host_call: source.matches("::external_host_call(").count(),
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
    let Some(LazySource::SemanticComputation(computation)) = lazy.source_snapshot() else {
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
        HostCallRecord::external(
            "external call fixture",
            "src/core/managed/containment_inventory.rs",
            "no value captures",
        ),
        || {
            Err(std::sync::Arc::new(EvaluationFailure::message(
                "not invoked",
            )))
        },
    );
    let Some(LazySource::HostCall(producer)) = lazy.source_snapshot() else {
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
