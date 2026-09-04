//! I3F inventory of every Glam-side managed-heap admission.
//!
//! The core value domain owns the only direct `Heap::with_mutator` calls.
//! Evaluation, compiler, API, and test code enter through one of its two
//! higher-ranked gateways, so mutator authority cannot outlive the callback.
//! Exact per-owner counts make a new admission site an explicit review event;
//! the concurrent collector plan reuses this inventory when these bounded
//! regions become participant epochs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GatewayCounts {
    access: usize,
    construction: usize,
}

impl GatewayCounts {
    const fn new(access: usize, construction: usize) -> Self {
        Self {
            access,
            construction,
        }
    }

    fn in_source(source: &str) -> Self {
        Self {
            access: source.matches(".with_runtime_value_access(").count(),
            construction: source.matches(".with_managed_values(").count(),
        }
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

#[test]
fn all_managed_entries_have_bounded_mutator_regions() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);

    let inventory_path = Path::new("src/evaluation/access_inventory.rs");
    let mut actual_gateways = BTreeMap::new();
    let mut direct_entries = BTreeMap::new();
    for path in sources {
        let relative = path
            .strip_prefix(manifest)
            .expect("a source path should belong to this package");
        if relative == inventory_path {
            continue;
        }
        let source = fs::read_to_string(&path).expect("Rust source should be readable");
        let gateways = GatewayCounts::in_source(&source);
        if !gateways.is_empty() {
            actual_gateways.insert(relative.to_path_buf(), gateways);
        }
        let direct = source.matches(".with_mutator(").count();
        if direct != 0 {
            direct_entries.insert(relative.to_path_buf(), direct);
        }
    }

    let expected_gateways = [
        ("src/api/assembly.rs", GatewayCounts::new(1, 0)),
        ("src/api/tests.rs", GatewayCounts::new(0, 2)),
        ("src/api/value.rs", GatewayCounts::new(2, 0)),
        ("src/compiler.rs", GatewayCounts::new(1, 0)),
        // I4.0's owner-local destruction fixtures exercise the admitted
        // construction gateway; production allocation still enters through
        // the same higher-ranked scope.
        ("src/core/managed.rs", GatewayCounts::new(0, 2)),
        // I4F.2b's test-only passive-closure matrix allocates each real
        // compatibility value variant through the same bounded gateway.
        (
            "src/core/managed/active_owner_inventory.rs",
            GatewayCounts::new(0, 2),
        ),
        (
            "src/core/managed/payload_edges/persistent.rs",
            GatewayCounts::new(0, 1),
        ),
        // I5B's synthetic managed-leaf fixtures construct closed graphs to
        // verify transitive compatibility traversal and identity stops.
        (
            "src/core/managed/payload_edges/managed.rs",
            GatewayCounts::new(0, 2),
        ),
        (
            "src/core/managed/payload_edges/runtime_net.rs",
            GatewayCounts::new(3, 1),
        ),
        // I5C's dormant recursive-cell fixtures prove matching-domain access
        // and reject unrelated runtime-value authority.
        (
            "src/core/managed/recursive_cells.rs",
            GatewayCounts::new(10, 0),
        ),
        // I4F.2c keeps the production-shaped node and prepared root private
        // while their local lifecycle, provenance, and nested-access fixtures
        // exercise construction and observation.
        ("src/core/managed/value_node.rs", GatewayCounts::new(8, 0)),
        // I4F.2b.2 briefly projects a rooted reflection effect through a
        // bounded access region before coordinator reservation.
        ("src/core.rs", GatewayCounts::new(3, 5)),
        // I5C.1c scopes source-frontier traversal through the same matching
        // value-domain authority instead of reopening a stored source owner;
        // the existing test gateway also exercises mismatched-net rejection.
        ("src/core_net.rs", GatewayCounts::new(8, 0)),
        // The reflection active-owner fixture proves that managed
        // finalization leaves reservation cancellation to the external drain.
        ("src/eval/tests.rs", GatewayCounts::new(0, 1)),
        ("src/evaluation/access.rs", GatewayCounts::new(5, 0)),
        ("src/evaluation/executor.rs", GatewayCounts::new(1, 0)),
        ("src/evaluation/session.rs", GatewayCounts::new(2, 0)),
        ("src/g_syntax/compiler_values.rs", GatewayCounts::new(1, 0)),
        (
            "src/g_syntax/diagnostic_formatter.rs",
            GatewayCounts::new(1, 0),
        ),
        ("src/g_syntax/module_lowering.rs", GatewayCounts::new(3, 0)),
        ("src/runtime.rs", GatewayCounts::new(1, 0)),
    ]
    .into_iter()
    .map(|(path, counts)| (PathBuf::from(path), counts))
    .collect::<BTreeMap<_, _>>();

    assert_eq!(actual_gateways, expected_gateways);
    assert_eq!(
        direct_entries,
        [(PathBuf::from("src/core/managed.rs"), 2)]
            .into_iter()
            .collect(),
        "raw mutator admission must remain private to the core value domain"
    );

    let owner = fs::read_to_string(manifest.join("src/core/managed.rs"))
        .expect("the managed value-domain owner should be readable");
    assert_eq!(
        owner.matches("impl for<'scope> FnOnce(").count(),
        2,
        "both managed gateways must preserve a higher-ranked callback scope"
    );
}
