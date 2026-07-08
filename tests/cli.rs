use std::process::Command;

#[test]
fn file_option_writes_asm_result_to_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("--file")
        .arg("samples/assembly/hello_text.g")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "glam failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Hello, World!");
    assert_eq!(output.stderr, b"");
}

#[test]
fn file_option_writes_computed_asm_result_to_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("--file")
        .arg("samples/assembly/hello_list.g")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "glam failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Hello, World!");
}

#[test]
fn file_option_writes_mixed_list_and_binary_result_to_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_glam"))
        .arg("--file")
        .arg("samples/assembly/hello_mixed.g")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "glam failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Hello, World!");
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
