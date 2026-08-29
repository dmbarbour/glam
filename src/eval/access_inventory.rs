//! I3B.1 inventory of direct production entries into recursive evaluation.
//!
//! These compatibility calls do not yet inspect managed semantic pointers,
//! and production collection remains `NoAuto`. The inventory prevents a new
//! authority-free entry from appearing while I3B-I3E replace each listed
//! caller with a scheduler- or runtime-service-owned evaluator-step context.

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
    entry!("src/evaluation/pump.rs", [1, 0, 1, 0, 0], "I3B.1b/I3C"),
    entry!("src/g_syntax/compiler_values.rs", [1, 0, 0, 0, 0], "I3E.2"),
    entry!(
        "src/g_syntax/diagnostic_formatter.rs",
        [1, 0, 0, 0, 0],
        "I3E.2"
    ),
    entry!(
        "src/g_syntax/macro_expansion/effects.rs",
        [1, 0, 0, 1, 0],
        "I3E.2"
    ),
    entry!(
        "src/g_syntax/macro_expansion/runner.rs",
        [1, 0, 0, 0, 0],
        "I3E.2"
    ),
    entry!("src/g_syntax/parser/source.rs", [1, 0, 0, 0, 0], "I3E.2"),
    entry!("src/reflection/machine.rs", [1, 1, 0, 8, 3], "I3D.2/I3D.4"),
    entry!("src/reflection/protocol.rs", [1, 0, 0, 0, 0], "I3D.2"),
    entry!("src/reflection/requests.rs", [1, 0, 0, 1, 0], "I3D.2"),
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
