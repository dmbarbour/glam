use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Arc;

use crate::ModuleInput;

use super::CompletionRequest;
use super::model::{
    CliArguments, CliError, CommandEdit, CommandPlanBuilder, ParseVerbosity, TopLevelCommand,
};

pub fn dispatch_bootstrap(
    user_args: impl IntoIterator<Item = OsString>,
) -> Result<TopLevelCommand, CliError> {
    let user_args = Arc::<[OsString]>::from(user_args.into_iter().collect::<Vec<_>>());
    let cli_arguments = CliArguments::new(user_args.clone());
    let Some(first) = user_args.first() else {
        return Ok(TopLevelCommand::Help);
    };

    match option(first) {
        Some("-h" | "--help") => Ok(TopLevelCommand::Help),
        Some("-V" | "--version") => Ok(TopLevelCommand::Version),
        Some("--parse") => parse_inspection(&user_args[1..]),
        Some("--parse_cli") => parse_configured_inspection(&user_args[1..], false),
        Some("--parse_cli.0") => parse_configured_inspection(&user_args[1..], true),
        Some("--completions") => parse_completion_request(&user_args[1..]),
        Some("--completion_script") => parse_completion_script(&user_args[1..], cli_arguments),
        Some("--check_manifest") => parse_manifest_check(&user_args[1..], false),
        Some("--quiet")
            if user_args
                .get(1)
                .is_some_and(|arg| os_eq(arg, "--check_manifest")) =>
        {
            parse_manifest_check(&user_args[2..], true)
        }
        Some("--quiet") => match user_args.get(1) {
            Some(argument) => Err(CliError::new(format!(
                "`--quiet` is currently supported only with `--check_manifest`, got `{}`",
                argument.to_string_lossy()
            ))),
            None => Err(CliError::new("`--quiet` needs `--check_manifest <PATH>`")),
        },
        Some("-f" | "--file" | "--manifest" | "--refl" | "--workers") => {
            parse_assembly(user_args, cli_arguments)
        }
        Some(option) if script_extension(option).is_some() => {
            parse_assembly(user_args, cli_arguments)
        }
        Some(option) => Err(CliError::new(format!("unknown option `{option}`"))),
        None if is_option_like(first) => Err(CliError::new(format!(
            "unknown option `{}`",
            first.to_string_lossy()
        ))),
        None => Ok(TopLevelCommand::ConfiguredCli(cli_arguments)),
    }
}

fn parse_completion_request(args: &[OsString]) -> Result<TopLevelCommand, CliError> {
    let Some(version) = args.first() else {
        return Err(CliError::new("`--completions` needs protocol version `v0`"));
    };
    if !os_eq(version, "v0") {
        return Err(CliError::new(format!(
            "unsupported completion protocol `{}`; expected `v0`",
            version.to_string_lossy()
        )));
    }
    let Some(mode) = args.get(1) else {
        return Err(CliError::new(
            "`--completions v0` needs mode `active` or `absent`",
        ));
    };
    let active = if os_eq(mode, "active") {
        true
    } else if os_eq(mode, "absent") {
        false
    } else {
        return Err(CliError::new(format!(
            "invalid completion mode `{}`; expected `active` or `absent`",
            mode.to_string_lossy()
        )));
    };
    let before_count = args
        .get(2)
        .ok_or_else(|| CliError::new("completion request is missing BEFORE_COUNT"))
        .and_then(|count| parse_completion_count(count, "BEFORE_COUNT"))?;
    let after_count = args
        .get(3)
        .ok_or_else(|| CliError::new("completion request is missing AFTER_COUNT"))
        .and_then(|count| parse_completion_count(count, "AFTER_COUNT"))?;
    let payload = args.get(4..).unwrap_or_default();
    let active_fields = if active { 2 } else { 0 };
    let expected = before_count
        .checked_add(active_fields)
        .and_then(|count| count.checked_add(after_count))
        .ok_or_else(|| CliError::new("completion request payload size is unsupported"))?;
    if payload.len() != expected {
        return Err(CliError::new(format!(
            "completion request declares {before_count} argument(s) before and {after_count} after but supplies {} payload argument(s); expected {expected}",
            payload.len()
        )));
    }
    let before = payload[..before_count].iter().cloned();
    let after_start = before_count + active_fields;
    let after = payload[after_start..].iter().cloned();
    let request = if active {
        CompletionRequest::with_active(
            before,
            payload[before_count].clone(),
            payload[before_count + 1].clone(),
            after,
        )
    } else {
        CompletionRequest::without_active(before, after)
    };
    Ok(TopLevelCommand::Complete(request))
}

