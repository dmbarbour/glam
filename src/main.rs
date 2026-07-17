use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use bytes::Bytes;
use glam::{Assembler, Builtin, Diagnostic, Error, ModuleInput, Severity, Value};

// Parse inspection intentionally remains on the front-end API while ordinary
// assembly uses only the embedding facade.
use glam::g_syntax::{DeclarationKind, ParsedSource, parse_source};

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
    let assembler = Assembler::default();
    let logger = DefaultLogger::new(assembler.clone());
    let assembler = assembler.with_diagnostic_callback(move |diagnostic| {
        logger.emit(&diagnostic);
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

struct DefaultLogger {
    evaluator: Assembler,
    working_directory: PathBuf,
}

impl DefaultLogger {
    const AUTO_INDENT: usize = 2;

    fn new(evaluator: Assembler) -> Self {
        Self {
            evaluator,
            working_directory: env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    fn emit(&self, diagnostic: &Diagnostic) {
        let terminal = TerminalContext::snapshot();
        let updates = self.viewer_updates(diagnostic, &terminal);
        let text = diagnostic
            .enrich_with(updates)
            .and_then(|message| self.evaluator.get(&message, "msg.text"))
            .and_then(|text| self.evaluator.to_binary(&text))
            .map(|text| String::from_utf8_lossy(&text).into_owned())
            .unwrap_or_else(|_| diagnostic.message().to_owned());
        let rendered = self.render(diagnostic, &text, terminal.color);

        let _ = io::stderr().lock().write_all(rendered.as_bytes());
    }

    fn viewer_updates(&self, diagnostic: &Diagnostic, terminal: &TerminalContext) -> Value {
        let mut viewer = vec![
            ("kind", Value::text("terminal")),
            (
                "columns",
                Value::integer(i64::try_from(terminal.columns).unwrap_or(i64::MAX)),
            ),
            ("color", Value::text(terminal.color.name())),
            ("auto_indent", Value::integer(Self::AUTO_INDENT as i64)),
        ];
        if let Some(term) = &terminal.term {
            viewer.push(("term", Value::text(term)));
        }
        if let Some(language) = &terminal.language {
            viewer.push(("lang", Value::text(language)));
        }
        if let Some(source) = diagnostic.source().and_then(|source| {
            let path = Path::new(source);
            path.is_absolute().then(|| self.display_source(path))
        }) {
            viewer.push(("source", Value::record([("file", Value::text(source))])));
        }
        Value::record([("viewer", Value::record(viewer))])
    }

    fn render(&self, diagnostic: &Diagnostic, text: &str, color: TerminalColor) -> String {
        let severity = diagnostic.severity().to_string();
        let severity = color.paint(diagnostic.severity(), &severity);
        let location = match (diagnostic.source(), diagnostic.line()) {
            (Some(source), Some(line)) => {
                format!("{}:{line}: ", self.display_source(Path::new(source)))
            }
            (Some(source), None) => format!("{}: ", self.display_source(Path::new(source))),
            (None, Some(line)) => format!("line {line}: "),
            (None, None) => String::new(),
        };
        let mut rendered = format!("{location}{severity}: ");
        let mut lines = text.split('\n');
        rendered.push_str(lines.next().unwrap_or_default());
        for line in lines {
            rendered.push('\n');
            if !line.is_empty() {
                rendered.push_str(&" ".repeat(Self::AUTO_INDENT));
                rendered.push_str(line);
            }
        }
        rendered.push('\n');
        rendered
    }

    fn display_source(&self, source: &Path) -> String {
        source
            .strip_prefix(&self.working_directory)
            .unwrap_or(source)
            .display()
            .to_string()
    }
}

struct TerminalContext {
    columns: usize,
    color: TerminalColor,
    term: Option<String>,
    language: Option<String>,
}

impl TerminalContext {
    fn snapshot() -> Self {
        let term = env::var("TERM").ok();
        let color = TerminalColor::detect(term.as_deref());
        Self {
            columns: env::var("COLUMNS")
                .ok()
                .and_then(|columns| columns.parse().ok())
                .filter(|columns| *columns > 0)
                .unwrap_or(80),
            color,
            term,
            language: ["LC_ALL", "LC_MESSAGES", "LANG"]
                .into_iter()
                .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty())),
        }
    }
}

#[derive(Clone, Copy)]
enum TerminalColor {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

impl TerminalColor {
    fn detect(term: Option<&str>) -> Self {
        if !io::stderr().is_terminal() || env::var_os("NO_COLOR").is_some() || term == Some("dumb")
        {
            return Self::None;
        }
        if env::var("COLORTERM").is_ok_and(|value| {
            value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
        }) {
            Self::TrueColor
        } else if term.is_some_and(|term| term.contains("256color")) {
            Self::Ansi256
        } else {
            Self::Ansi16
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Ansi16 => "ansi16",
            Self::Ansi256 => "ansi256",
            Self::TrueColor => "truecolor",
        }
    }

    fn paint(self, severity: Severity, text: &str) -> String {
        let code = match (self, severity) {
            (Self::None, _) => return text.to_owned(),
            (_, Severity::Info) => 36,
            (_, Severity::Warning) => 33,
            (_, Severity::Error) => 31,
        };
        format!("\x1b[{code}m{text}\x1b[0m")
    }
}

fn parse_path(path: &str) -> ExitCode {
    let parsed = match parse_source_path(path) {
        Ok(parsed) => parsed,
        Err(exit_code) => return exit_code,
    };

    print_parse_summary(path, &parsed)
}

fn parse_source_path(path: &str) -> Result<ParsedSource, ExitCode> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("error: could not read `{path}`: {error}");
            return Err(ExitCode::from(1));
        }
    };

    Ok(parse_source(&bytes))
}

