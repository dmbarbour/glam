use std::fs;
use std::path::{Path, PathBuf};

use glam::inspect_g_source;

/// Every fenced `g` block in the standing docs parses: whole, or item by
/// item, each as a declaration or an expression. An item is a line at
/// column zero with the lines after it, and an item that elides code with
/// `...` is skipped. Indented blocks are informal sketches and pseudocode,
/// and plans and reviews are history, so neither is checked.
#[test]
fn fenced_g_examples_in_the_docs_parse() {
    let mut failures = Vec::new();
    let mut blocks = 0;
    for doc in standing_docs() {
        let text = fs::read_to_string(&doc)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", doc.display()));
        let relative = doc
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(&doc)
            .display()
            .to_string();
        for (line, block) in fenced_g_blocks(&text) {
            blocks += 1;
            for item in failing_items(&block) {
                failures.push(format!("{relative}:{line}: {item}"));
            }
        }
    }
    assert!(blocks > 0, "the docs should hold fenced `g` examples");
    assert!(
        failures.is_empty(),
        "fenced `g` examples that do not parse:\n{}",
        failures.join("\n")
    );
}

/// The items of `block` that parse neither whole nor as a declaration or an
/// expression, by their first lines.
fn failing_items(block: &[&str]) -> Vec<String> {
    let whole = block.join("\n");
    let declares_language = block.iter().any(|line| line.starts_with("language "));
    let whole = if declares_language {
        whole
    } else {
        format!("language g0\n{whole}")
    };
    if parses(&whole) {
        return Vec::new();
    }
    items(block)
        .into_iter()
        .filter(|item| {
            let text = item.join("\n");
            !(text.contains("...") || text.contains('…') || text.starts_with("language "))
        })
        .filter(|item| {
            let declaration = format!("language g0\n{}", item.join("\n"));
            // An expression on lines of its own keeps its lines' alignment.
            let expression = format!(
                "language g0\nx =\n{}",
                item.iter()
                    .map(|line| format!("  {line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            !parses(&declaration) && !parses(&expression)
        })
        .map(|item| item[0].to_string())
        .collect()
}

fn parses(source: &str) -> bool {
    inspect_g_source(source.as_bytes())
        .diagnostics()
        .iter()
        .all(|diagnostic| diagnostic.severity() != glam::Severity::Error)
}

/// The block's items: each line at column zero, other than a closer,
/// begins one.
fn items<'a>(block: &[&'a str]) -> Vec<Vec<&'a str>> {
    let mut items: Vec<Vec<&str>> = Vec::new();
    for &line in block {
        if line.trim().is_empty() {
            continue;
        }
        let begins = !line.starts_with(char::is_whitespace) && !line.starts_with([')', ']', '}']);
        match items.last_mut() {
            Some(item) if !begins => item.push(line),
            _ => items.push(vec![line]),
        }
    }
    items
}

/// Each fenced `g` block's first line number and lines, unindented by its
/// fence's indentation.
fn fenced_g_blocks(text: &str) -> Vec<(usize, Vec<&str>)> {
    let mut blocks = Vec::new();
    let mut open: Option<(usize, usize, Vec<&str>)> = None;
    for (number, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let indentation = line.len() - trimmed.len();
        match &mut open {
            None if trimmed.trim_end() == "```g" => {
                open = Some((number + 2, indentation, Vec::new()))
            }
            None => {}
            Some(_) if trimmed.trim_end() == "```" => {
                let (start, _, lines) = open.take().expect("a block is open");
                blocks.push((start, lines));
            }
            Some((_, fence, lines)) => lines.push(line.get(*fence..).unwrap_or("")),
        }
    }
    blocks
}

fn standing_docs() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    let mut docs = Vec::new();
    collect_docs(&root, &mut docs);
    docs.sort();
    docs
}

fn collect_docs(dir: &Path, docs: &mut Vec<PathBuf>) {
    for entry in
        fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
    {
        let path = entry
            .unwrap_or_else(|err| panic!("failed to read an entry of {}: {err}", dir.display()))
            .path();
        if path.is_dir() {
            if !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("plans" | "reviews")
            ) {
                collect_docs(&path, docs);
            }
        } else if path.extension().is_some_and(|extension| extension == "md") {
            docs.push(path);
        }
    }
}
