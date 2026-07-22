use std::ffi::{OsStr, OsString};

use super::adapters::BUILTIN_COMPLETION_SCRIPTS;
use super::bootstrap::{is_option_like, os_eq};
use super::completion::{CliCompletion, CompletionCandidate, CompletionKind, CompletionRequest};
use super::path::{self, PathAccess, PathKind};

const ROOT_OPTIONS: &[&str] = &[
    "--check_manifest",
    "--completion_script",
    "--completions",
    "--file",
    "--help",
    "--manifest",
    "--parse",
    "--parse_cli",
    "--parse_cli.0",
    "--quiet",
    "--refl",
    "--script.",
    "--version",
    "--workers",
    "-V",
    "-f",
    "-h",
    "-s.",
];

const ASSEMBLY_OPTIONS: &[&str] = &[
    "--",
    "--file",
    "--manifest",
    "--refl",
    "--script.",
    "--workers",
    "-f",
    "-s.",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionRoute {
    Basic(CompletionRequest),
    Configured(CompletionRequest),
}

/// Applies the same first-argument ownership rule as ordinary CLI dispatch.
pub fn route_completion(request: CompletionRequest) -> CompletionRoute {
    if request
        .arguments_before()
        .first()
        .is_some_and(|first| os_eq(first, "--parse_cli") || os_eq(first, "--parse_cli.0"))
    {
        let active = request.active_argument();
        let rebased = match active {
            Some(active) => CompletionRequest::with_active(
                request.arguments_before()[1..].iter().cloned(),
                active.prefix().to_owned(),
                active.suffix().to_owned(),
                request.arguments_after().iter().cloned(),
            ),
            None => CompletionRequest::without_active(
                request.arguments_before()[1..].iter().cloned(),
                request.arguments_after().iter().cloned(),
            ),
        };
        return CompletionRoute::Configured(rebased);
    }

    let first = request
        .arguments_before()
        .first()
        .map(OsString::as_os_str)
        .or_else(|| request.active_argument().map(|active| active.prefix()));
    match first {
        None => CompletionRoute::Basic(request),
        Some(first) if is_option_like(first) => CompletionRoute::Basic(request),
        Some(_) => CompletionRoute::Configured(request),
    }
}

/// Completes the bootstrap-owned command grammar without loading configuration.
pub fn complete_basic(request: &CompletionRequest) -> CliCompletion {
    let active = request.active_argument();
    let prefix = active.map_or_else(|| OsStr::new(""), |active| active.prefix());
    let suffix = active.map_or_else(|| OsStr::new(""), |active| active.suffix());
    let mut candidates = match basic_context(request.arguments_before()) {
        BasicContext::Root => keyword_candidates(ROOT_OPTIONS, prefix, suffix),
        BasicContext::AssemblyOptions => keyword_candidates(ASSEMBLY_OPTIONS, prefix, suffix),
        BasicContext::Parse {
            path,
            quiet,
            verbose,
        } => {
            let mut candidates = Vec::new();
            if !quiet && !verbose {
                candidates.extend(keyword_candidates(
                    &["--quiet", "--verbose", "-q", "-v"],
                    prefix,
                    suffix,
                ));
            }
            if !path {
                candidates.extend(path_candidates(
                    prefix,
                    suffix,
                    PathKind::File,
                    PathAccess::Read,
                ));
            }
            candidates
        }
        BasicContext::ManifestCheck { path, quiet } => {
            if !path {
                path_candidates(prefix, suffix, PathKind::File, PathAccess::Read)
            } else if !quiet {
                keyword_candidates(&["--quiet"], prefix, suffix)
            } else {
                Vec::new()
            }
        }
        BasicContext::CompletionScriptName => {
            keyword_candidates(BUILTIN_COMPLETION_SCRIPTS, prefix, suffix)
        }
        BasicContext::Path(kind, access) => path_candidates(prefix, suffix, kind, access),
        BasicContext::Value | BasicContext::Done | BasicContext::Invalid => Vec::new(),
    };

    candidates.retain(|candidate| candidate_viable(request, candidate.replacement()));
    CliCompletion::new(candidates, Vec::new(), Vec::new())
}

#[derive(Clone, Copy)]
enum BasicContext {
    Root,
    AssemblyOptions,
    Parse {
        path: bool,
        quiet: bool,
        verbose: bool,
    },
    ManifestCheck {
        path: bool,
        quiet: bool,
    },
    CompletionScriptName,
    Path(PathKind, PathAccess),
    Value,
    Done,
    Invalid,
}

fn basic_context(arguments: &[OsString]) -> BasicContext {
    let Some(first) = arguments.first() else {
        return BasicContext::Root;
    };
    if os_eq(first, "--parse") {
        return parse_context(&arguments[1..]);
    }
    if os_eq(first, "--check_manifest") {
        return manifest_context(&arguments[1..], false);
    }
    if os_eq(first, "--quiet") {
        return if arguments.len() == 1 {
            BasicContext::ManifestCheck {
                path: false,
                quiet: true,
            }
        } else if os_eq(&arguments[1], "--check_manifest") {
            manifest_context(&arguments[2..], true)
        } else {
            BasicContext::Invalid
        };
    }
    if os_eq(first, "--completion_script") {
        return if arguments.len() == 1 {
            BasicContext::CompletionScriptName
        } else {
            BasicContext::Done
        };
    }
    if matches_option(first, &["--help", "-h", "--version", "-V"]) || os_eq(first, "--completions")
    {
        return BasicContext::Done;
    }
    assembly_context(arguments)
}

fn parse_context(arguments: &[OsString]) -> BasicContext {
    let mut path = false;
    let mut quiet = false;
    let mut verbose = false;
    for argument in arguments {
        if matches_option(argument, &["--quiet", "-q"]) && !quiet && !verbose {
            quiet = true;
        } else if matches_option(argument, &["--verbose", "-v"]) && !quiet && !verbose {
            verbose = true;
        } else if !is_option_like(argument) && !path {
            path = true;
        } else {
            return BasicContext::Invalid;
        }
    }
    BasicContext::Parse {
        path,
        quiet,
        verbose,
    }
}

fn manifest_context(arguments: &[OsString], leading_quiet: bool) -> BasicContext {
    match arguments {
        [] => BasicContext::ManifestCheck {
            path: false,
            quiet: leading_quiet,
        },
        [path] if !is_option_like(path) => BasicContext::ManifestCheck {
            path: true,
            quiet: leading_quiet,
        },
        [path, quiet] if !is_option_like(path) && os_eq(quiet, "--quiet") && !leading_quiet => {
            BasicContext::ManifestCheck {
                path: true,
                quiet: true,
            }
        }
        _ => BasicContext::Invalid,
    }
}

fn assembly_context(arguments: &[OsString]) -> BasicContext {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        index += 1;
        if os_eq(argument, "--") {
            return BasicContext::Done;
        }
        let operand = if matches_option(argument, &["--file", "-f"]) {
            Some(BasicContext::Path(PathKind::File, PathAccess::Read))
        } else if os_eq(argument, "--manifest") {
            Some(BasicContext::Path(PathKind::File, PathAccess::Write))
        } else if matches_option(argument, &["--refl", "--workers"])
            || script_extension(argument).is_some()
            || matches_option(argument, &["--script.", "-s."])
        {
            Some(BasicContext::Value)
        } else {
            return BasicContext::Invalid;
        };
        if index == arguments.len() {
            return operand.expect("assembly options above require operands");
        }
        index += 1;
    }
    BasicContext::AssemblyOptions
}

