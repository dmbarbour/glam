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
        ("src/compiler.rs", GatewayCounts::new(3, 0)),
        // I4.0's owner-local destruction fixtures exercise the admitted
        // construction gateway; production allocation still enters through
        // the same higher-ranked scope. GCI5R-002B's scoped gateway and
        // foreign-owner fixtures add three matching-domain access regions.
        // I8A.2's rooted-lazy transition fixture uses the same explicit test
        // gateway instead of exposing recursive-cell construction to sibling
        // modules.
        ("src/core/managed.rs", GatewayCounts::new(4, 2)),
        // I4F.2b's test-only passive-closure matrix allocates each real
        // compatibility value variant through the same bounded gateway.
        // GCI5R-005A/E add production effect- and target-backedge reflection
        // cycles plus matching-runtime source inspection.
        (
            "src/core/managed/active_owner_inventory.rs",
            GatewayCounts::new(2, 2),
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
        // I5D's recursive-cell cutover and I5F's closed self- and cross-family
        // cycle fixtures use matching-domain access for construction,
        // observation, publication, mutation-gateway installation, and root
        // projection. GCI5R-002 added deterministic production-writer probes
        // for lazy, promise, and synchronized net transitions through that
        // same bounded access surface. I8A.2 retires the two-access arbitrary
        // whole-net replacement fixture with the whole-net mutation bridge.
        // I8B adds two isolated managed-net cycle fixtures, each with one
        // construction/publication region.
        (
            "src/core/managed/recursive_cells.rs",
            GatewayCounts::new(39, 0),
        ),
        // I4F.2c keeps the production-shaped node and prepared root private
        // while their local lifecycle, provenance, and nested-access fixtures
        // exercise construction and observation.
        ("src/core/managed/value_node.rs", GatewayCounts::new(8, 0)),
        // I5D routes lazy and promise cell construction and access through the
        // same bounded domain gateway as reflection-value projection.
        // GCI5R-001B adds one synchronous construction entry which publishes
        // its returned graph before that same bounded access ends. GCI5R-003E
        // adds the explicit producer-installation gateway plus test-only
        // promise/lazy publication helpers; semantic facades no longer reopen
        // managed access to mutate themselves. GCI5R-003F removes their weak
        // observers; seven test-only inspection helpers now require an
        // explicit matching factory and enter through this counted gateway.
        // GCI5R-005B removes the former external reflection-root projection.
        ("src/core.rs", GatewayCounts::new(30, 5)),
        // I5D scopes every managed core-net construction, root handoff, and
        // source-frontier traversal through matching value-domain authority.
        // GCI5R-008's test-only prepared-source bridge reopens the matching
        // runtime solely to project a root-owned source for generic fixtures.
        ("src/core_net.rs", GatewayCounts::new(13, 0)),
        ("src/diagnostic.rs", GatewayCounts::new(1, 0)),
        ("src/eval/operator.rs", GatewayCounts::new(1, 0)),
        // Reflection evaluator fixtures construct their managed wrapper under
        // one bounded access region.
        ("src/eval/tests.rs", GatewayCounts::new(0, 1)),
        ("src/evaluation/access.rs", GatewayCounts::new(5, 0)),
        // Promise terminalization projects a managed assignment through the
        // producer root while the coordinator mutation remains admitted.
        ("src/evaluation/coordinator.rs", GatewayCounts::new(1, 0)),
        ("src/evaluation/executor.rs", GatewayCounts::new(1, 0)),
        // GCI5R-003E publishes a strict lazy-cycle failure through all of the
        // already-retained producer roots in one bounded batch.
        ("src/evaluation/pump.rs", GatewayCounts::new(1, 0)),
        // GCI5R-003D roots lazy and promise producers before coordinator
        // admission instead of letting either semantic façade reopen access.
        ("src/evaluation/session.rs", GatewayCounts::new(3, 0)),
        // Production-shaped task fixtures retain lazy/promise roots and use
        // explicit matching-domain access rather than facade mutation.
        ("src/evaluation/tests.rs", GatewayCounts::new(3, 0)),
        ("src/g_syntax/compiler_values.rs", GatewayCounts::new(2, 0)),
        (
            "src/g_syntax/diagnostic_formatter.rs",
            GatewayCounts::new(1, 0),
        ),
        ("src/g_syntax/module_lowering.rs", GatewayCounts::new(3, 0)),
        ("src/g_syntax/net_lowering.rs", GatewayCounts::new(3, 0)),
        // GCI5R-003D roots a freshly constructed reflection fixpoint before
        // publishing it into branch/coordinator state.
        ("src/reflection/machine.rs", GatewayCounts::new(1, 0)),
        // I6C's isolated failure-root lifecycle fixture constructs its managed
        // promise in one explicit region before publishing the durable root.
        ("src/runtime.rs", GatewayCounts::new(2, 0)),
    ]
    .into_iter()
    .map(|(path, counts)| (PathBuf::from(path), counts))
    .collect::<BTreeMap<_, _>>();

    assert_eq!(
        actual_gateways.keys().collect::<Vec<_>>(),
        expected_gateways.keys().collect::<Vec<_>>(),
        "managed gateway owners drifted"
    );
    for (path, expected) in &expected_gateways {
        assert_eq!(
            actual_gateways.get(path),
            Some(expected),
            "managed gateway count drifted for {}",
            path.display()
        );
    }
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
