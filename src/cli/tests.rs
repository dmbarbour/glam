use std::ffi::OsString;
use std::fs;
use std::path::Path;

use crate::Assembler;

use super::{
    ParseVerbosity, TopLevelCommand, dispatch_bootstrap, expand_configured,
    format_configured_arguments,
};

fn dispatch(arguments: &[&str]) -> Result<TopLevelCommand, String> {
    dispatch_bootstrap(arguments.iter().map(OsString::from)).map_err(|error| error.to_string())
}

#[test]
fn empty_command_displays_help() {
    assert_eq!(dispatch(&[]), Ok(TopLevelCommand::Help));
}

#[test]
fn assembly_plan_preserves_input_and_argument_order() {
    let command = dispatch(&[
        "--file",
        "first.g",
        "--script.g",
        "body",
        "--refl",
        "explain",
        "--",
        "one",
        "two",
    ])
    .expect("assembly command should parse");
    let TopLevelCommand::Assembly(plan) = command else {
        panic!("expected an assembly plan");
    };
    assert_eq!(plan.process_args(), plan.cli_arguments().args());
    let parts = plan.into_parts();
    assert_eq!(parts.inputs.len(), 2);
    assert_eq!(parts.reflection_args, [OsString::from("explain")]);
    assert_eq!(
        parts.assembly_args,
        [OsString::from("one"), OsString::from("two")]
    );
}

#[test]
fn file_paths_are_not_required_to_be_utf8_text() {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;

        let path = OsString::from_vec(vec![b'f', 0xFF]);
        let command = dispatch_bootstrap([OsString::from("--file"), path.clone()])
            .expect("opaque file path should parse");
        let TopLevelCommand::Assembly(plan) = command else {
            panic!("expected an assembly plan");
        };
        let parts = plan.into_parts();
        assert_eq!(parts.inputs, [crate::ModuleInput::file(path)]);
    }
}

#[test]
fn assembly_arguments_are_not_required_to_be_utf8_text() {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;

        let argument = OsString::from_vec(vec![b'a', 0xFF]);
        let command = dispatch_bootstrap([
            OsString::from("--file"),
            OsString::from("input.g"),
            OsString::from("--"),
            argument.clone(),
        ])
        .expect("opaque assembly argument should parse");
        let TopLevelCommand::Assembly(plan) = command else {
            panic!("expected an assembly plan");
        };
        assert_eq!(plan.cli_arguments().args().last(), Some(&argument));
        assert_eq!(plan.into_parts().assembly_args, [argument]);
    }
}

#[test]
fn parse_is_a_standalone_typed_command() {
    assert_eq!(
        dispatch(&["--parse", "input.g", "--verbose"]),
        Ok(TopLevelCommand::InspectGSource {
            path: Path::new("input.g").to_owned(),
            verbosity: ParseVerbosity::Verbose,
        })
    );
    assert_eq!(
        dispatch(&["--parse", "input.g", "--workers", "1"]),
        Err("unknown `--parse` option `--workers`".to_owned())
    );
}

#[test]
fn manifest_check_has_clear_arity() {
    assert_eq!(
        dispatch(&["--check_manifest", "inputs.manifest", "--quiet"]),
        Ok(TopLevelCommand::CheckManifest {
            path: Path::new("inputs.manifest").to_owned(),
            quiet: true,
        })
    );
    assert_eq!(
        dispatch(&["--check_manifest", "inputs.manifest", "other.manifest"]),
        Err("`--check_manifest` accepts only a manifest path and optional `--quiet`".to_owned())
    );
}

#[test]
fn duplicate_singleton_assembly_options_are_rejected() {
    assert_eq!(
        dispatch(&[
            "--manifest",
            "one",
            "--manifest",
            "two",
            "--file",
            "input.g"
        ]),
        Err("`--manifest` may be specified only once".to_owned())
    );
    assert_eq!(
        dispatch(&["--workers", "1", "--workers", "2", "--file", "input.g"]),
        Err("`--workers` may be specified only once".to_owned())
    );
}

#[test]
fn a_bare_first_argument_is_deferred_to_configured_cli() {
    let command = dispatch(&["build", "input.g"]).expect("bare command should dispatch");
    let TopLevelCommand::ConfiguredCli(arguments) = command else {
        panic!("expected configured CLI dispatch");
    };
    assert_eq!(
        arguments.args(),
        [OsString::from("build"), OsString::from("input.g")]
    );
}

