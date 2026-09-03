//! Production managed-root publication and forbidden-escape inventories.
//!
//! Registered root creation is a legitimate publication boundary and remains
//! source-counted by owner. Authority-free bare-core conversions are not a
//! migration allowance: the second latch rejects them anywhere in production.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RootPublicationCounts {
    root_new: usize,
}

impl RootPublicationCounts {
    const fn new(root_new: usize) -> Self {
        Self { root_new }
    }

    fn in_source(source: &str) -> Self {
        Self::new(source.matches("RuntimeValueRoot::new(").count())
    }
}

struct InventoryEntry {
    path: &'static str,
    counts: RootPublicationCounts,
    role: &'static str,
    migration: &'static str,
}

macro_rules! entry {
    ($path:literal, $root_new:literal, $role:literal, $migration:literal) => {
        InventoryEntry {
            path: $path,
            counts: RootPublicationCounts::new($root_new),
            role: $role,
            migration: $migration,
        }
    };
}

const INVENTORY: &[InventoryEntry] = &[
    entry!(
        "src/api/assembly.rs",
        2,
        "assembly setup, rooted compiler handoff, import results, modules, and reflection environment",
        "I3E.2 bounded compiler regions; I4F.1 durable roots"
    ),
    entry!(
        "src/api/value.rs",
        1,
        "constructors, composite validation, observers, extraction, and net data",
        "I3B.1 scoped construction/extraction; I4F.2 public facade switch"
    ),
    entry!(
        "src/compiler.rs",
        10,
        "rooted source context, origins, definition promises, and import handoff",
        "I3E.2 bounded compiler regions; I4F.1 durable roots"
    ),
    entry!(
        "src/core.rs",
        6,
        "post-domain canonical-root initialization, promise publication, and externalized reflection effect/target roots",
        "I4F.2d.0 canonical initialization; I4F.2a.1c fixture closure; I4F.2b.2 reflection ownership; I5 managed promise assignment"
    ),
    entry!(
        "src/evaluation/access.rs",
        2,
        "poll/evaluator-step completion rooting and scoped projection",
        "I3A.4/I3C.2 outcome typing and projection; I4F.2 managed root switch"
    ),
    entry!(
        "src/evaluation/coordinator/spark.rs",
        1,
        "durable spark demand",
        "I3A.4/I3C.2 poll outcomes; I4F.1 coordinator roots"
    ),
    entry!(
        "src/evaluation/pump.rs",
        1,
        "centralized client/spark evaluation and exceptional lazy-cycle publication",
        "I3A.3/I3B.1b/I3B.2/I3C.2 scoped polling; I4F.1 outcomes"
    ),
    entry!(
        "src/evaluation/session.rs",
        4,
        "session demand, reserved reflection activation, effect entry, and patient completion",
        "I3A.3/I3B.2/I3C.1-I3D.1 scoped polling and activation; I4F.1 outcomes"
    ),
    entry!(
        "src/g_syntax.rs",
        2,
        "rooted lowered definitions and compiler diagnostics across publication",
        "I3E.2 bounded compiler regions; I4F.1 durable roots"
    ),
    entry!(
        "src/g_syntax/compiler_values.rs",
        1,
        "complete runtime-cached compiler helper bundles",
        "I3E.2 rooted cache publication; I4F.1 durable roots"
    ),
    entry!(
        "src/g_syntax/diagnostic_formatter.rs",
        1,
        "runtime-cached closed diagnostic formatter",
        "I3E.2 rooted cache publication; I4F.1 durable roots"
    ),
    entry!(
        "src/g_syntax/module_lowering.rs",
        2,
        "declaration-to-declaration definitions and reflection annotator",
        "I3E.2 bounded lowering regions; I4F.1 durable roots"
    ),
    entry!(
        "src/reflection/machine.rs",
        7,
        "rooted reflection machine and decoded-request handoff plus bounded evaluator, parser, and store access",
        "I3D.2/I3D.4 interpreter phases; I4F.1d.3 complete machine roots and bounded raw values; I4F.2a compatibility-access retirement"
    ),
    entry!(
        "src/runtime.rs",
        1,
        "shallow direct-value rooting for one runtime failure root",
        "I4F.1c.1 failure-root boundary; I6C managed failure shell"
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
    if relative
        .file_name()
        .is_some_and(|name| name == "tests.rs" || name == "access_inventory.rs")
    {
        return false;
    }
    true
}

#[test]
fn registered_runtime_root_publication_inventory_is_complete() {
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
            let counts = RootPublicationCounts::in_source(&source);
            (counts != RootPublicationCounts::new(0)).then(|| {
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
            "{} appears twice in the root-publication inventory",
            entry.path
        );
    }

    assert_eq!(actual, expected);
}

#[test]
fn public_value_switch_inventory_has_no_compatibility_escape() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);

    let forbidden = [
        ".as_core()",
        ".into_core()",
        "Value::from_core(",
        "Value::from_runtime(",
        "Self::from_runtime(",
        "RuntimeValueRoot::from_runtime(",
    ];
    let mut escapes = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(manifest)
            .expect("a discovered source should belong to this package");
        if !is_inventoried_source(relative) {
            continue;
        }
        let source = fs::read_to_string(&path).expect("production source should be UTF-8");
        for forbidden in forbidden {
            if source.contains(forbidden) {
                escapes.push((relative.to_path_buf(), forbidden));
            }
        }
    }

    assert!(
        escapes.is_empty(),
        "the managed public-value switch regained authority-free core escapes: {escapes:?}"
    );
}

fn braced_item_after<'a>(source: &'a str, marker: &str) -> &'a str {
    let start = source
        .find(marker)
        .expect("source item marker should exist");
    let open = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .expect("source item should have a body");
    let mut depth = 0_usize;
    for (offset, byte) in source[open..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[start..=open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("source item body should be balanced");
}

#[test]
fn public_value_facade_exposes_no_core_or_provenance_observer() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(manifest.join("src/api/value.rs"))
        .expect("the public value facade should be readable");
    let value_impl = braced_item_after(&source, "impl Value {");

    for forbidden in [
        "pub fn as_core",
        "pub fn into_core",
        "pub(crate) fn into_core",
        "pub fn runtime_id",
        "pub fn is_undefined",
        "pub fn as_binary",
        "pub fn as_i64",
        "pub fn kind",
        "pub fn as_number_text",
    ] {
        assert!(
            !value_impl.contains(forbidden),
            "public Value facade regained forbidden surface `{forbidden}`"
        );
    }
}
