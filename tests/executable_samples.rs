#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

#[test]
fn direct_assembly_sample_generates_a_runnable_hello_world_elf() {
    let generated = Command::new(env!("CARGO_BIN_EXE_glam"))
        .env("GLAM_CONF", "samples/config/direct_assembly.g")
        .env_remove("GLAM_WORKERS")
        .arg("--file")
        .arg("samples/executable/hello_x86_64_linux/hello.g")
        .output()
        .expect("direct-assembly sample should run through glam");

    assert!(
        generated.status.success(),
        "assembly failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&generated.stdout),
        String::from_utf8_lossy(&generated.stderr)
    );
    assert!(
        generated.stderr.is_empty(),
        "assembly emitted diagnostics: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    assert_eq!(&generated.stdout[..4], b"\x7fELF");

    let path = generated_executable_path();
    fs::write(&path, &generated.stdout).expect("generated ELF should be writable");
    let mut permissions = fs::metadata(&path)
        .expect("generated ELF should have metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).expect("generated ELF should be executable");

    let executed = Command::new(&path)
        .output()
        .expect("generated ELF should execute");
    let _ = fs::remove_file(&path);

    assert!(executed.status.success(), "generated ELF should exit zero");
    assert_eq!(executed.stdout, b"Hello, World!\n");
    assert!(executed.stderr.is_empty());
}

fn generated_executable_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("direct-assembly-hello-{}", std::process::id()))
}
