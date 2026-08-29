//! Phase I2C inventory of the production compatibility value boundary.
//!
//! The production facade still exposes bare-core conversion internally. This
//! test prevents that temporary surface from growing or moving unnoticed while
//! I3, I4F.1, and I4F.2 replace it with scoped access and managed roots.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AccessCounts {
    as_core: usize,
    into_core: usize,
    from_core: usize,
    from_runtime: usize,
    root_new: usize,
    root_from_runtime: usize,
}

impl AccessCounts {
    const fn new(
        as_core: usize,
        into_core: usize,
        from_core: usize,
        from_runtime: usize,
        root_new: usize,
        root_from_runtime: usize,
    ) -> Self {
        Self {
            as_core,
            into_core,
            from_core,
            from_runtime,
            root_new,
            root_from_runtime,
        }
    }

    fn in_source(source: &str) -> Self {
        Self::new(
            source.matches(".as_core()").count(),
            source.matches(".into_core()").count(),
            source.matches("Value::from_core(").count(),
            source.matches("Value::from_runtime(").count()
                + source.matches("Self::from_runtime(").count(),
            source.matches("RuntimeValueRoot::new(").count(),
            source.matches("RuntimeValueRoot::from_runtime(").count(),
        )
    }
}

struct InventoryEntry {
    path: &'static str,
    counts: AccessCounts,
    role: &'static str,
    migration: &'static str,
}

macro_rules! entry {
    ($path:literal, [$as_core:literal, $into_core:literal, $from_core:literal, $from_runtime:literal, $root_new:literal, $root_from_runtime:literal], $role:literal, $migration:literal) => {
        InventoryEntry {
            path: $path,
            counts: AccessCounts::new(
                $as_core,
                $into_core,
                $from_core,
                $from_runtime,
                $root_new,
                $root_from_runtime,
            ),
            role: $role,
            migration: $migration,
        }
    };
}