#[test]
fn configured_inspection_has_an_explicit_bare_arity() {
    let command = dispatch(&["--parse_cli", "build", "input.g"])
        .expect("configured inspection should dispatch");
    let TopLevelCommand::InspectConfiguredCli {
        arguments,
        nul_terminated,
    } = command
    else {
        panic!("expected configured CLI inspection");
    };
    assert!(!nul_terminated);
    assert_eq!(arguments.args(), ["build", "input.g"]);
    assert_eq!(
        dispatch(&["--parse_cli.0", "--file"]),
        Err("configured CLI inspection requires a bare first argument".to_owned())
    );
}

fn configuration(source: &str) -> (Assembler, crate::Value) {
    let assembler = Assembler::new();
    let module = assembler
        .module(["configuration"])
        .script("g", source)
        .build()
        .expect("CLI configuration should compile");
    (assembler, module.into_value())
}

fn configured_arguments(arguments: &[&str]) -> super::CliArguments {
    let TopLevelCommand::ConfiguredCli(arguments) =
        dispatch(arguments).expect("bare arguments should dispatch")
    else {
        panic!("expected configured CLI dispatch");
    };
    arguments
}

#[test]
fn configured_cli_builds_one_canonical_command_plan() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli =\n    .read.keyword \"build\" =>>\n    .read.text \"script\" >>= (\\body ->\n    .write.script \"g\" body =>>\n    .write.refl_arg \"detail\" =>>\n    .write.assembly_arg \"argument\" =>>\n    .write.worker_count 2 =>>\n    .read.end)\n",
    );
    let expansion = expand_configured(
        &assembler,
        &configuration,
        configured_arguments(&["build", "asm.result = \"ok\""]),
    )
    .expect("configured command should expand");
    assert!(expansion.diagnostics().is_empty());
    assert_eq!(
        expansion.plan().process_args(),
        [
            "--script.g",
            "asm.result = \"ok\"",
            "--refl",
            "detail",
            "--workers",
            "2",
            "--",
            "argument",
        ]
    );
}

#[test]
fn configured_cli_reads_its_immutable_environment() {
    let assembler = Assembler::builder()
        .reflection_environment(|_| {
            Ok(crate::Value::record([(
                "process",
                crate::Value::record([(
                    "cli",
                    crate::Value::record([(
                        "args",
                        crate::Value::list([crate::Value::text("build")]),
                    )]),
                )]),
            )]))
        })
        .expect("CLI environment should build")
        .build()
        .expect("assembler should build");
    let module = assembler
        .module(["configuration"])
        .script(
            "g",
            "language g0\nimport 'std\nconf.cli = .env ['process,'cli,'args] >>= (\\args -> (args == [\"build\"]) =>> .read.keyword \"build\" =>> .read.end =>> .write.script \"g\" \"asm.result = 1\")\n",
        )
        .build()
        .expect("CLI configuration should compile");
    let expansion = expand_configured(&assembler, module.value(), configured_arguments(&["build"]))
        .expect("CLI should observe its immutable environment");
    assert_eq!(
        expansion.plan().process_args(),
        ["--script.g", "asm.result = 1"]
    );
}

#[test]
fn configured_cli_cannot_observe_canonical_arguments_before_selection() {
    let mut resolver = None;
    let assembler = Assembler::builder()
        .reflection_environment(|environment| {
            let (process_args, process_resolver) = environment.promise("canonical arguments");
            resolver = Some(process_resolver);
            Ok(crate::Value::record([(
                "process",
                crate::Value::record([("args", process_args)]),
            )]))
        })
        .expect("CLI environment should build")
        .build()
        .expect("assembler should build");
    let module = assembler
        .module(["configuration"])
        .script(
            "g",
            "language g0\nimport 'std\nconf.cli = .env ['process,'args] >>= (\\args -> (args == [\"canonical\"]) =>> .read.keyword \"build\" =>> .read.end =>> .write.script \"g\" \"asm.result = 1\")\n",
        )
        .build()
        .expect("CLI configuration should compile");
    let arguments = configured_arguments(&["build"]);

    let error = expand_configured(&assembler, module.value(), arguments.clone())
        .expect_err("canonical arguments must remain unresolved during selection");
    assert!(error.to_string().contains("configured CLI became blocked"));

    resolver
        .take()
        .expect("resolver should remain available")
        .resolve(crate::Value::list([crate::Value::text("canonical")]))
        .expect("canonical argument promise should resolve");
    if let Err(error) = expand_configured(&assembler, module.value(), arguments) {
        panic!("resolved canonical arguments should unblock a new search: {error}");
    }
}

