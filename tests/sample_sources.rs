use std::fs;
use std::path::{Path, PathBuf};

use glam::diagnostic::Severity;
use glam::g_syntax::SourceFile;

#[test]
fn syntax_samples_parse_without_errors() {
    assert_samples_parse_without_errors("samples/syntax");
}

#[test]
fn config_samples_parse_without_errors() {
    assert_samples_parse_without_errors("samples/config");
}

#[test]
fn assembly_samples_parse_without_errors() {
    assert_samples_parse_without_errors("samples/assembly");
}

fn assert_samples_parse_without_errors(relative_dir: &str) {
    for path in sample_files(relative_dir) {
        if is_aspirational_syntax_sample(&path) {
            continue;
        }

        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let source = SourceFile::new(path.display().to_string(), text);
        let parsed = source.parse();
        let errors = parsed
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .collect::<Vec<_>>();

        assert!(
            errors.is_empty(),
            "{} had parse errors: {errors:#?}",
            path.display()
        );
    }
}

fn is_aspirational_syntax_sample(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("do_block.g" | "multi_line_text.g")
    )
}

fn sample_files(relative_dir: &str) -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_dir);
    let mut files = Vec::new();
    collect_g_files(&root, &mut files);
    files.sort();
    files
}

fn collect_g_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in
        fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
    {
        let path = entry
            .unwrap_or_else(|err| panic!("failed to read entry in {}: {err}", dir.display()))
            .path();

        if path.is_dir() {
            collect_g_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "g") {
            files.push(path);
        }
    }
}