fn print_parse_summary(path: &str, parsed: &ParsedSource) -> ExitCode {
    let has_errors = parsed
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic.severity, Severity::Error));

    let logger = DefaultLogger::new(Assembler::default());
    for diagnostic in &parsed.diagnostics {
        logger.emit(
            &Diagnostic::new(diagnostic.severity, diagnostic.message.clone())
                .with_source_location(path, diagnostic.line),
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
    const HELP: &str = "\
Usage: glam [(-f|--file) <PATH> | (-s|--script).<EXT> <TEXT>]...
       glam --parse <PATH>
       glam --help
       glam --version

Assembly inputs are applied as mixins; earlier inputs override later inputs.
Configuration is loaded from GLAM_CONF as an OS path-list, or from the user config/default fixture.
Bare arguments are reserved for configured `conf.cli` rewriting.
";

    print!("{HELP}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_logger_indents_nonempty_continuation_lines_by_two_spaces() {
        let logger = DefaultLogger {
            evaluator: Assembler::default(),
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::new(Severity::Warning, "first\nsecond\n\nfourth")
            .with_source_location("/work/src/test.g", 4);

        assert_eq!(
            logger.render(&diagnostic, diagnostic.message(), TerminalColor::None),
            "src/test.g:4: warning: first\n  second\n\n  fourth\n"
        );
    }

    #[test]
    fn terminal_viewer_context_is_an_independent_diagnostic_mixin() {
        let logger = DefaultLogger::new(Assembler::default());
        let diagnostic = Diagnostic::new(Severity::Info, "hello");
        let terminal = TerminalContext {
            columns: 100,
            color: TerminalColor::Ansi256,
            term: Some("xterm-256color".to_owned()),
            language: Some("en_US.UTF-8".to_owned()),
        };
        let enriched = diagnostic
            .enrich_with(logger.viewer_updates(&diagnostic, &terminal))
            .expect("terminal viewer metadata should mix into a diagnostic");

        assert_eq!(
            logger
                .evaluator
                .get(&enriched, "viewer.auto_indent")
                .expect("viewer should declare automatic indentation")
                .as_i64(),
            Some(2)
        );
        assert_eq!(
            logger
                .evaluator
                .get(&enriched, "viewer.term")
                .expect("viewer should declare its terminal")
                .as_binary(),
            Some(b"xterm-256color".as_slice())
        );
        assert!(
            logger
                .evaluator
                .get(diagnostic.emission(), "viewer")
                .is_err()
        );
    }
}