fn parse_completion_count(value: &OsStr, label: &str) -> Result<usize, CliError> {
    let Some(value) = value.to_str() else {
        return Err(CliError::new(format!(
            "completion {label} must be a canonical non-negative integer"
        )));
    };
    if value.is_empty()
        || value.len() > 1 && value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(CliError::new(format!(
            "completion {label} must be a canonical non-negative integer, got `{value}`"
        )));
    }
    value.parse().map_err(|_| {
        CliError::new(format!(
            "completion {label} is too large for this implementation"
        ))
    })
}

fn parse_completion_script(
    args: &[OsString],
    cli_arguments: CliArguments,
) -> Result<TopLevelCommand, CliError> {
    let [name] = args else {
        return Err(CliError::new(
            "`--completion_script` accepts exactly one binding name",
        ));
    };
    Ok(TopLevelCommand::CompletionScript {
        name: name.clone(),
        cli_arguments,
    })
}

fn parse_configured_inspection(
    args: &[OsString],
    nul_terminated: bool,
) -> Result<TopLevelCommand, CliError> {
    let Some(first) = args.first() else {
        return Err(CliError::new(
            "configured CLI inspection needs at least one bare argument",
        ));
    };
    if is_option_like(first) {
        return Err(CliError::new(
            "configured CLI inspection requires a bare first argument",
        ));
    }
    Ok(TopLevelCommand::InspectConfiguredCli {
        arguments: CliArguments::new(Arc::from(args)),
        nul_terminated,
    })
}

pub fn parse_worker_count(value: &OsStr, source: &str) -> Result<usize, CliError> {
    let Some(value) = value.to_str() else {
        return Err(CliError::new(format!(
            "`{source}` requires a non-negative integer"
        )));
    };
    value.parse::<usize>().map_err(|_| {
        CliError::new(format!(
            "`{source}` requires a non-negative integer, got `{value}`"
        ))
    })
}

fn parse_inspection(args: &[OsString]) -> Result<TopLevelCommand, CliError> {
    let mut path = None;
    let mut verbosity = ParseVerbosity::Normal;
    for argument in args {
        match option(argument) {
            Some("-q" | "--quiet") if verbosity != ParseVerbosity::Verbose => {
                verbosity = ParseVerbosity::Quiet;
            }
            Some("-v" | "--verbose") if verbosity != ParseVerbosity::Quiet => {
                verbosity = ParseVerbosity::Verbose;
            }
            Some("-q" | "--quiet" | "-v" | "--verbose") => {
                return Err(CliError::new(
                    "`--quiet` and `--verbose` cannot be combined with `--parse`",
                ));
            }
            Some(option) => {
                return Err(CliError::new(format!(
                    "unknown `--parse` option `{option}`"
                )));
            }
            None if is_option_like(argument) => {
                return Err(CliError::new(format!(
                    "unknown `--parse` option `{}`",
                    argument.to_string_lossy()
                )));
            }
            None if path.is_some() => {
                return Err(CliError::new(
                    "`glam --parse` accepts exactly one source path",
                ));
            }
            None => path = Some(PathBuf::from(argument)),
        }
    }
    let Some(path) = path else {
        return Err(CliError::new("`glam --parse` needs a source path"));
    };
    Ok(TopLevelCommand::InspectGSource { path, verbosity })
}

