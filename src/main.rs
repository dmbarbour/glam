use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;

use glam::compiler::CompileContext;
use glam::core::{Expr as CoreExpr, Value};
use glam::diagnostic::Severity;
use glam::eval;
use glam::g_syntax::{DeclarationKind, ParsedSource, SourceFile, lower_to_core_with_context};

#[derive(Debug, Clone, PartialEq, Eq)]
enum AssemblyInput {
    File(String),
    Script { extension: String, body: String },
}

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

            let mut inputs = vec![AssemblyInput::File(path)];
            match collect_assembly_inputs(args, &mut inputs) {
                Ok(()) => assemble_inputs(&inputs),
                Err(exit_code) => exit_code,
            }
        }
        option if script_extension(option).is_some() => {
            let extension = script_extension(option).expect("checked above").to_owned();
            let Some(body) = args.next() else {
                eprintln!("error: `{option}` needs a script body");
                return ExitCode::from(2);
            };

            let mut inputs = vec![AssemblyInput::Script { extension, body }];
            match collect_assembly_inputs(args, &mut inputs) {
                Ok(()) => assemble_inputs(&inputs),
                Err(exit_code) => exit_code,
            }
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

fn collect_assembly_inputs(
    mut args: impl Iterator<Item = String>,
    inputs: &mut Vec<AssemblyInput>,
) -> Result<(), ExitCode> {
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-f" | "--file" => {
                let Some(path) = args.next() else {
                    eprintln!("error: `{arg}` needs a source path");
                    return Err(ExitCode::from(2));
                };
                inputs.push(AssemblyInput::File(path));
            }
            option if script_extension(option).is_some() => {
                let extension = script_extension(option).expect("checked above").to_owned();
                let Some(body) = args.next() else {
                    eprintln!("error: `{option}` needs a script body");
                    return Err(ExitCode::from(2));
                };
                inputs.push(AssemblyInput::Script { extension, body });
            }
            option if option.starts_with('-') => {
                eprintln!("error: unknown option `{option}`");
                return Err(ExitCode::from(2));
            }
            _arg => {
                eprintln!(
                    "error: bare command-line arguments are reserved for configured `conf.cli` rewriting"
                );
                return Err(ExitCode::from(2));
            }
        }
    }

    Ok(())
}

fn script_extension(option: &str) -> Option<&str> {
    option
        .strip_prefix("--script.")
        .or_else(|| option.strip_prefix("-s."))
        .filter(|extension| !extension.is_empty())
}

fn assemble_inputs(inputs: &[AssemblyInput]) -> ExitCode {
    // Note this is a temporary spike wiring: syntax lowering still happens
    // here until the front end compiler (and user-defined syntax) owns more
    // of the translation pipeline.
    //
    // In the general case, a user-defined syntax may support multiple file
    // extensions, and shall effectfully lower to core, via common API with
    // the built-in syntax.

    let assembly_context = CompileContext::from_module_path(["assembly"]);
    let final_defs = assembly_context.final_defs.clone();
    let mut definitions = assembly_context.prior_defs.clone();
    let mut had_errors = false;

    for input in inputs.iter().rev() {
        let (parsed, context, diagnostic_label) =
            match parse_assembly_input(input, definitions.clone(), final_defs.clone()) {
                Ok(parsed) => parsed,
                Err(exit_code) => return exit_code,
            };

        // TODO: move printing errors into CompileContext, so a common logger can be used
        let lowered = lower_to_core_with_context(&parsed, &context);

        for diagnostic in &lowered.diagnostics {
            eprintln!(
                "{diagnostic_label}:{}: {}: {}",
                diagnostic.line, diagnostic.severity, diagnostic.message
            );
        }

        had_errors |= lowered
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error);
        definitions = lowered.definitions;
    }

    if had_errors {
        return ExitCode::from(1);
    }

    let term = instantiate_module(&assembly_context, &definitions);
    let root = match eval::eval_value(&term) {
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

fn parse_assembly_input(
    input: &AssemblyInput,
    prior_defs: Value,
    final_defs: Value,
) -> Result<(ParsedSource, CompileContext, String), ExitCode> {
    match input {
        AssemblyInput::File(path) => {
            let source = read_source_path(path)?;
            let context = CompileContext::from_source_path(path)
                .with_module_path(["assembly"])
                .with_prior_defs(prior_defs)
                .with_final_defs(final_defs)
                .with_source_binary(source.text.as_bytes());
            let parsed = source.parse_with_context(&context);
            Ok((parsed, context, path.clone()))
        }
        AssemblyInput::Script { extension, body } => {
            let label = format!("<script.{extension}>");
            let source = SourceFile::new(&label, body);
            let context = CompileContext::from_module_path(["assembly"])
                .with_prior_defs(prior_defs)
                .with_final_defs(final_defs)
                .with_source_binary(source.text.as_bytes());
            let parsed = source.parse_with_context(&context);
            Ok((parsed, context, label))
        }
    }
}

fn instantiate_module(context: &CompileContext, definitions: &Value) -> Value {
    // Currently relying on default CompileContext to provide default fixpoint.
    let Value::Expr(thunk) = &context.final_defs else {
        panic!("CompileContext.final_defs must be a future expression");
    };
    let CoreExpr::Future(ivar) = &(*thunk.expr) else {
        panic!("CompileContext.final_defs must be a future expression");
    };
    ivar.set(definitions.clone())
        .expect("CompileContext.final_defs future must be unassigned");
    definitions.clone()
}

fn result_bytes(root: &glam::core::Value, path: &str) -> Result<Vec<u8>, String> {
    let value = value_at_path(root, path)?;
    value_bytes(&value, path)
}

fn value_bytes(value: &glam::core::Value, path: &str) -> Result<Vec<u8>, String> {
    match value {
        glam::core::Value::Binary(bytes) => Ok(bytes.to_vec()),
        glam::core::Value::List(list) => list_bytes(list).map_err(|err| format!("`{path}` {err}")),
        glam::core::Value::Expr(thunk) => {
            let value = eval::eval_value(&glam::core::Value::Expr(thunk.clone()))
                .map_err(|err| err.to_string())?;
            value_bytes(&value, path)
        }
        glam::core::Value::Atom(_)
        | glam::core::Value::Dict(_)
        | glam::core::Value::Number(_)
        | glam::core::Value::Closure(_)
        | glam::core::Value::Builtin(_) => Err(format!("`{path}` is not binary text data")),
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
                let item = eval::eval_value(item).map_err(|err| err.to_string())?;
                let glam::core::Value::Number(number) = item else {
                    return Err("must contain only integers and binary segments".to_owned());
                };

                let byte = number.to_u8_if_integer().ok_or_else(|| {
                    format!("contains number `{number}` that is not an in-range byte integer")
                })?;
                bytes.borrow_mut().push(byte);
            }
            Ok(())
        },
    )?;
    Ok(bytes.into_inner())
}