#[test]
fn configured_cli_rejects_nonunit_unconsumed_and_ambiguous_results() {
    let cases = [
        ("conf.cli = .r 1", "configured `conf.cli` must return unit"),
        (
            "conf.cli = .write.script \"g\" \"asm.result = 1\"",
            "left 1 command-line argument(s) unconsumed",
        ),
        (
            "conf.cli = .read.keyword \"build\" =>> .read.end =>> .alt (.write.script \"g\" \"asm.result = 1\") (.write.script \"g\" \"asm.result = 2\")",
            "more than one distinct command",
        ),
    ];
    for (effect, expected) in cases {
        let source = format!("language g0\nimport 'std\n{effect}\n");
        let (assembler, configuration) = configuration(&source);
        let error = expand_configured(&assembler, &configuration, configured_arguments(&["build"]))
            .expect_err("invalid configured command should fail");
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn missing_configured_cli_behaves_like_an_explicit_failure() {
    let arguments = configured_arguments(&["build"]);
    let (missing_assembler, missing_configuration) =
        configuration("language g0\nobject conf.env\n");
    let missing = expand_configured(
        &missing_assembler,
        &missing_configuration,
        arguments.clone(),
    )
    .expect_err("missing conf.cli should not match");
    let (failed_assembler, failed_configuration) =
        configuration("language g0\nimport 'std\nconf.cli = .fail\n");
    let failed = expand_configured(&failed_assembler, &failed_configuration, arguments)
        .expect_err("explicit failure should not match");

    assert_eq!(missing.to_string(), failed.to_string());
}

#[test]
fn invalid_alternative_does_not_veto_a_valid_configured_plan() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.keyword \"build\" =>> .read.end =>> .alt (.r 1) (.write.script \"g\" \"asm.result = 1\")\n",
    );
    let expansion = expand_configured(&assembler, &configuration, configured_arguments(&["build"]))
        .expect("valid alternative should win over invalid parse evidence");
    assert_eq!(
        expansion.plan().process_args(),
        ["--script.g", "asm.result = 1"]
    );
}

#[test]
fn configured_cli_api_omits_shared_heap_and_task_capabilities() {
    for effect in [".heap.get []", ".task.status 1"] {
        let source = format!(
            "language g0\nimport 'std\nconf.cli = {effect} =>> .write.script \"g\" \"asm.result = 1\"\n"
        );
        let (assembler, configuration) = configuration(&source);
        let error = expand_configured(&assembler, &configuration, configured_arguments(&["build"]))
            .expect_err("CLI effects must not expose shared state or task capabilities");
        assert!(error.to_string().contains("configured CLI failed"));
    }
}

#[test]
fn configured_cli_path_handles_are_invocation_scoped_writer_inputs() {
    let path = std::env::temp_dir().join(format!(
        "glam-cli-path-handle-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    fs::write(&path, "language g0\nasm.result = 1\n")
        .expect("temporary CLI input should be written");
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.keyword \"build\" =>> .read.path 'file 'r >>= (\\path -> .read.end =>> .write.file path)\n",
    );
    let arguments =
        super::CliArguments::new([OsString::from("build"), path.as_os_str().to_owned()].into());
    let expansion = expand_configured(&assembler, &configuration, arguments)
        .expect("readable file path should produce an input edit");
    assert_eq!(
        expansion.plan().process_args(),
        [OsString::from("--file"), path.as_os_str().to_owned()]
    );
    fs::remove_file(path).expect("temporary CLI input should be removed");
}

#[test]
fn configured_cli_returns_only_selected_branch_diagnostics() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.keyword \"build\" =>> .read.end =>> .log 'warn { msg:{ text:\"selected\" } } =>> .write.script \"g\" \"asm.result = 1\"\n",
    );
    let expansion = expand_configured(&assembler, &configuration, configured_arguments(&["build"]))
        .expect("logged configured command should expand");
    assert_eq!(expansion.diagnostics().len(), 1);
    assert_eq!(expansion.diagnostics()[0].message(), "selected");
    assert_eq!(assembler.diagnostic_bus().counts().total(), 0);
}

#[test]
fn configured_argument_output_preserves_boundaries() {
    let arguments = [OsString::from("one"), OsString::from("two")];
    assert_eq!(
        format_configured_arguments(&arguments, false),
        b"one\ntwo\n"
    );
    assert_eq!(format_configured_arguments(&arguments, true), b"one\0two\0");
}
