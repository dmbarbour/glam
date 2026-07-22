use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn hello_assembly_samples_write_hello_world_to_stdout() {
    for path in hello_sample_files() {
        let output = glam_command()
            .arg("--file")
            .arg(&path)
            .output()
            .unwrap_or_else(|error| panic!("failed to run glam for {}: {error}", path.display()));

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

fn hello_sample_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/hello");
    let mut files = fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", root.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| {
                    panic!("failed to read entry in {}: {error}", root.display())
                })
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

fn glam_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_glam"));
    command.env("GLAM_CONF", "samples/config/unit_tests.g");
    command.env_remove("GLAM_WORKERS");
    command
}
