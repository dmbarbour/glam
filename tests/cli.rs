use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn hello_assembly_samples_write_hello_world_to_stdout() {
    for path in hello_sample_files() {
        let output = glam_command()
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
    let output = glam_command()
        .arg("-f")
        .arg("samples/assembly/hello_text.g")
        .output()
        .expect("failed to run glam");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"Hello, World!");
}

#[test]
fn multiple_files_compose_as_ordered_mixins() {
    let output = glam_command()
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
    let output = glam_command()
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
    let output = glam_command()
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
    let output = glam_command()
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
fn configuration_env_is_visible_to_assembly() {
    let output = glam_command()
        .arg("--script.g")
        .arg("language g0\nasm.result = env.hello_message\n")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Hello from unit_tests conf.env!");
    assert_eq!(output.stderr, b"");
}

#[test]
fn import_as_inherits_assembly_env() {
    let dir = unique_temp_dir("glam-import-as-env");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create temp dir {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    let main = dir.join("main.g");
    let lib = dir.join("lib.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nextend conf.env with\n  message = \"Hello from import env!\"\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));
    fs::write(
        &main,
        "language g0\nimport \"lib.g\" as lib\nasm.result = lib.result\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", main.display()));
    fs::write(&lib, "language g0\nresult = env.message\n")
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", lib.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--file")
        .arg(&main)
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Hello from import env!");
    assert_eq!(output.stderr, b"");
}

#[test]
fn configuration_files_compose_using_path_separator() {
    let dir = unique_temp_dir("glam-conf-path-list");
    fs::create_dir_all(&dir).unwrap_or_else(|err| {
        panic!(
            "failed to create temp configuration dir {}: {err}",
            dir.display()
        )
    });
    let base = dir.join("base.g");
    let override_ = dir.join("override.g");
    fs::write(
        &base,
        "language g0\nobject conf.env\nextend conf.env with\n  message = \"base\"\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", base.display()));
    fs::write(
        &override_,
        "language g0\nextend conf.env with\n  message := \"override\"\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", override_.display()));
    let conf = env::join_paths([override_.as_path(), base.as_path()])
        .expect("test configuration paths should join");

    let output = glam_command()
        .env("GLAM_CONF", conf)
        .arg("--script.g")
        .arg("language g0\nasm.result = env.message\n")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"override");
    assert_eq!(output.stderr, b"");
}

#[test]
fn undefined_configuration_env_defaults_assembly_env_to_empty_object() {
    let dir = unique_temp_dir("glam-undefined-conf-env");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create temp config dir {}: {err}", dir.display()));
    let config = dir.join("no_env.g");
    fs::write(&config, "language g0\n")
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--script.g")
        .arg("language g0\nasm.result = { [{}]:\"missing\" }.[env.missing]\n")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"missing");
    assert_eq!(output.stderr, b"");
}

#[test]
fn script_local_import_errors_only_when_observed() {
    let unused = glam_command()
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

    let observed = glam_command()
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
fn script_binary_import_errors_only_when_observed() {
    let unused = glam_command()
        .arg("--script.g")
        .arg("language g0\nimport \"missing.bin\" binary as unused\nasm.result = \"ok\"\n")
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

    let observed = glam_command()
        .arg("--script.g")
        .arg("language g0\nimport \"missing.bin\" binary as missing\nasm.result = missing\n")
        .output()
        .expect("failed to run glam");

    assert!(!observed.status.success());
    assert_eq!(observed.stdout, b"");
    assert!(String::from_utf8_lossy(&observed.stderr).contains(
        "binary import `missing.bin` cannot be loaded from a source without a file path"
    ));
}

#[test]
fn parse_errors_write_summary_and_diagnostics_to_stderr() {
    let output = glam_command()
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
    let output = glam_command()
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

#[test]
fn configured_logger_reads_diagnostics_and_writes_stderr_effectfully() {
    let dir = unique_temp_dir("glam-conf-log");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    let invalid = dir.join("invalid.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nconf.log = .read_log >>= (\\message -> .write_stderr (\"CUSTOM: \" ++ message.msg.text ++ [10]))\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));
    fs::write(&invalid, b"language g0\nvalue = \xff\n")
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", invalid.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--file")
        .arg(&invalid)
        .output()
        .expect("failed to run glam");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("CUSTOM: source is not valid UTF-8"),
        "configured logger did not render the diagnostic:\n{stderr}"
    );
}

#[test]
fn configured_logger_composes_reusable_reflection_requests() {
    let dir = unique_temp_dir("glam-conf-log-request");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    let invalid = dir.join("invalid.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nconf.log = .cut (.read_log >>= (\\_message -> (.log 'warn { msg:{ text:\"REFLECTION LOG\" } }) =>> (.write_stderr (\"COMPOSED\" ++ [10]) =>> .r ())))\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));
    fs::write(&invalid, b"language g0\nvalue = \xff\n")
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", invalid.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--file")
        .arg(&invalid)
        .output()
        .expect("failed to run glam");

    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("COMPOSED\n"),
        "configured logger did not run its main-owned request:\n{stderr}"
    );
    assert!(
        stderr.contains("warning: REFLECTION LOG\n"),
        "configured logger did not compose the reflection request:\n{}",
        stderr
    );
}

