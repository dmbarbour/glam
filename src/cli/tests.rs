use std::ffi::OsString;
use std::fs;
use std::path::Path;

use crate::Assembler;

use super::{
    CompletionKind, CompletionRequest, CompletionRoute, ParseVerbosity, TopLevelCommand,
    complete_basic, complete_configured, dispatch_bootstrap, expand_configured,
    format_completion_replacements, format_configured_arguments, route_completion,
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

#[test]
fn completion_protocol_has_exact_versioned_arity() {
    let command = dispatch(&[
        "--completions",
        "v0",
        "active",
        "1",
        "1",
        "build",
        "bu",
        "ndle",
        "tail",
    ])
    .expect("active completion request should dispatch");
    let TopLevelCommand::Complete(request) = command else {
        panic!("expected a completion request");
    };
    assert_eq!(request.arguments_before(), [OsString::from("build")]);
    let active = request
        .active_argument()
        .expect("active mode should retain its active argument");
    assert_eq!(active.prefix(), "bu");
    assert_eq!(active.suffix(), "ndle");
    assert_eq!(request.arguments_after(), [OsString::from("tail")]);
    assert_eq!(
        request.arguments().as_ref(),
        [
            OsString::from("build"),
            OsString::from("bundle"),
            OsString::from("tail")
        ]
    );

    let command = dispatch(&["--completions", "v0", "absent", "0", "0"])
        .expect("absent completion request should dispatch");
    let TopLevelCommand::Complete(request) = command else {
        panic!("expected an absent completion request");
    };
    assert!(request.active_argument().is_none());
    assert!(request.arguments().is_empty());

    assert!(dispatch(&["--completions", "v1", "absent", "0", "0"]).is_err());
    assert!(dispatch(&["--completions", "v0", "missing", "0", "0"]).is_err());
    assert!(dispatch(&["--completions", "v0", "absent", "00", "0"]).is_err());
    assert!(dispatch(&["--completions", "v0", "active", "0", "0", "only-prefix"]).is_err());
    assert!(dispatch(&["--completions", "v0", "absent", "0", "0", "extra"]).is_err());
}

#[test]
fn completion_routing_preserves_missing_empty_and_configured_boundaries() {
    assert!(matches!(
        route_completion(CompletionRequest::without_active([], [])),
        CompletionRoute::Basic(_)
    ));
    assert!(matches!(
        route_completion(CompletionRequest::with_active([], "", "", [])),
        CompletionRoute::Configured(_)
    ));
    assert!(matches!(
        route_completion(CompletionRequest::with_active([], "--pa", "", [])),
        CompletionRoute::Basic(_)
    ));

    let CompletionRoute::Configured(rebased) = route_completion(CompletionRequest::with_active(
        [OsString::from("--parse_cli")],
        "",
        "",
        [],
    )) else {
        panic!("a complete inspection prefix should delegate to configured completion");
    };
    assert!(rebased.arguments_before().is_empty());
    assert_eq!(
        rebased
            .active_argument()
            .expect("the empty configured argument should remain present")
            .value(),
        ""
    );
}

#[test]
fn basic_completion_uses_whole_argument_replacements_and_minimal_output() {
    let completion = complete_basic(&CompletionRequest::without_active([], []));
    let root_replacements = replacements(&completion);
    assert!(root_replacements.contains(&"--file".to_owned()));
    assert!(root_replacements.contains(&"--completion_script".to_owned()));

    let completion = complete_basic(&CompletionRequest::with_active([], "--par", "se", []));
    assert_eq!(replacements(&completion), ["--parse"]);
    assert_eq!(format_completion_replacements(&completion), b"--parse\0");
}

#[cfg(unix)]
#[test]
fn completion_output_preserves_non_utf8_path_replacements() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let root = std::env::temp_dir().join(format!(
        "glam-cli-opaque-completion-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    fs::create_dir_all(&root).expect("opaque completion folder should be created");
    let mut path_bytes = root.as_os_str().as_bytes().to_vec();
    path_bytes.push(b'/');
    path_bytes.extend_from_slice(b"f\xff");
    let path = OsString::from_vec(path_bytes);
    fs::write(Path::new(&path), "source").expect("opaque completion file should be created");
    let prefix = root.join("f").into_os_string();

    let completion = complete_basic(&CompletionRequest::with_active(
        [OsString::from("--file")],
        prefix,
        "",
        [],
    ));
    let mut expected = path.as_os_str().as_bytes().to_vec();
    expected.push(0);
    assert_eq!(format_completion_replacements(&completion), expected);

    fs::remove_dir_all(root).expect("opaque completion folder should be removed");
}

#[test]
fn completion_script_dispatch_has_exact_arity() {
    let command = dispatch(&["--completion_script", "bash"])
        .expect("completion script request should dispatch");
    let TopLevelCommand::CompletionScript {
        name,
        cli_arguments,
    } = command
    else {
        panic!("expected a completion script request");
    };
    assert_eq!(name, "bash");
    assert_eq!(cli_arguments.args(), ["--completion_script", "bash"]);
    assert!(dispatch(&["--completion_script"]).is_err());
    assert!(dispatch(&["--completion_script", "bash", "extra"]).is_err());
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
fn configured_cli_parses_structured_utf8_tokens() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli =\n    .read.token \"script option\" (.token.text \"--script.\" =>> .token.regex \"[A-Za-z0-9_+-]+\") >>= (\\extension ->\n    .read.text \"script body\" >>= (\\body ->\n    .read.end =>> .write.script extension body))\n",
    );
    let expansion = expand_configured(
        &assembler,
        &configuration,
        super::CliArguments::new(
            ["--script.g", "asm.result = 7"]
                .into_iter()
                .map(OsString::from)
                .collect(),
        ),
    )
    .expect("nested token parser should return its regex span");
    assert_eq!(
        expansion.plan().process_args(),
        ["--script.g", "asm.result = 7"]
    );

    let error = expand_configured(
        &assembler,
        &configuration,
        super::CliArguments::new(
            ["--script.g!", "asm.result = 7"]
                .into_iter()
                .map(OsString::from)
                .collect(),
        ),
    )
    .expect_err("nested parser must consume the complete token");
    assert!(error.to_string().contains("did not match"), "{error}");
}

#[test]
fn configured_cli_token_alternatives_reenter_outer_search() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.token \"choice\" (.alt (.token.text \"x\" =>> .r \"one\") (.token.text \"x\" =>> .r \"two\")) >>= (\\choice -> .read.end =>> .write.script \"g\" choice)\n",
    );
    let error = expand_configured(&assembler, &configuration, configured_arguments(&["x"]))
        .expect_err("distinct nested results should remain ambiguous outside the token parser");
    assert!(
        error.to_string().contains("more than one distinct command"),
        "{error}"
    );
}

