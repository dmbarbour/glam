#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

#[test]
fn direct_assembly_repeated_splits_generate_a_runnable_hello_world_elf() {
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

    let entry_address = u64::from_le_bytes(
        generated.stdout[24..32]
            .try_into()
            .expect("ELF entry field should contain eight bytes"),
    );
    assert_eq!(
        entry_address, 0x400079,
        "the published `_start` label should follow the leading trap byte"
    );
    assert_eq!(
        generated.stdout[120], 0xcc,
        "the byte preceding the published entry should remain in the image"
    );

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

#[test]
fn direct_assembly_exposes_only_the_public_effect_api() {
    let inspected = Command::new(env!("CARGO_BIN_EXE_glam"))
        .env("GLAM_CONF", "samples/config/direct_assembly.g")
        .env_remove("GLAM_WORKERS")
        .arg("--script.g")
        .arg(
            "language g0\n\
             import 'std\n\
             asm.result = anno 'binary (\n\
             \x20\x20if env.x86_64.run == {} and\n\
             \x20\x20\x20\x20env.x86_64.continue_after == {}\n\
             \x20\x20then \"ok\"\n\
             \x20\x20else \"handler internals leaked\"\n\
             )",
        )
        .output()
        .expect("direct-assembly API inspection should run through glam");

    assert!(
        inspected.status.success(),
        "inspection failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&inspected.stdout),
        String::from_utf8_lossy(&inspected.stderr)
    );
    assert_eq!(inspected.stdout, b"ok");
    assert!(inspected.stderr.is_empty());
}

#[test]
fn direct_assembly_rejects_duplicate_symbol_publication() {
    let rejected = Command::new(env!("CARGO_BIN_EXE_glam"))
        .env("GLAM_CONF", "samples/config/direct_assembly.g")
        .env_remove("GLAM_WORKERS")
        .arg("--script.g")
        .arg(
            "language g0\n\
             import 'std\n\
             program = do\n\
             \x20\x20.section.root 'text -> root\n\
             \x20\x20.cursor.on root do\n\
             \x20\x20\x20\x20.global \"_start\" -> _\n\
             \x20\x20\x20\x20.global \"_start\" -> _\n\
             \x20\x20\x20\x20.r ()\n\
             asm.result = env.linux_x86_64.executable program",
        )
        .output()
        .expect("invalid direct-assembly program should run through glam");

    assert!(
        !rejected.status.success(),
        "duplicate symbol publication unexpectedly succeeded"
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("direct-assembly symbol is already published"),
        "missing duplicate-publication diagnostic: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

fn generated_executable_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!("direct-assembly-hello-{}", std::process::id()))
}
