use std::ffi::OsString;
use std::path::Path;

use super::{ParseVerbosity, TopLevelCommand, dispatch_bootstrap};

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
    assert_eq!(
        plan.cli_arguments().user_args(),
        plan.cli_arguments().args()
    );
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
        arguments.user_args(),
        [OsString::from("build"), OsString::from("input.g")]
    );
    assert_eq!(arguments.user_args(), arguments.args());
}