#[test]
fn configured_token_parser_returns_user_structured_values() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.token \"field\" (.token.regex \"[A-Za-z]+\" >>= (\\key -> .token.text \"=\" =>> .token.regex \"[0-9]+\" >>= (\\value -> .r {key:key, value:value}))) >>= (\\field -> (field == {key:\"answer\", value:\"42\"}) =>> .read.end =>> .write.script \"g\" \"ok\")\n",
    );
    let expansion = expand_configured(
        &assembler,
        &configuration,
        configured_arguments(&["answer=42"]),
    )
    .expect("nested token results should retain user-constructed structure");
    assert_eq!(expansion.plan().process_args(), ["--script.g", "ok"]);
}

fn replacements(completion: &super::CliCompletion) -> Vec<String> {
    completion
        .candidates()
        .iter()
        .map(|candidate| candidate.replacement().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn configured_completion_combines_only_furthest_viable_keywords() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .alt (.read.keyword \"build\" =>> .read.keyword \"x\" =>> .read.end =>> .write.script \"g\" \"build\") (.alt (.read.keyword \"bundle\" =>> .read.keyword \"y\" =>> .read.end =>> .write.script \"g\" \"bundle\") (.read.keyword \"other\" =>> .read.end =>> .write.script \"g\" \"other\"))\n",
    );
    let completion = complete_configured(
        &assembler,
        &configuration,
        CompletionRequest::with_active([], "bu", "", []),
    )
    .expect("configured completion should run");
    assert_eq!(replacements(&completion), ["build", "bundle"]);
    assert_eq!(completion.candidates()[0].kind(), CompletionKind::Keyword);

    let completion = complete_configured(
        &assembler,
        &configuration,
        CompletionRequest::with_active([], "bu", "", [OsString::from("x")]),
    )
    .expect("later arguments should validate otherwise local candidates");
    assert_eq!(replacements(&completion), ["build"]);

    let completion = complete_configured(
        &assembler,
        &configuration,
        CompletionRequest::with_active([], "bu", "ld", [OsString::from("x")]),
    )
    .expect("the unchanged suffix should participate in completion");
    assert_eq!(replacements(&completion), ["build"]);
}

#[test]
fn explained_cli_cases_render_furthest_parse_context() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .case {usage:\"build FILE\", summary:\"Assemble one file\"} (.read.keyword \"build\" =>> .read.text \"source file\" =>> .read.end =>> .write.script \"g\" \"ok\")\n",
    );
    let error = expand_configured(
        &assembler,
        &configuration,
        configured_arguments(&["bundle"]),
    )
    .expect_err("a mismatched explained case should fail");
    assert!(error.to_string().contains("expected `build`"), "{error}");
    assert!(
        error
            .to_string()
            .contains("while parsing: build FILE — Assemble one file"),
        "{error}"
    );
    assert_eq!(error.explanations().len(), 1);
    assert_eq!(
        assembler
            .get(error.explanations()[0].value(), "usage")
            .expect("usage should be readable")
            .as_binary(),
        Some(b"build FILE".as_slice())
    );
    assert!(
        !assembler
            .get(error.diagnostic().emission(), "cli.cases")
            .expect("rich CLI diagnostic should retain case values")
            .is_undefined()
    );
}