#[test]
fn configured_logger_can_join_a_restricted_reflection_task() {
    let dir = unique_temp_dir("glam-conf-log-task");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    let invalid = dir.join("invalid.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nconf.log = .refl_task (.r \"SPAWNED\") >>= (\\task -> .join_task task >>= (\\text -> .write_stderr (text ++ [10])))\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));
    fs::write(&invalid, b"language g0\nvalue = \xff\n")
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", invalid.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--file")
        .arg(&invalid)
        .output()
        .expect("failed to run glam");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SPAWNED\n"),
        "configured logger did not join its child reflection task:\n{stderr}"
    );
}

#[test]
fn configured_logger_can_read_the_os_environment() {
    let dir = unique_temp_dir("glam-conf-log-env");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    let invalid = dir.join("invalid.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nconf.log = .os_env \"GLAM_TEST_REFLECTION_ENV\" >>= (\\value -> .write_stderr (value ++ [10]))\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));
    fs::write(&invalid, b"language g0\nvalue = \xff\n")
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", invalid.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .env("GLAM_TEST_REFLECTION_ENV", "FROM ENV")
        .arg("--file")
        .arg(&invalid)
        .output()
        .expect("failed to run glam");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("FROM ENV\n"),
        "configured logger did not receive the OS environment:\n{stderr}"
    );
}

#[test]
fn configured_logger_output_precedes_a_later_logger_failure() {
    let dir = unique_temp_dir("glam-conf-log-failure");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nconf.log = (.log 'warn { msg:{ text:\"REMAINING MESSAGE\" } }) =>> .get 42\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--script.g")
        .arg("language g0\nasm.result = \"ok\"\n")
        .output()
        .expect("failed to run glam");

    assert!(!output.status.success());
    assert_eq!(output.stdout, b"ok");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let failure = stderr
        .find("configured logger failed")
        .unwrap_or_else(|| panic!("logger failure diagnostic was not rendered:\n{stderr}"));
    let emitted = stderr
        .find("REMAINING MESSAGE")
        .unwrap_or_else(|| panic!("logger diagnostic did not use its output target:\n{stderr}"));
    assert!(
        emitted < failure,
        "committed logger output must render before the later logger failure:\n{stderr}"
    );
}

#[test]
fn configured_logger_requires_a_unit_result() {
    let dir = unique_temp_dir("glam-conf-log-unit-result");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nconf.log = .r \"not unit\"\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--script.g")
        .arg("language g0\nasm.result = \"ok\"\n")
        .output()
        .expect("failed to run glam");

    assert!(!output.status.success());
    assert_eq!(output.stdout, b"ok");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("configured logger failed")
            && stderr.contains("requires discarded effect results to be unit"),
        "non-unit logger result was not reported:\n{stderr}"
    );
}