fn keyword_candidates(
    options: &[&str],
    prefix: &OsStr,
    suffix: &OsStr,
) -> Vec<CompletionCandidate> {
    let Some(prefix) = prefix.to_str() else {
        return Vec::new();
    };
    let Some(suffix) = suffix.to_str() else {
        return Vec::new();
    };
    options
        .iter()
        .filter(|option| option.starts_with(prefix) && option.ends_with(suffix))
        .map(|option| CompletionCandidate::new(*option, CompletionKind::Keyword))
        .collect()
}

fn path_candidates(
    prefix: &OsStr,
    suffix: &OsStr,
    kind: PathKind,
    access: PathAccess,
) -> Vec<CompletionCandidate> {
    path::completions(prefix, suffix, kind, access)
        .into_iter()
        .map(|(replacement, kind, _)| CompletionCandidate::new(replacement, kind))
        .collect()
}

fn candidate_viable(request: &CompletionRequest, replacement: &OsStr) -> bool {
    let mut arguments = request.arguments_before().to_vec();
    arguments.push(replacement.to_owned());
    arguments.extend(request.arguments_after().iter().cloned());
    partial_command_valid(&arguments)
}

fn partial_command_valid(arguments: &[OsString]) -> bool {
    let Some(first) = arguments.first() else {
        return true;
    };
    if matches_option(first, &["--help", "-h", "--version", "-V"]) {
        return arguments.len() == 1;
    }
    if os_eq(first, "--parse") {
        return !matches!(parse_context(&arguments[1..]), BasicContext::Invalid);
    }
    if os_eq(first, "--check_manifest") {
        return !matches!(
            manifest_context(&arguments[1..], false),
            BasicContext::Invalid
        );
    }
    if os_eq(first, "--quiet") {
        return arguments.len() == 1
            || arguments
                .get(1)
                .is_some_and(|arg| os_eq(arg, "--check_manifest"))
                && !matches!(
                    manifest_context(&arguments[2..], true),
                    BasicContext::Invalid
                );
    }
    if os_eq(first, "--parse_cli") || os_eq(first, "--parse_cli.0") {
        return arguments.len() == 1 || !is_option_like(&arguments[1]);
    }
    if os_eq(first, "--completion_script") {
        return arguments.len() <= 2;
    }
    if os_eq(first, "--completions") {
        return true;
    }
    if !is_option_like(first) {
        return true;
    }
    !matches!(assembly_context(arguments), BasicContext::Invalid)
}

fn matches_option(argument: &OsStr, accepted: &[&str]) -> bool {
    accepted.iter().any(|expected| os_eq(argument, expected))
}

fn script_extension(argument: &OsStr) -> Option<&str> {
    super::bootstrap::script_extension(argument.to_str()?)
}