#[test]
fn explained_cli_cases_are_scoped_and_remain_lazy_on_success() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .case 42 (.read.keyword \"build\") =>> .read.keyword \"tail\" =>> .read.end =>> .write.script \"g\" \"ok\"\n",
    );
    let expansion = expand_configured(
        &assembler,
        &configuration,
        configured_arguments(&["build", "tail"]),
    )
    .expect("ordinary command construction must not inspect case explanations");
    assert_eq!(expansion.plan().process_args(), ["--script.g", "ok"]);

    let error = expand_configured(
        &assembler,
        &configuration,
        configured_arguments(&["build", "wrong"]),
    )
    .expect_err("a reader after the case should fail");
    assert!(error.to_string().contains("expected `tail`"), "{error}");
    assert!(!error.to_string().contains("while parsing"), "{error}");
    assert!(error.explanations().is_empty());
}

#[test]
fn nested_cli_cases_attach_context_to_completion_candidates() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .case {summary:\"Build commands\"} (.case {summary:\"Build one input\"} (.read.keyword \"build\" =>> .read.end =>> .write.script \"g\" \"ok\"))\n",
    );
    let completion = complete_configured(
        &assembler,
        &configuration,
        CompletionRequest::with_active([], "bu", "", []),
    )
    .expect("explained completion should run");
    assert_eq!(replacements(&completion), ["build"]);
    let explanations = completion.candidates()[0].explanations();
    assert_eq!(explanations.len(), 2);
    assert_eq!(
        assembler
            .get(explanations[0].value(), "summary")
            .expect("outer summary should be readable")
            .as_binary(),
        Some(b"Build commands".as_slice())
    );
    assert_eq!(
        assembler
            .get(explanations[1].value(), "summary")
            .expect("inner summary should be readable")
            .as_binary(),
        Some(b"Build one input".as_slice())
    );
}

#[test]
fn ambiguous_explained_cli_cases_name_the_competing_commands() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .alt (.case {summary:\"First build form\"} (.read.keyword \"build\" =>> .read.end =>> .write.script \"g\" \"one\")) (.case {summary:\"Second build form\"} (.read.keyword \"build\" =>> .read.end =>> .write.script \"g\" \"two\"))\n",
    );
    let error = expand_configured(&assembler, &configuration, configured_arguments(&["build"]))
        .expect_err("distinct explained plans should remain ambiguous");
    assert!(error.to_string().contains("First build form"), "{error}");
    assert!(error.to_string().contains("Second build form"), "{error}");
    assert_eq!(error.explanations().len(), 2);
}

