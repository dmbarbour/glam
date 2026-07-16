use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use glam::compiler::CompileContext;
use glam::diagnostic::Severity;
use glam::g_syntax::{Diagnostic, lower_to_core_with_context, parse_source};

#[test]
fn invalid_syntax_samples_report_expected_diagnostics() {
    for source_path in sample_files("samples/invalid/syntax") {
        let expect_path = source_path.with_extension("expect");
        assert!(
            expect_path.exists(),
            "{} is missing sibling expectation file {}",
            source_path.display(),
            expect_path.display()
        );

        let source_bytes = fs::read(&source_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", source_path.display()));
        let expect_text = fs::read_to_string(&expect_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", expect_path.display()));

        let context = CompileContext::from_module_path(["assembly"]);
        let parsed = parse_source(&source_bytes);
        let lowered = lower_to_core_with_context(parsed, &context);
        assert_expectations(
            &source_path,
            &parse_expectations(&expect_path, &expect_text),
            &lowered.diagnostics,
        );
    }
}

#[test]
fn invalid_eval_samples_report_expected_runtime_errors() {
    for source_path in sample_files("samples/invalid/eval") {
        let expect_path = source_path.with_extension("expect");
        assert!(
            expect_path.exists(),
            "{} is missing sibling expectation file {}",
            source_path.display(),
            expect_path.display()
        );

        let expect_text = fs::read_to_string(&expect_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", expect_path.display()));
        let expected_stderr = parse_eval_expectations(&expect_path, &expect_text);

        let output = Command::new(env!("CARGO_BIN_EXE_glam"))
            .arg("--file")
            .arg(&source_path)
            .output()
            .unwrap_or_else(|err| {
                panic!("failed to run glam for {}: {err}", source_path.display())
            });

        assert!(
            !output.status.success(),
            "{} unexpectedly succeeded\nstdout: {}\nstderr: {}",
            source_path.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout,
            b"",
            "{} produced unexpected stdout",
            source_path.display()
        );

        let stderr = String::from_utf8_lossy(&output.stderr);
        for expected in expected_stderr {
            assert!(
                stderr.contains(&expected),
                "{} stderr did not contain expected substring {:?}\nstderr: {}",
                source_path.display(),
                expected,
                stderr
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpectedDiagnostic {
    severity: Severity,
    line: usize,
    substrings: Vec<String>,
}

fn parse_expectations(path: &Path, text: &str) -> Vec<ExpectedDiagnostic> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                None
            } else {
                Some(parse_expectation(path, index + 1, line))
            }
        })
        .collect()
}

fn parse_eval_expectations(path: &Path, text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                None
            } else {
                assert!(
                    !line.contains('|'),
                    "{}:{} eval expectations should be plain stderr substrings",
                    path.display(),
                    index + 1
                );
                Some(line.to_owned())
            }
        })
        .collect()
}

fn parse_expectation(path: &Path, line_number: usize, line: &str) -> ExpectedDiagnostic {
    let parts = line.split('|').map(str::trim).collect::<Vec<_>>();
    assert!(
        parts.len() >= 3,
        "{}:{} expectation should be `severity|line|substring...`",
        path.display(),
        line_number
    );

    let severity = match parts[0] {
        "info" => Severity::Info,
        "warning" => Severity::Warning,
        "error" => Severity::Error,
        other => panic!(
            "{}:{} unknown severity `{other}`",
            path.display(),
            line_number
        ),
    };
    let line = parts[1].parse::<usize>().unwrap_or_else(|err| {
        panic!(
            "{}:{} invalid diagnostic line `{}`: {err}",
            path.display(),
            line_number,
            parts[1]
        )
    });
    let substrings = parts[2..]
        .iter()
        .map(|part| (*part).to_owned())
        .collect::<Vec<_>>();

    ExpectedDiagnostic {
        severity,
        line,
        substrings,
    }
}

fn assert_expectations(source_path: &Path, expected: &[ExpectedDiagnostic], actual: &[Diagnostic]) {
    for expected in expected {
        assert!(
            actual
                .iter()
                .any(|actual| diagnostic_matches(expected, actual)),
            "{} did not report expected diagnostic {expected:?}\nactual diagnostics: {actual:#?}",
            source_path.display()
        );
    }
}

fn diagnostic_matches(expected: &ExpectedDiagnostic, actual: &Diagnostic) -> bool {
    expected.severity == actual.severity
        && expected.line == actual.line
        && expected
            .substrings
            .iter()
            .all(|substring| actual.message.contains(substring))
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
