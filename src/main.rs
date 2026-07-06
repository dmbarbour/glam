use std::env;
use std::fs;
use std::process::ExitCode;

use glam::source::{DeclarationKind, SourceFile};

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
        "parse" => {
            let Some(path) = args.next() else {
                eprintln!("error: `glam parse` needs a source path");
                return ExitCode::from(2);
            };
            parse_path(&path)
        }
        path => parse_path(path),
    }
}

fn parse_path(path: &str) -> ExitCode {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("error: could not read `{path}`: {err}");
            return ExitCode::from(1);
        }
    };

    let source = SourceFile::new(path, text);
    let parsed = source.parse();

    for diagnostic in &parsed.diagnostics {
        eprintln!(
            "{}:{}: {}: {}",
            source.path, diagnostic.line, diagnostic.severity, diagnostic.message
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
        .any(|diagnostic| matches!(diagnostic.severity, glam::source::Severity::Error))
    {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn declaration_label(kind: &DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Language(_) => "language",
        DeclarationKind::Import => "import",
        DeclarationKind::Abstract => "abstract",
        DeclarationKind::Unique => "unique",
        DeclarationKind::Object => "object",
        DeclarationKind::Extend => "extend",
        DeclarationKind::Definition(_) => "definition",
        DeclarationKind::Unknown => "unknown",
    }
}

fn print_help() {
    println!(
        "Usage: glam [parse] <PATH>\n       glam --help\n       glam --version\n\nParses an initial .g source surface and prints declaration diagnostics."
    );
}