#[test]
fn explained_cli_cases_attach_context_to_completion_expectations() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .case {summary:\"Name the output\"} (.read.text \"output name\" =>> .read.end =>> .write.script \"g\" \"ok\")\n",
    );
    let completion = complete_configured(
        &assembler,
        &configuration,
        CompletionRequest::with_active([], "", "", []),
    )
    .expect("explained expectation completion should run");
    let expectation = completion
        .expectations()
        .iter()
        .find(|expectation| expectation.label() == "output name")
        .expect("text reader should report its expectation");
    assert_eq!(expectation.explanations().len(), 1);
    assert_eq!(
        assembler
            .get(expectation.explanations()[0].value(), "summary")
            .expect("summary should be readable")
            .as_binary(),
        Some(b"Name the output".as_slice())
    );
}

#[test]
fn configured_completion_derives_literals_but_not_regex_languages() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.token \"script option\" (.token.text \"--script.\" =>> .token.regex \"[A-Za-z]+\") >>= (\\_ -> .read.text \"body\" =>> .read.end =>> .write.script \"g\" \"body\")\n",
    );
    let completion = complete_configured(
        &assembler,
        &configuration,
        CompletionRequest::with_active([], "--scr", "", []),
    )
    .expect("literal token completion should run");
    assert_eq!(replacements(&completion), ["--script."]);

    let completion = complete_configured(
        &assembler,
        &configuration,
        CompletionRequest::with_active([], "--script.", "", []),
    )
    .expect("regex frontier should remain a non-enumerated expectation");
    assert!(completion.candidates().is_empty());
    assert!(
        completion
            .expectations()
            .iter()
            .any(|expectation| expectation.label() == "matching text")
    );
}

