use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use glam::compiler::{
    BinaryFileLoader, BinaryLoadArgs, CompileContext, ModuleLoadArgs, ModuleLoader,
};
use glam::core::{Builtin, Dict, Expr as CoreExpr, Key, Value};
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
            let mut cli_args = Vec::new();
            match collect_assembly_inputs(args, &mut inputs, &mut cli_args) {
                Ok(()) => assemble_inputs(&inputs, &cli_args),
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
            let mut cli_args = Vec::new();
            match collect_assembly_inputs(args, &mut inputs, &mut cli_args) {
                Ok(()) => assemble_inputs(&inputs, &cli_args),
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

fn assemble_inputs(inputs: &[AssemblyInput], cli_args: &[String]) -> ExitCode {
    // Note this is a temporary spike wiring: syntax lowering still happens
    // here until the front end compiler (and user-defined syntax) owns more
    // of the translation pipeline.
    //
    // In the general case, a user-defined syntax may support multiple file
    // extensions, and shall effectfully lower to core, via common API with
    // the built-in syntax.

    let module_loader = local_module_loader();
    let binary_loader = local_binary_loader();
    let configuration_env = match load_configuration(module_loader.clone(), binary_loader.clone()) {
        Ok(env) => env,
        Err(exit_code) => return exit_code,
    };
    let assembly_context = CompileContext::from_module_path(["assembly"])
        .with_local_module_loader(module_loader.clone())
        .with_local_binary_loader(binary_loader.clone());
    let final_defs = assembly_context.final_defs.clone();
    let mut definitions =
        initial_assembly_definitions(&assembly_context, cli_args, configuration_env);
    let mut had_errors = false;

    for input in inputs.iter().rev() {
        let (parsed, context, diagnostic_label) = match parse_assembly_input(
            input,
            definitions.clone(),
            final_defs.clone(),
            module_loader.clone(),
            binary_loader.clone(),
        ) {
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

fn initial_assembly_definitions(
    context: &CompileContext,
    cli_args: &[String],
    configuration_env: Value,
) -> Value {
    let args = context.value_list(
        cli_args
            .iter()
            .map(|arg| context.value_binary(arg))
            .collect(),
    );
    let asm = Value::Dict(Dict::new_sync().insert(Key::atom_from_text("args"), args));
    Value::Dict(
        Dict::new_sync()
            .insert(Key::atom_from_text("asm"), asm)
            .insert(Key::atom_from_text("env"), configuration_env),
    )
}

fn load_configuration(
    module_loader: ModuleLoader,
    binary_loader: BinaryFileLoader,
) -> Result<Value, ExitCode> {
    let configuration_context = CompileContext::from_module_path(["configuration"])
        .with_local_module_loader(module_loader.clone())
        .with_local_binary_loader(binary_loader.clone());
    let final_defs = configuration_context.final_defs.clone();
    let mut definitions = initial_configuration_definitions(&configuration_context);
    let mut had_errors = false;

    for path in configuration_paths().into_iter().rev() {
        let source = read_source_path_buf(&path)?;
        let diagnostic_label = source.path.clone();
        let context = CompileContext::from_source_path(&diagnostic_label)
            .with_module_path(["configuration"])
            .with_prior_defs(definitions.clone())
            .with_final_defs(final_defs.clone())
            .with_local_module_loader(module_loader.clone())
            .with_local_binary_loader(binary_loader.clone())
            .with_source_binary(source.text.as_bytes());
        let parsed = source.parse_with_context(&context);
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
        return Err(ExitCode::from(1));
    }

    let term = instantiate_module(&configuration_context, &definitions);
    let root = match eval::eval_value(&term) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("error: configuration evaluation failed: {err}");
            return Err(ExitCode::from(1));
        }
    };

    match value_at_path(&root, "conf.env") {
        Ok(env) if !is_undefined_value(&env) => eval::eval_value(&env).map_err(|err| {
            eprintln!("error: configuration `conf.env` failed to evaluate: {err}");
            ExitCode::from(1)
        }),
        Ok(_) | Err(_) => Ok(empty_environment_object(&configuration_context)),
    }
}

fn initial_configuration_definitions(context: &CompileContext) -> Value {
    let env = empty_environment_object(context);

    Value::Dict(Dict::new_sync().insert(Key::atom_from_text("env"), env))
}

fn is_undefined_value(value: &Value) -> bool {
    matches!(value, Value::Dict(dict) if dict.is_empty())
}

fn empty_environment_object(context: &CompileContext) -> Value {
    let spec = Dict::new_sync()
        .insert(
            Key::atom_from_text("name"),
            context.abstract_global_path_value(context.abstract_global_path("env").as_ref()),
        )
        .insert(Key::atom_from_text("deps"), context.value_list(Vec::new()))
        .insert(
            Key::atom_from_text("defs"),
            context.value_lambda(context.value_lambda(context.value_local(1))),
        );

    context.value_apply(
        context.value_builtin(Builtin::ObjectInstance),
        context.value_dict(spec),
    )
}

fn configuration_paths() -> Vec<PathBuf> {
    if let Some(paths) = configuration_paths_from_env("GLAS_CONF")
        .or_else(|| configuration_paths_from_env("GLAM_CONF"))
    {
        return paths;
    }

    if let Some(path) = default_user_configuration_path().filter(|path| path.exists()) {
        return vec![path];
    }

    let workspace_default = PathBuf::from("samples/config/dev.g");
    if workspace_default.exists() {
        return vec![workspace_default];
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
            .map(|path| path.join("glas").join("conf.g"))
    }

    #[cfg(target_os = "macos")]
    {
        home_dir().map(|path| {
            path.join("Library")
                .join("Application Support")
                .join("glas")
                .join("conf.g")
        })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".config")))
            .map(|path| path.join("glas").join("conf.g"))
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

fn parse_assembly_input(
    input: &AssemblyInput,
    prior_defs: Value,
    final_defs: Value,
    loader: ModuleLoader,
    binary_loader: BinaryFileLoader,
) -> Result<(ParsedSource, CompileContext, String), ExitCode> {
    match input {
        AssemblyInput::File(path) => {
            let source = read_source_path(path)?;
            let context = CompileContext::from_source_path(path)
                .with_module_path(["assembly"])
                .with_prior_defs(prior_defs)
                .with_final_defs(final_defs)
                .with_local_module_loader(loader)
                .with_local_binary_loader(binary_loader)
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
                .with_local_module_loader(loader)
                .with_local_binary_loader(binary_loader)
                .with_source_binary(source.text.as_bytes());
            let parsed = source.parse_with_context(&context);
            Ok((parsed, context, label))
        }
    }
}

fn local_module_loader() -> ModuleLoader {
    Arc::new(load_local_module)
}

fn local_binary_loader() -> BinaryFileLoader {
    Arc::new(load_local_binary)
}

fn load_local_module(args: ModuleLoadArgs) -> Result<Value, String> {
    let import_path = resolve_local_import_path(
        args.importer_source_path.as_deref(),
        &args.reference,
        "local import",
    )?;
    let path_label = import_path.display().to_string();

    let text = fs::read_to_string(&import_path)
        .map_err(|err| format!("could not read `{path_label}`: {err}"))?;
    let source = SourceFile::new(&path_label, text);
    let context = CompileContext::from_source_path(&path_label)
        .with_module_path(args.module_path.iter().cloned())
        .with_prior_defs(args.prior_defs)
        .with_final_defs(args.final_defs)
        .with_local_module_loader(local_module_loader())
        .with_local_binary_loader(local_binary_loader())
        .with_source_binary(source.text.as_bytes());
    let parsed = source.parse_with_context(&context);
    let lowered = lower_to_core_with_context(&parsed, &context);

    for diagnostic in &lowered.diagnostics {
        eprintln!(
            "{path_label}:{}: {}: {}",
            diagnostic.line, diagnostic.severity, diagnostic.message
        );
    }

    if lowered
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        Err(format!("local import `{path_label}` failed to compile"))
    } else {
        Ok(lowered.definitions)
    }
}

fn load_local_binary(args: BinaryLoadArgs) -> Result<Value, String> {
    let import_path = resolve_local_import_path(
        args.importer_source_path.as_deref(),
        &args.reference,
        "binary import",
    )?;
    let path_label = import_path.display().to_string();
    let bytes =
        fs::read(&import_path).map_err(|err| format!("could not read `{path_label}`: {err}"))?;
    Ok(Value::Binary(Arc::from(bytes.into_boxed_slice())))
}

fn resolve_local_import_path(
    importer_source_path: Option<&str>,
    reference: &str,
    kind: &str,
) -> Result<std::path::PathBuf, String> {
    let importer = importer_source_path.ok_or_else(|| {
        format!("{kind} `{reference}` cannot be loaded from a source without a file path")
    })?;
    let importer = Path::new(importer);
    let base = importer.parent().unwrap_or_else(|| Path::new("."));
    Ok(base.join(reference))
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

fn read_source_path_buf(path: &Path) -> Result<SourceFile, ExitCode> {
    let path_label = path.display().to_string();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("error: could not read `{path_label}`: {err}");
            return Err(ExitCode::from(1));
        }
    };

    Ok(SourceFile::new(&path_label, text))
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
        DeclarationKind::Object(_) => "object",
        DeclarationKind::Extend(_) => "extend",
        DeclarationKind::Definition(_) => "definition",
        DeclarationKind::Unknown => "unknown",
    }
}

fn print_help() {
    println!(
        "Usage: glam [(-f|--file) <PATH> | (-s|--script).<EXT> <TEXT>]...\n       glam --parse <PATH>\n       glam --help\n       glam --version\n\nAssembly inputs are applied as mixins; earlier inputs override later inputs.\nConfiguration is loaded from GLAS_CONF as an OS path-list, or from the user config/default fixture.\nBare arguments are reserved for configured `conf.cli` rewriting."
    );
}