fn value_at_path(root: &glam::core::Value, path: &str) -> Result<glam::core::Value, String> {
    let mut current = root.clone();

    for part in path.split('.') {
        let current_value = eval::eval_value(&current).map_err(|err| err.to_string())?;
        let glam::core::Value::Dict(dict) = current_value else {
            return Err(format!("assembly did not define `{path}`"));
        };

        current = dict
            .get(&glam::core::Key::Atom(glam::core::Atom::from_key(
                &glam::core::Key::binary_from_text(part),
            )))
            .cloned()
            .ok_or_else(|| format!("assembly did not define `{path}`"))?;
    }

    Ok(current)
}

fn parse_path(path: &str) -> ExitCode {
    let (parsed, _context) = match parse_source_path(path) {
        Ok(parsed) => parsed,
        Err(exit_code) => return exit_code,
    };

    print_parse_summary(path, &parsed)
}

fn parse_source_path(path: &str) -> Result<(ParsedSource, CompileContext), ExitCode> {
    let source = read_source_path(path)?;
    let context =
        CompileContext::for_assembly_file(path).with_source_binary(source.text.as_bytes());
    Ok((source.parse_with_context(&context), context))
}

fn read_source_path(path: &str) -> Result<SourceFile, ExitCode> {
    // TODO: never convert to string, instead pass bytes to the parser and let it handle UTF-8 decoding.
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("error: could not read `{path}`: {err}");
            return Err(ExitCode::from(1));
        }
    };

    Ok(SourceFile::new(path, text))
}

fn print_parse_summary(path: &str, parsed: &glam::g_syntax::ParsedSource) -> ExitCode {
    let has_errors = parsed
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic.severity, Severity::Error));

    for diagnostic in &parsed.diagnostics {
        eprintln!(
            "{path}:{}: {}: {}",
            diagnostic.line, diagnostic.severity, diagnostic.message
        );
    }

    let out = &mut io::stderr();

    writeln!(out, "{} declarations", parsed.declarations.len())
        .expect("failed to write parse summary");
    for declaration in &parsed.declarations {
        writeln!(
            out,
            "{:>4}: {:<12} {}",
            declaration.line,
            declaration_label(&declaration.kind),
            declaration.text.lines().next().unwrap_or("")
        )
        .expect("failed to write parse summary");
    }

    if has_errors {
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
        "Usage: glam [(-f|--file) <PATH> | (-s|--script).<EXT> <TEXT>]...\n       glam --parse <PATH>\n       glam --help\n       glam --version\n\nAssembly inputs are applied as mixins; earlier inputs override later inputs.\nBare arguments are reserved for configured `conf.cli` rewriting."
    );
}