#[test]
fn configured_completion_filters_filesystem_candidates_by_path_kind() {
    let root = std::env::temp_dir().join(format!(
        "glam-cli-completion-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    fs::create_dir_all(root.join("folder")).expect("completion folder should be created");
    fs::write(root.join("file.g"), "source").expect("completion file should be created");
    let prefix = root.join("fi").into_os_string();

    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.keyword \"open\" =>> .read.path 'file 'r >>= (\\_ -> .read.end =>> .write.script \"g\" \"ok\")\n",
    );
    let completion = complete_configured(
        &assembler,
        &configuration,
        CompletionRequest::with_active([OsString::from("open")], prefix, "", []),
    )
    .expect("filesystem completion should run");
    assert_eq!(
        replacements(&completion),
        [root.join("file.g").display().to_string()]
    );
    assert_eq!(completion.candidates()[0].kind(), CompletionKind::File);

    fs::remove_dir_all(root).expect("completion fixture should be removed");
}

#[test]
fn missing_configured_cli_has_no_completion_candidates() {
    let (assembler, configuration) = configuration("language g0\nobject conf.env\n");
    let completion = complete_configured(
        &assembler,
        &configuration,
        CompletionRequest::with_active([], "build", "", []),
    )
    .expect("undefined conf.cli should behave like failure during completion");
    assert!(completion.candidates().is_empty());
}

#[test]
fn token_regex_rejects_captures_and_any_consumes_one_unicode_scalar() {
    let (assembler, unicode_configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.token \"unicode pair\" (.token.any >>= (\\a -> .token.any >>= (\\_ -> .token.end =>> .r a))) >>= (\\body -> .read.end =>> .write.script \"g\" body)\n",
    );
    let expansion = expand_configured(
        &assembler,
        &unicode_configuration,
        configured_arguments(&["λx"]),
    )
    .expect("token any should consume Unicode scalar values rather than bytes");
    assert_eq!(expansion.plan().process_args(), ["--script.g", "λ"]);

    let (assembler, regex_configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.token \"capture-free text\" (.token.regex \"(x)\") =>> .read.end =>> .write.script \"g\" \"ok\"\n",
    );
    let error = expand_configured(
        &assembler,
        &regex_configuration,
        configured_arguments(&["x"]),
    )
    .expect_err("capturing token regex should be rejected");
    assert!(
        error.to_string().contains("does not permit capture"),
        "{error}"
    );
}

#[test]
fn token_regex_is_anchored_capture_free_and_leftmost_first() {
    let source = |pattern: &str| {
        format!(
            "language g0\nimport 'std\nconf.cli = .read.token \"choice\" (.token.regex \"{pattern}\") >>= (\\_ -> .read.end =>> .write.script \"g\" \"ok\")\n"
        )
    };
    let (assembler, longest_configuration) = configuration(&source("(?:foofoo|foo)"));
    expand_configured(
        &assembler,
        &longest_configuration,
        configured_arguments(&["foofoo"]),
    )
    .expect("non-capturing groups should be accepted");

    let (assembler, shortest_configuration) = configuration(&source("(?:foo|foofoo)"));
    expand_configured(
        &assembler,
        &shortest_configuration,
        configured_arguments(&["foofoo"]),
    )
    .expect_err("leftmost-first matching should leave the longer suffix unconsumed");

    let (assembler, anchored_configuration) = configuration(&source("foo"));
    expand_configured(
        &assembler,
        &anchored_configuration,
        configured_arguments(&["xfoo"]),
    )
    .expect_err("token regex must be anchored at the current cursor");
}

#[test]
fn token_parser_rejects_outer_cli_and_host_effects() {
    let effects = [
        ".env []",
        ".log 'info {msg:{text:\"not emitted\"}}",
        ".read.text \"nested argument\"",
        ".write.script \"g\" \"not emitted\"",
        ".heap.get []",
        ".task.status 0",
    ];
    for effect in effects {
        let source = format!(
            "language g0\nimport 'std\nconf.cli = .read.token \"isolated token\" (.token.text \"x\" =>> ({effect})) >>= (\\_ -> .read.end =>> .write.script \"g\" \"ok\")\n"
        );
        let (assembler, configuration) = configuration(&source);
        let error = expand_configured(&assembler, &configuration, configured_arguments(&["x"]))
            .expect_err("token parsers must not inherit host or outer CLI effects");
        assert!(
            error.to_string().contains("configured CLI failed"),
            "unexpected token effect result for `{effect}`: {error}"
        );
    }
}

#[test]
fn token_task_local_state_is_isolated_from_outer_cli_state() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .set ['scope] \"outer\" =>> .read.token \"stateful token\" (.token.text \"x\" =>> .get ['scope] >>= (\\before -> (before == {}) =>> .set ['scope] \"inner\" =>> .get ['scope])) >>= (\\inside -> (inside == \"inner\") =>> .get ['scope] >>= (\\outside -> (outside == \"outer\") =>> .read.end =>> .write.script \"g\" \"ok\"))\n",
    );
    let expansion = expand_configured(&assembler, &configuration, configured_arguments(&["x"]))
        .expect("token and outer CLI local states should not observe each other");
    assert_eq!(expansion.plan().process_args(), ["--script.g", "ok"]);
}

#[test]
fn each_token_parse_starts_with_fresh_task_local_state() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.token \"first token\" (.token.text \"x\" =>> .set ['scope] \"first\") =>> .read.token \"second token\" (.token.text \"y\" =>> .get ['scope] >>= (\\state -> (state == {}) =>> .r ())) =>> .read.end =>> .write.script \"g\" \"ok\"\n",
    );
    let expansion = expand_configured(
        &assembler,
        &configuration,
        configured_arguments(&["x", "y"]),
    )
    .expect("separate token parsers should not share task-local state");
    assert_eq!(expansion.plan().process_args(), ["--script.g", "ok"]);
}

#[test]
fn failed_token_alternatives_discard_their_task_local_state() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.token \"choice\" (.token.text \"x\" =>> .alt (.set ['leak] \"bad\" =>> .fail) (.get ['leak] >>= (\\state -> (state == {}) =>> .r ()))) =>> .read.end =>> .write.script \"g\" \"ok\"\n",
    );
    let expansion = expand_configured(&assembler, &configuration, configured_arguments(&["x"]))
        .expect("failed token alternatives should not leak local writes");
    assert_eq!(expansion.plan().process_args(), ["--script.g", "ok"]);
}

