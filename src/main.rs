use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use bytes::Bytes;
use glam::{Assembler, Builtin, Diagnostic, Error, ModuleInput, Severity, Value};

// Parse inspection intentionally remains on the front-end API while ordinary
// assembly uses only the embedding facade.
use glam::compiler::CompileContext;
use glam::g_syntax::{DeclarationKind, ParsedSource, SourceFile};

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

            let mut inputs = vec![ModuleInput::file(path)];
            let mut cli_args = Vec::new();
            match collect_assembly_inputs(args, &mut inputs, &mut cli_args) {
                Ok(()) => assemble_inputs(inputs, cli_args),
                Err(exit_code) => exit_code,
            }
        }
        option if script_extension(option).is_some() => {
            let extension = script_extension(option).expect("checked above").to_owned();
            let Some(body) = args.next() else {
                eprintln!("error: `{option}` needs a script body");
                return ExitCode::from(2);
            };

            let mut inputs = vec![ModuleInput::script(extension, body)];
            let mut cli_args = Vec::new();
            match collect_assembly_inputs(args, &mut inputs, &mut cli_args) {
                Ok(()) => assemble_inputs(inputs, cli_args),
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
    inputs: &mut Vec<ModuleInput>,
    cli_args: &mut Vec<String>,
) -> Result<(), ExitCode> {
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => {
                cli_args.extend(args);
                return Ok(());
            }
            "-f" | "--file" => {
                let Some(path) = args.next() else {
                    eprintln!("error: `{arg}` needs a source path");
                    return Err(ExitCode::from(2));
                };
                inputs.push(ModuleInput::file(path));
            }
            option if script_extension(option).is_some() => {
                let extension = script_extension(option).expect("checked above").to_owned();
                let Some(body) = args.next() else {
                    eprintln!("error: `{option}` needs a script body");
                    return Err(ExitCode::from(2));
                };
                inputs.push(ModuleInput::script(extension, body));
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

fn assemble_inputs(inputs: Vec<ModuleInput>, cli_args: Vec<String>) -> ExitCode {
    let assembler = Assembler::default().with_diagnostic_callback(|diagnostic| {
        print_diagnostic(&diagnostic);
    });
    let result = assemble(&assembler, inputs, cli_args);

    let bytes = match result {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(1);
        }
    };

    if let Err(error) = io::stdout().write_all(&bytes) {
        eprintln!("error: could not write stdout: {error}");
        return ExitCode::from(1);
    }

    ExitCode::SUCCESS
}

fn assemble(
    assembler: &Assembler,
    inputs: Vec<ModuleInput>,
    cli_args: Vec<String>,
) -> Result<Bytes, Error> {
    let environment = load_configuration(assembler)?;
    let arguments = Value::list(cli_args.into_iter().map(Value::text));
    let initial_definitions = Value::record([
        ("asm", Value::record([("args", arguments)])),
        ("env", environment),
    ]);
    let module = assembler
        .module(["assembly"])
        .initial_definitions(initial_definitions)
        .inputs(inputs)
        .build()?;
    assembler.binary_at(module.value(), "asm.result")
}

fn load_configuration(assembler: &Assembler) -> Result<Value, Error> {
    let default_environment = empty_environment_object();
    let initial_definitions = Value::record([("env", default_environment.clone())]);
    let module = assembler
        .module(["configuration"])
        .initial_definitions(initial_definitions)
        .inputs(configuration_paths().into_iter().map(ModuleInput::file))
        .build()?;

    match assembler.get(module.value(), "conf.env") {
        Ok(environment) if !environment.is_undefined() => assembler.evaluate(&environment),
        Ok(_) | Err(_) => Ok(default_environment),
    }
}

fn empty_environment_object() -> Value {
    let spec = Value::record([
        (
            "name",
            Value::abstract_global_path(["configuration", "env"]),
        ),
        ("deps", Value::list(std::iter::empty())),
        ("defs", Value::builtin(Builtin::ObjectDefaultDefs)),
    ]);
    Value::builtin_call(Builtin::ObjectInstance, [spec])
}

fn configuration_paths() -> Vec<PathBuf> {
    if let Some(paths) = configuration_paths_from_env("GLAM_CONF") {
        return paths;
    }

    if let Some(path) = default_user_configuration_path().filter(|path| path.exists()) {
        return vec![path];
    }

    Vec::new()
}

fn configuration_paths_from_env(name: &str) -> Option<Vec<PathBuf>> {
    env::var_os(name).map(|value| {
        env::split_paths(&value)
            .filter(|path| !path.as_os_str().is_empty())
            .collect()
    })
}

fn default_user_configuration_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join("glam").join("conf.g"))
    }

    #[cfg(target_os = "macos")]
    {
        home_dir().map(|path| {
            path.join("Library")
                .join("Application Support")
                .join("glam")
                .join("conf.g")
        })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".config")))
            .map(|path| path.join("glam").join("conf.g"))
    }

    #[cfg(not(any(unix, target_os = "windows")))]
    {
        None
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

fn print_diagnostic(diagnostic: &Diagnostic) {
    match (diagnostic.source(), diagnostic.line()) {
        (Some(source), Some(line)) => eprintln!(
            "{source}:{line}: {}: {}",
            diagnostic.severity(),
            diagnostic.message()
        ),
        _ => eprintln!("{}: {}", diagnostic.severity(), diagnostic.message()),
    }
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
    // TODO: let the front-end parse source bytes without this UTF-8 copy.
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("error: could not read `{path}`: {error}");
            return Err(ExitCode::from(1));
        }
    };

    Ok(SourceFile::new(path, text))
}

fn print_parse_summary(path: &str, parsed: &ParsedSource) -> ExitCode {
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
        DeclarationKind::Object(_) => "object",
        DeclarationKind::Extend(_) => "extend",
        DeclarationKind::Definition(_) => "definition",
        DeclarationKind::Unknown => "unknown",
    }
}

fn print_help() {
    println!(
        "Usage: glam [(-f|--file) <PATH> | (-s|--script).<EXT> <TEXT>]...\n       glam --parse <PATH>\n       glam --help\n       glam --version\n\nAssembly inputs are applied as mixins; earlier inputs override later inputs.\nConfiguration is loaded from GLAM_CONF as an OS path-list, or from the user config/default fixture.\nBare arguments are reserved for configured `conf.cli` rewriting."
    );
}