#[test]
fn configured_logger_can_finish_when_the_log_stream_closes() {
    let dir = unique_temp_dir("glam-conf-log-status");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nconf.log = .log_status >>= (\\status -> (status == 'closed) =>> .r ())\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--script.g")
        .arg("language g0\nasm.result = \"ok\"\n")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ok");
    assert!(output.stderr.is_empty());
}

#[test]
fn configured_logger_does_not_read_its_own_log_output() {
    let dir = unique_temp_dir("glam-conf-log-separate-output");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nconf.log = (.log 'warn { msg:{ text:\"LOGGER OUTPUT\" } }) =>> .cut (.alt (.read_log >>= (\\_message -> .log 'error { msg:{ text:\"READ OWN OUTPUT\" } })) (.log_status >>= (\\status -> (status == 'closed) =>> .r ())))\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--script.g")
        .arg("language g0\nasm.result = \"ok\"\n")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ok");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("warning: LOGGER OUTPUT"));
    assert!(!stderr.contains("READ OWN OUTPUT"));
}

#[test]
fn configured_logger_drains_unjoined_child_tasks_to_its_output() {
    let dir = unique_temp_dir("glam-conf-log-child-drain");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nconf.log = .refl_task ((.log 'warn { msg:{ text:\"DETACHED CHILD\" } }) =>> .r ()) >>= (\\_task -> .r ())\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--script.g")
        .arg("language g0\nasm.result = \"ok\"\n")
        .output()
        .expect("failed to run glam");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ok");
    assert!(String::from_utf8_lossy(&output.stderr).contains("warning: DETACHED CHILD"));
}

#[test]
fn configured_logger_reports_an_unjoined_child_failure() {
    let dir = unique_temp_dir("glam-conf-log-child-failure");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nconf.log = .refl_task .fail >>= (\\_task -> .r ())\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--script.g")
        .arg("language g0\nasm.result = \"ok\"\n")
        .output()
        .expect("failed to run glam");

    assert!(!output.status.success());
    assert_eq!(output.stdout, b"ok");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("configured logger failed"));
    assert!(
        stderr.contains("task") && stderr.contains("failed"),
        "child failure was not reported:\n{stderr}"
    );
}

#[test]
fn configured_logger_must_observe_stream_closure_when_waiting_for_input() {
    let dir = unique_temp_dir("glam-conf-log-close-deadlock");
    fs::create_dir_all(&dir)
        .unwrap_or_else(|err| panic!("failed to create {}: {err}", dir.display()));
    let config = dir.join("conf.g");
    fs::write(
        &config,
        "language g0\nobject conf.env\nconf.log = .read_log =>> .r ()\n",
    )
    .unwrap_or_else(|err| panic!("failed to write {}: {err}", config.display()));

    let output = glam_command()
        .env("GLAM_CONF", &config)
        .arg("--script.g")
        .arg("language g0\nasm.result = \"ok\"\n")
        .output()
        .expect("failed to run glam");

    assert!(!output.status.success());
    assert_eq!(output.stdout, b"ok");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("configured logger remained blocked after the log stream closed")
    );
}

#[test]
fn failed_reflection_tasks_fail_the_batch_after_writing_the_result() {
    let output = glam_command()
        .arg("--script.g")
        .arg("language g0\nrefl.failure = .fail\nasm.result = \"ok\"\n")
        .output()
        .expect("failed to run glam");

    assert!(!output.status.success());
    assert_eq!(output.stdout, b"ok");
    assert!(String::from_utf8_lossy(&output.stderr).contains("reflection task"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed"));
}

#[test]
fn quiescent_reflection_tasks_report_a_scheduler_deadlock() {
    let output = glam_command()
        .arg("--script.g")
        .arg(
            "language g0\nrefl.deadlock = .get ['heap,'never] >>= (\\_ -> .fail)\nasm.result = \"ok\"\n",
        )
        .output()
        .expect("failed to run glam");

    assert!(!output.status.success());
    assert_eq!(output.stdout, b"ok");
    assert!(String::from_utf8_lossy(&output.stderr).contains("reflection scheduler deadlocked"));
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

fn glam_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_glam"));
    command.env("GLAM_CONF", "samples/config/unit_tests.g");
    command
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after Unix epoch")
        .as_nanos();
    env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
}