#[test]
fn configured_parse_errors_report_the_furthest_expectation() {
    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .alt (.read.keyword \"build\" =>> .read.token \"script option\" (.token.text \"--script.\" =>> .token.regex \"[A-Za-z]+\") =>> .read.end =>> .write.script \"g\" \"ok\") (.read.keyword \"bundle\" =>> .read.end =>> .write.script \"g\" \"other\")\n",
    );
    let error = expand_configured(
        &assembler,
        &configuration,
        configured_arguments(&["build", "--script."]),
    )
    .expect_err("incomplete regex span should fail at its token frontier");
    assert!(error.to_string().contains("argument 2, byte 9"), "{error}");
    assert!(error.to_string().contains("matching text"), "{error}");
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
fn configured_path_readers_distinguish_kind_and_access_intent() {
    let root = std::env::temp_dir().join(format!(
        "glam-cli-path-matrix-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos()
    ));
    let file = root.join("input.g");
    let readonly_file = root.join("readonly.g");
    let folder = root.join("folder");
    let missing = root.join("new-target");
    let missing_parent = root.join("absent").join("target");
    fs::create_dir_all(&folder).expect("path fixture folder should be created");
    fs::write(&file, "source").expect("path fixture file should be created");
    fs::write(&readonly_file, "source").expect("read-only path fixture should be created");
    let original_permissions = fs::metadata(&readonly_file)
        .expect("read-only fixture should have metadata")
        .permissions();
    let mut readonly_permissions = original_permissions.clone();
    readonly_permissions.set_readonly(true);
    fs::set_permissions(&readonly_file, readonly_permissions)
        .expect("read-only fixture permissions should be installed");

    let cases = [
        ("file", "r", file.as_path(), true),
        ("file", "r", folder.as_path(), false),
        ("folder", "r", folder.as_path(), true),
        ("folder", "r", file.as_path(), false),
        ("file", "w", file.as_path(), true),
        ("file", "r", readonly_file.as_path(), true),
        ("file", "w", readonly_file.as_path(), false),
        ("file", "w", folder.as_path(), false),
        ("folder", "w", folder.as_path(), true),
        ("folder", "w", file.as_path(), false),
        ("file", "w", missing.as_path(), true),
        ("folder", "w", missing.as_path(), true),
        ("any", "r", file.as_path(), true),
        ("any", "w", missing.as_path(), true),
        ("any", "w", missing_parent.as_path(), false),
    ];
    for (kind, access, path, should_match) in cases {
        let source = format!(
            "language g0\nimport 'std\nconf.cli = .read.keyword \"path\" =>> .read.path '{kind} '{access} >>= (\\_ -> .read.end =>> .write.script \"g\" \"ok\")\n"
        );
        let (assembler, configuration) = configuration(&source);
        let result = expand_configured(
            &assembler,
            &configuration,
            super::CliArguments::new([OsString::from("path"), path.as_os_str().to_owned()].into()),
        );
        assert_eq!(
            result.is_ok(),
            should_match,
            "unexpected path result for {kind}/{access}: {}",
            path.display()
        );
    }

    let (assembler, configuration) = configuration(
        "language g0\nimport 'std\nconf.cli = .read.keyword \"path\" =>> .read.path 'folder 'r >>= (\\path -> .read.end =>> .write.file path)\n",
    );
    let error = expand_configured(
        &assembler,
        &configuration,
        super::CliArguments::new([OsString::from("path"), folder.as_os_str().to_owned()].into()),
    )
    .expect_err("file writer must reject a folder path handle");
    assert!(
        error.to_string().contains("requires a readable file path"),
        "{error}"
    );

    fs::set_permissions(&readonly_file, original_permissions)
        .expect("read-only fixture should be made removable");
    fs::remove_dir_all(root).expect("path fixture should be removed");
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
    let arguments = [
        OsString::from("one"),
        OsString::from("two\ncontinued"),
        OsString::from(""),
    ];
    assert_eq!(
        format_configured_arguments(&arguments, false),
        b"[1]: one\n[2]: two\n  continued\n[3]: \n"
    );
    assert_eq!(
        format_configured_arguments(&arguments, true),
        b"one\0two\ncontinued\0\0"
    );
}
