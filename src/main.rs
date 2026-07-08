use std::convert::TryFrom;
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
    // Note this is a temporary spike wiring: syntax lowering still happens
    // here until the front end compiler (and user-defined syntax) owns more
    // of the translation pipeline.
    //
    // In the general case, a user-defined syntax may support multiple file
    // extensions, and shall effectfully lower to core, via common API with
    // the built-in syntax.

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

    let root = match eval::eval_term(term) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };

    let bytes = match result_bytes(&root, "asm.result") {
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

fn result_bytes(root: &glam::core::Value, path: &str) -> Result<Vec<u8>, String> {
    let value =
        value_at_path(root, path).ok_or_else(|| format!("assembly did not define `{path}`"))?;
    value_bytes(value, root, path)
}

fn value_bytes(
    value: &glam::core::Value,
    root: &glam::core::Value,
    path: &str,
) -> Result<Vec<u8>, String> {
    match value {
        glam::core::Value::Binary(bytes) => Ok(bytes.to_vec()),
        glam::core::Value::List(list) => list_bytes(list).map_err(|err| format!("`{path}` {err}")),
        glam::core::Value::Expr(expr) => {
            let value = eval::eval_value(&glam::core::Value::Expr(expr.clone()), Some(root))
                .map_err(|err| err.to_string())?;
            value_bytes(&value, root, path)
        }
        glam::core::Value::Dict(_) | glam::core::Value::Number(_) => {
            Err(format!("`{path}` is not binary text data"))
        }
    }
}

fn list_bytes(list: &glam::core::List) -> Result<Vec<u8>, String> {
    let bytes = std::cell::RefCell::new(Vec::new());
    list.for_each_segment(
        &mut |segment| {
            bytes.borrow_mut().extend_from_slice(segment);
            Ok::<_, String>(())
        },
        &mut |segment| {
            for item in segment.iter() {
                let glam::core::Value::Number(number) = item else {
                    return Err("must contain only integers and binary segments".to_owned());
                };

                let byte = u8::try_from(*number)
                    .map_err(|_| format!("contains integer `{number}` outside the byte range"))?;
                bytes.borrow_mut().push(byte);
            }
            Ok(())
        },
    )?;
    Ok(bytes.into_inner())
}

fn value_at_path<'a>(root: &'a glam::core::Value, path: &str) -> Option<&'a glam::core::Value> {
    let path = path
        .split('.')
        .map(|part| glam::core::Atom::from_key(&glam::core::Key::binary_from_text(part)))
        .collect::<Vec<_>>();
    root.get_atom_path(&path)
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
        "Usage: glam (-f|--file) <PATH>\n       glam --parse <PATH>\n       glam --help\n       glam --version\n\nBare arguments to be rewritten by `conf.cli`."
    );
}
