//! I3E.2 inventory of compiler roots and their bounded projection regions.
//!
//! A raw semantic value may exist inside one callback-free compiler operation,
//! but any state retained across source loading, macro/evaluator waits,
//! diagnostics, imports, or compilation drain must use a runtime-qualified
//! root. Exact per-owner counts make moving or adding either a durable root or
//! a compatibility projection an explicit migration decision before I4F
//! changes the root's representation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BoundaryCounts {
    runtime_roots: usize,
    public_roots: usize,
    access_regions: usize,
    projections: usize,
}

impl BoundaryCounts {
    const fn new(counts: [usize; 4]) -> Self {
        Self {
            runtime_roots: counts[0],
            public_roots: counts[1],
            access_regions: counts[2],
            projections: counts[3],
        }
    }

    fn in_source(source: &str) -> Self {
        Self {
            runtime_roots: source.matches("RuntimeValueRoot").count(),
            public_roots: source.matches("PublicValue").count(),
            access_regions: source.matches("with_runtime_value_access").count(),
            projections: source.matches(".as_core()").count()
                + source.matches(".into_core()").count(),
        }
    }
}

struct InventoryEntry {
    path: &'static str,
    counts: BoundaryCounts,
    role: &'static str,
}

macro_rules! entry {
    ($path:literal, $counts:expr, $role:literal) => {
        InventoryEntry {
            path: $path,
            counts: BoundaryCounts::new($counts),
            role: $role,
        }
    };
}

const INVENTORY: &[InventoryEntry] = &[
    entry!(
        "src/api/assembly.rs",
        [12, 0, 1, 0],
        "rooted input setup, recursive loader results, and module result through drain"
    ),
    entry!(
        "src/compiler.rs",
        [28, 0, 1, 0],
        "rooted source definitions, final promise, origin, and import request"
    ),
    entry!(
        "src/g_syntax.rs",
        [6, 0, 0, 0],
        "rooted compiler diagnostics and lowered definitions across publication"
    ),
    entry!(
        "src/g_syntax/compiler_values.rs",
        [22, 0, 1, 0],
        "admitted complete rooted compiler-helper and effect caches"
    ),
    entry!(
        "src/g_syntax/diagnostic_formatter.rs",
        [4, 0, 1, 0],
        "admitted rooted closed diagnostic formatter cache"
    ),
    entry!(
        "src/g_syntax/macro_expansion/runner.rs",
        [0, 12, 0, 0],
        "rooted macro inputs, outputs, failures, and public diagnostic values"
    ),
    entry!(
        "src/g_syntax/module_lowering.rs",
        [5, 0, 3, 0],
        "rooted declaration-to-declaration definitions and reflection boundary"
    ),
    entry!(
        "src/g_syntax/parser/logical.rs",
        [0, 6, 0, 0],
        "rooted embedded macro data across declaration rewrites"
    ),
    entry!(
        "src/g_syntax/parser/source.rs",
        [0, 7, 0, 0],
        "bounded macro-data and diagnostic-context projection"
    ),
];

#[test]
fn compiler_root_and_projection_inventory_is_complete() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let actual = INVENTORY
        .iter()
        .map(|entry| {
            assert!(!entry.role.is_empty(), "{} needs a role", entry.path);
            let source = fs::read_to_string(manifest.join(entry.path))
                .expect("an inventoried compiler source should be readable");
            (
                PathBuf::from(entry.path),
                BoundaryCounts::in_source(&source),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let expected = INVENTORY
        .iter()
        .map(|entry| (PathBuf::from(entry.path), entry.counts))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(actual, expected);
}

#[test]
fn compiler_regions_do_not_reopen_the_direct_evaluator() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for entry in INVENTORY {
        let source = fs::read_to_string(manifest.join(entry.path))
            .expect("an inventoried compiler source should be readable");
        assert!(
            !source.contains("eval::eval_value("),
            "{} reopened the direct evaluator compatibility path",
            entry.path
        );
    }
}
