use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn hello_assembly_samples_write_hello_world_to_stdout() {
    for path in hello_sample_files() {
        let output = Command::new(env!("CARGO_BIN_EXE_glam"))
            .arg("--file")
            .arg(&path)
            .output()
            .unwrap_or_else(|err| panic!("failed to run glam for {}: {err}", path.display()));

        assert!(
            output.status.success(),
            "{} failed\nstdout: {}\nstderr: {}",
            path.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout,
            b"Hello, World!",
            "{} produced unexpected stdout",
            path.display()
        );
        assert_eq!(output.stderr, b"", "{} produced stderr", path.display());
    }
}

#[test]
fn short_file_option_writes_asm_result_to_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("-f")
        .arg("samples/assembly/hello_text.g")
        .output()
        .expect("failed to run glam");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"Hello, World!");
}

#[test]
fn multiple_files_compose_as_ordered_mixins() {
    let output = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("--file")
        .arg("samples/assembly/mixin_override.g")
        .arg("--file")
        .arg("samples/assembly/mixin_base.g")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Hello, World!");
    assert_eq!(output.stderr, b"");
}

#[test]
fn scripts_compose_with_files_as_ordered_mixins() {
    let output = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("--script.g")
        .arg("language g0\nasm.result := \"script\"\n")
        .arg("--file")
        .arg("samples/assembly/hello_text.g")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"script");
    assert_eq!(output.stderr, b"");
}

#[test]
fn assembly_args_default_to_empty_list() {
    let output = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("--script.g")
        .arg("language g0\nasm.result = { [[]]:\"empty\" }.[asm.args]\n")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"empty");
    assert_eq!(output.stderr, b"");
}

#[test]
fn assembly_args_are_string_list_and_can_be_rewritten_by_mixins() {
    let output = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("--script.g")
        .arg("language g0\nasm.result = { [[\"rewritten\"]]:\"ok\" }.[asm.args]\n")
        .arg("--script.g")
        .arg("language g0\nasm.args := [\"rewritten\"]\n")
        .arg("--")
        .arg("original")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ok");
    assert_eq!(output.stderr, b"");
}

#[test]
fn script_local_import_errors_only_when_observed() {
    let unused = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("--script.g")
        .arg("language g0\nimport \"missing.g\" as unused\nasm.result = \"ok\"\n")
        .output()
        .expect("failed to run glam");

    assert!(
        unused.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&unused.stdout),
        String::from_utf8_lossy(&unused.stderr)
    );
    assert_eq!(unused.stdout, b"ok");
    assert_eq!(unused.stderr, b"");

    let observed = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("--script.g")
        .arg("language g0\nimport \"missing.g\" as missing\nasm.result = missing.result\n")
        .output()
        .expect("failed to run glam");

    assert!(!observed.status.success());
    assert_eq!(observed.stdout, b"");
    assert!(
        String::from_utf8_lossy(&observed.stderr).contains(
            "local import `missing.g` cannot be loaded from a source without a file path"
        )
    );
}

#[test]
fn parse_errors_write_summary_and_diagnostics_to_stderr() {
    let output = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("--parse")
        .arg("samples/invalid/syntax/missing_language.g")
        .output()
        .expect("failed to run glam");

    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("samples/invalid/syntax/missing_language.g:1: error:"));
    assert!(stderr.contains("1 declarations"));
    assert!(stderr.contains("definition"));
}

#[test]
fn parse_success_writes_summary_to_stderr() {
    let output = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("--parse")
        .arg("samples/syntax/minimal.g")
        .output()
        .expect("failed to run glam");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("2 declarations"));
    assert!(stderr.contains("language"));
    assert!(stderr.contains("definition"));
}

fn hello_sample_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/assembly");
    let mut files = fs::read_dir(&root)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", root.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|err| panic!("failed to read entry in {}: {err}", root.display()))
                .path()
        })
        .filter(|path| path.is_file())
        .filter(|path| path.extension().is_some_and(|extension| extension == "g"))
        .filter(|path| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("hello_"))
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}