fn parse_manifest_check(
    args: &[OsString],
    leading_quiet: bool,
) -> Result<TopLevelCommand, CliError> {
    let Some(manifest) = args.first() else {
        return Err(CliError::new("`--check_manifest` needs a manifest path"));
    };
    if is_option_like(manifest) {
        return Err(CliError::new(
            "`--check_manifest` needs a manifest path before any options",
        ));
    }
    let quiet = match args.get(1).and_then(|argument| option(argument)) {
        None if args.len() == 1 => leading_quiet,
        Some("--quiet") if !leading_quiet => true,
        Some("--quiet") if leading_quiet => {
            return Err(CliError::new("`--quiet` may be specified only once"));
        }
        Some(option) => {
            return Err(CliError::new(format!(
                "unknown `--check_manifest` option `{option}`"
            )));
        }
        None if args.get(1).is_some_and(|argument| is_option_like(argument)) => {
            return Err(CliError::new(format!(
                "unknown `--check_manifest` option `{}`",
                args[1].to_string_lossy()
            )));
        }
        None => {
            return Err(CliError::new(
                "`--check_manifest` accepts only a manifest path and optional `--quiet`",
            ));
        }
    };
    if args.len() > 2 {
        return Err(CliError::new(
            "`--check_manifest` accepts only a manifest path and optional `--quiet`",
        ));
    }
    Ok(TopLevelCommand::CheckManifest {
        path: PathBuf::from(manifest),
        quiet,
    })
}

fn parse_assembly(
    user_args: Arc<[OsString]>,
    cli_arguments: CliArguments,
) -> Result<TopLevelCommand, CliError> {
    let mut builder = CommandPlanBuilder::default();
    let mut index = 0;
    while let Some(argument) = user_args.get(index) {
        index += 1;
        match option(argument) {
            Some("--") => {
                for argument in &user_args[index..] {
                    builder.push(CommandEdit::AssemblyArgument(argument.clone()))?;
                }
                break;
            }
            Some("-f" | "--file") => {
                let Some(path) = user_args.get(index) else {
                    return Err(CliError::new(format!(
                        "`{}` needs a source path",
                        argument.to_string_lossy()
                    )));
                };
                index += 1;
                builder.push(CommandEdit::Input(ModuleInput::file(PathBuf::from(path))))?;
            }
            Some("--manifest") => {
                let Some(path) = user_args.get(index) else {
                    return Err(CliError::new("`--manifest` needs an output path"));
                };
                index += 1;
                builder.push(CommandEdit::Manifest(PathBuf::from(path)))?;
            }
            Some("--refl") => {
                let Some(value) = user_args.get(index) else {
                    return Err(CliError::new("`--refl` needs an argument"));
                };
                index += 1;
                builder.push(CommandEdit::ReflectionArgument(value.clone()))?;
            }
            Some("--workers") => {
                let Some(value) = user_args.get(index) else {
                    return Err(CliError::new("`--workers` needs a non-negative integer"));
                };
                index += 1;
                builder.push(CommandEdit::WorkerCount(parse_worker_count(
                    value,
                    "--workers",
                )?))?;
            }
            Some(option) if script_extension(option).is_some() => {
                let extension = script_extension(option).expect("checked above");
                let Some(body) = user_args.get(index) else {
                    return Err(CliError::new(format!("`{option}` needs a script body")));
                };
                index += 1;
                let Some(body) = body.to_str() else {
                    return Err(CliError::new(format!(
                        "`{option}` needs a UTF-8 script body"
                    )));
                };
                builder.push(CommandEdit::Input(ModuleInput::script(extension, body)))?;
            }
            Some(option) => return Err(CliError::new(format!("unknown option `{option}`"))),
            None if is_option_like(argument) => {
                return Err(CliError::new(format!(
                    "unknown option `{}`",
                    argument.to_string_lossy()
                )));
            }
            None => {
                return Err(CliError::new(
                    "bare command-line arguments are reserved for configured `conf.cli` rewriting",
                ));
            }
        }
    }

    Ok(TopLevelCommand::Assembly(builder.finish(cli_arguments)?))
}

fn option(argument: &OsStr) -> Option<&str> {
    is_option_like(argument)
        .then(|| argument.to_str())
        .flatten()
}

pub(super) fn is_option_like(argument: &OsStr) -> bool {
    argument.as_encoded_bytes().first() == Some(&b'-')
}

pub(super) fn os_eq(argument: &OsStr, expected: &str) -> bool {
    argument.as_encoded_bytes() == expected.as_bytes()
}

pub(super) fn script_extension(option: &str) -> Option<&str> {
    option
        .strip_prefix("--script.")
        .or_else(|| option.strip_prefix("-s."))
        .filter(|extension| !extension.is_empty())
}