const INVENTORY: &[InventoryEntry] = &[
    entry!(
        "src/api/assembly.rs",
        [2, 2, 5, 2, 0, 0],
        "assembly setup, imports, modules, and reflection environment",
        "I3E.1/I3E.2 scoped demands and compiler regions; I4F.1 durable roots"
    ),
    entry!(
        "src/api/diagnostics.rs",
        [10, 5, 3, 3, 0, 0],
        "diagnostic construction, enrichment, projection, and transport",
        "I3E.3 callback regions; I4F.1 diagnostic roots"
    ),
    entry!(
        "src/api/error.rs",
        [2, 1, 2, 0, 0, 0],
        "evaluation-error conversion and diagnostic context",
        "I3E.3 callback regions; I4F.1 diagnostic roots"
    ),
    entry!(
        "src/api/evaluator.rs",
        [4, 0, 5, 0, 0, 0],
        "WHNF demand, reflection inspection, and owned extraction",
        "I3B.1 scoped evaluation; I4F.2 public facade switch"
    ),
    entry!(
        "src/api/runtime/readiness.rs",
        [0, 0, 2, 0, 0, 0],
        "readiness and deadlock report values",
        "I3A.4 outcome boundary; I4F.1 report roots"
    ),
    entry!(
        "src/api/value.rs",
        [20, 21, 0, 2, 1, 1],
        "constructors, composite validation, observers, extraction, and net data",
        "I3B.1 scoped construction/extraction; I4F.2 public facade switch"
    ),
    entry!(
        "src/core.rs",
        [0, 1, 0, 0, 0, 1],
        "compatibility root projection in core promise/deferred machinery",
        "I4A-I4E exact value shell; I4F.2 public facade switch"
    ),
    entry!(
        "src/eval/builtins/net/construction.rs",
        [4, 1, 5, 0, 0, 0],
        "effect-facing interaction-net construction",
        "I3D.3/I3D.4 scoped net access; I4F.1 outcomes; I8 managed net"
    ),
    entry!(
        "src/evaluation/coordinator/spark.rs",
        [0, 0, 0, 0, 1, 0],
        "durable spark demand",
        "I3A.4/I3C.2 poll outcomes; I4F.1 coordinator roots"
    ),
    entry!(
        "src/evaluation/coordinator/task.rs",
        [1, 0, 0, 0, 0, 0],
        "task completion projection",
        "I3A.4/I3C.2 poll outcomes; I4F.1 coordinator roots"
    ),
    entry!(
        "src/evaluation/pump.rs",
        [2, 0, 0, 0, 1, 3],
        "client demand, centralized spark evaluation, and pump completion",
        "I3A.3/I3B.2/I3C.2 scoped polling; I4F.1 outcomes"
    ),
    entry!(
        "src/evaluation/session.rs",
        [1, 1, 0, 0, 3, 0],
        "session demand, effect entry, and patient completion",
        "I3A.3/I3B.2/I3C.1 scoped polling; I4F.1 outcomes"
    ),
    entry!(
        "src/g_syntax/macro_expansion/effects.rs",
        [3, 0, 3, 0, 0, 0],
        "macro protocol evaluation and embedded values",
        "I3E.2 compiler and macro regions; I4F.1 retained roots"
    ),
    entry!(
        "src/g_syntax/macro_expansion/runner.rs",
        [2, 0, 2, 0, 0, 0],
        "macro runner demand and result conversion",
        "I3E.2 compiler and macro regions; I4F.1 retained roots"
    ),
    entry!(
        "src/g_syntax/parser/logical.rs",
        [1, 0, 3, 0, 0, 0],
        "embedded source values and macro replay",
        "I3E.2 compiler and macro regions; I4F.1 retained roots"
    ),
    entry!(
        "src/g_syntax/parser/source.rs",
        [2, 0, 0, 0, 0, 0],
        "source diagnostics and conditional definitions",
        "I3E.2 compiler regions; I4F.1 retained roots"
    ),
    entry!(
        "src/reflection/lifecycle.rs",
        [1, 2, 1, 0, 0, 0],
        "effect lifecycle activation and completion",
        "I3D.1/I3D.2 reflection phases; I4F.1 lifecycle roots"
    ),
    entry!(
        "src/reflection/machine.rs",
        [4, 5, 13, 0, 1, 0],
        "persistent effect machine, continuations, and store access",
        "I3D.2/I3D.4 interpreter phases; I4F.1 machine roots"
    ),
    entry!(
        "src/reflection/protocol.rs",
        [2, 1, 3, 0, 0, 0],
        "task-host evaluation, results, and failures",
        "I3D.2 interpreter boundary; I4F.1 task roots"
    ),
    entry!(
        "src/reflection/requests.rs",
        [13, 10, 21, 0, 0, 0],
        "standard reflection request parsing and results",
        "I3D.2 interpreter boundary; I4F.1 request roots"
    ),
    entry!(
        "src/reflection/search.rs",
        [3, 0, 1, 0, 0, 0],
        "isolated effect search and environment",
        "I3D.2/I3D.4 search regions; I4F.1 search roots"
    ),
    entry!(
        "src/reflection/store.rs",
        [4, 3, 11, 0, 0, 0],
        "persistent store, query roots, and transactional updates",
        "I3D.4 store regions; I4F.1 persistent roots"
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

fn is_inventoried_source(relative: &Path) -> bool {
    if relative.starts_with("src/bin") {
        return false;
    }
    if relative
        .components()
        .any(|component| component.as_os_str() == "tests")
    {
        return false;
    }
    if relative.file_name().is_some_and(|name| name == "tests.rs") {
        return false;
    }
    !matches!(
        relative.to_str(),
        Some("src/api/value/prototype.rs" | "src/api/value/access_inventory.rs")
    )
}

#[test]
fn public_value_compatibility_access_inventory_is_complete() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);

    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(manifest)
                .expect("a discovered source should belong to this package");
            if !is_inventoried_source(relative) {
                return None;
            }
            let source = fs::read_to_string(&path).expect("an inventoried source should be UTF-8");
            let counts = AccessCounts::in_source(&source);
            (counts != AccessCounts::new(0, 0, 0, 0, 0, 0)).then(|| {
                (
                    relative
                        .to_str()
                        .expect("repository source paths should be UTF-8")
                        .replace('\\', "/"),
                    counts,
                )
            })
        })
        .collect::<BTreeMap<_, _>>();

    let mut expected = BTreeMap::new();
    for entry in INVENTORY {
        assert!(!entry.role.is_empty(), "{} needs a role", entry.path);
        assert!(
            !entry.migration.is_empty(),
            "{} needs a migration checkpoint",
            entry.path
        );
        assert!(
            expected
                .insert(entry.path.to_owned(), entry.counts)
                .is_none(),
            "{} appears twice in the compatibility inventory",
            entry.path
        );
    }

    assert_eq!(actual, expected);
}
