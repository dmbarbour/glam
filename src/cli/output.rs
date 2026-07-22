use std::fmt::Write;
use std::path::Path;

use crate::{GSourceInspection, Severity};

use super::ParseVerbosity;

pub const HELP_TEXT: &str = "\
Usage: glam [(-f|--file) <PATH> | (-s|--script).<EXT> <TEXT>]...
            [--manifest <PATH>]
            [--refl <ARG>]...
            [--workers <N>]
       glam --parse <PATH> [--quiet|--verbose]
       glam --check_manifest <PATH> [--quiet]
       glam --help
       glam --version

Assembly inputs are applied as mixins; earlier inputs override later inputs.
--manifest records every local input path and its SHA-256 digest.
--check_manifest verifies every local file recorded by a manifest.
--quiet suppresses changed-file output from --check_manifest.
--parse inspects one built-in .g source without compiling or loading imports.
--parse --quiet reports only through its exit status; --verbose lists declarations.
--refl appends an argument visible only as reflection environment process.refl_args.
--workers sets the shared background evaluator thread count; zero disables sparks.
GLAM_WORKERS provides the default worker count when --workers is absent.
Configuration is loaded from GLAM_CONF as an OS path-list, or from the user config/default fixture.
Bare arguments are reserved for configured `conf.cli` rewriting.
";

pub fn format_parse_summary(
    path: &Path,
    parsed: &GSourceInspection,
    verbosity: ParseVerbosity,
) -> String {
    let mut output = String::new();
    if verbosity != ParseVerbosity::Quiet {
        for diagnostic in parsed.diagnostics() {
            writeln!(
                output,
                "{}:{}: {}: {}",
                path.display(),
                diagnostic.line(),
                severity_label(diagnostic.severity()),
                diagnostic.message()
            )
            .expect("writing to a String cannot fail");
        }
        writeln!(output, "{} declarations", parsed.declarations().len())
            .expect("writing to a String cannot fail");
    }
    if verbosity == ParseVerbosity::Verbose {
        for declaration in parsed.declarations() {
            writeln!(
                output,
                "{:>4}: {:<12} {}",
                declaration.line(),
                declaration.kind().as_str(),
                declaration.preview()
            )
            .expect("writing to a String cannot fail");
        }
    }
    output
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}
