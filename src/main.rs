use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;

use glam::diagnostic::Severity;
use glam::eval;
use glam::g_syntax::{DeclarationKind, SourceFile, lower_to_core};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(first) = args.next() else {
        print_help();
        return ExitCode::SUCCESS;
    };

    match first.as_str() {
        "-h" | "--help" => {
            print_help();
            ExitCode::SUCCESS
        }
        "-V" | "--version" => {
            println!("glam {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "--parse" => {
            let Some(path) = args.next() else {
                eprintln!("error: `glam --parse` needs a source path");
                return ExitCode::from(2);
            };
            parse_path(&path)
        }
        "-f" | "--file" => {
            let Some(path) = args.next() else {
                eprintln!("error: `{}` needs a source path", first);
                return ExitCode::from(2);
            };

            assemble_path(&path)
        }
        option if option.starts_with('-') => {
            eprintln!("error: unknown option `{option}`");
            ExitCode::from(2)
        }
        _arg => {
            eprintln!(
                "error: bare command-line arguments are reserved for configured `conf.cli` rewriting; use `--parse <PATH>` to inspect a source file"
            );
            ExitCode::from(2)
        }
    }
}

fn assemble_path(path: &str) -> ExitCode {
    let parsed = match parse_source_path(path) {
        Ok(parsed) => parsed,
        Err(exit_code) => return exit_code,
    };

    let lowered = lower_to_core(&parsed);

    for diagnostic in &lowered.diagnostics {
        eprintln!(
            "{path}:{}: {}: {}",
            diagnostic.line, diagnostic.severity, diagnostic.message
        );
    }

    if lowered
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return ExitCode::from(1);
    }

    let Some(term) = &lowered.term else {
        eprintln!("error: .g lowering did not produce a core term");
        return ExitCode::from(1);
    };

    let assembly = match eval::eval_term(term) {
        Ok(assembly) => assembly,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };

    let bytes = match assembly.result_bytes() {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };

    if let Err(err) = io::stdout().write_all(&bytes) {
        eprintln!("error: could not write stdout: {err}");
        return ExitCode::from(1);
    }

    ExitCode::SUCCESS
}

fn parse_path(path: &str) -> ExitCode {
    let parsed = match parse_source_path(path) {
        Ok(parsed) => parsed,
        Err(exit_code) => return exit_code,
    };

    print_parse_summary(path, &parsed)
}

fn parse_source_path(path: &str) -> Result<glam::g_syntax::ParsedSource, ExitCode> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("error: could not read `{path}`: {err}");
            return Err(ExitCode::from(1));
        }
    };

    let source = SourceFile::new(path, text);
    Ok(source.parse())
}

fn print_parse_summary(path: &str, parsed: &glam::g_syntax::ParsedSource) -> ExitCode {
    for diagnostic in &parsed.diagnostics {
        eprintln!(
            "{path}:{}: {}: {}",
            diagnostic.line, diagnostic.severity, diagnostic.message
        );
    }

    println!("{} declarations", parsed.declarations.len());
    for declaration in &parsed.declarations {
        println!(
            "{:>4}: {:<12} {}",
            declaration.line,
            declaration_label(&declaration.kind),
            declaration.text.lines().next().unwrap_or("")
        );
    }

    if parsed
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic.severity, Severity::Error))
    {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn declaration_label(kind: &DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Language(_) => "language",
        DeclarationKind::Import(_) => "import",
        DeclarationKind::Abstract(_) => "abstract",
        DeclarationKind::Unique(_) => "unique",
        DeclarationKind::Object => "object",
        DeclarationKind::Extend => "extend",
        DeclarationKind::Definition(_) => "definition",
        DeclarationKind::Unknown => "unknown",
    }
}

fn print_help() {
    println!(
        "Usage: glam (-f|--file) <PATH>\n       glam --parse <PATH>\n       glam --help\n       glam --version\n\nBare non-option arguments are reserved for future configured `conf.cli` rewriting."
    );
}
