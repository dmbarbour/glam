//! Guards the boundary between source and history documents.
//!
//! `docs/plans` and `docs/reviews` are history, not authority (see
//! `docs/AgentContext.md`). Production and test source must not couple to their
//! prose: a test that `include_str!`s a plan or review turns an editorial reflow
//! into a build failure, and makes a completed plan undeletable. This check
//! fails closed if any `.rs` file under `src/` references those directories, so
//! the coupling cannot silently reappear.

use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &["docs/plans", "docs/reviews"];

#[test]
fn source_does_not_reference_history_documents() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut rust_files = Vec::new();
    collect_rust_files(&src, &mut rust_files);
    assert!(
        !rust_files.is_empty(),
        "found no .rs files under {}; the walker is broken",
        src.display()
    );

    let mut offenders = Vec::new();
    for path in rust_files {
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        for (number, line) in text.lines().enumerate() {
            if let Some(needle) = FORBIDDEN.iter().find(|needle| line.contains(**needle)) {
                offenders.push(format!(
                    "{}:{} references {needle}",
                    path.display(),
                    number + 1
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "source must not reference history documents under docs/plans or docs/reviews; \
         move the invariant into a code-level check or a current architecture doc:\n{}",
        offenders.join("\n")
    );
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in
        fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
    {
        let path = entry
            .unwrap_or_else(|err| panic!("failed to read entry in {}: {err}", dir.display()))
            .path();

        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}
